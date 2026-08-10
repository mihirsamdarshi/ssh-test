//! Local port forwarding (`ssh -L`) implemented on top of the synchronous
//! `ssh2` (libssh2) bindings.
//!
//! # Why this is a single-threaded event loop
//!
//! `ssh2::Session` is `Arc<Mutex<SessionInner>>`: *every* operation on *every*
//! channel derived from a session takes the same mutex. A thread parked in a
//! blocking `read` on one channel therefore holds the session lock and starves
//! every other channel (including the writer for the very same connection),
//! which deadlocks the classic "two blocking threads per connection" design.
//!
//! The design used here is the pattern libssh2 itself recommends:
//!
//! * the session is put in **non-blocking** mode after authentication, so every
//!   channel call returns [`std::io::ErrorKind::WouldBlock`] instead of parking,
//! * a **single** thread owns the session, the listener and every connection and
//!   pumps all of them in one loop, so the session lock is never contended and
//!   channel-open state can never be interleaved,
//! * each accepted TCP connection gets **its own** `channel_direct_tcpip`
//!   channel (libssh2 stream ids select stdout/stderr of one channel, they are
//!   *not* independent streams),
//! * when a full pass over every connection moves zero bytes the loop blocks in
//!   `poll(2)` on the SSH socket, the listener and the client sockets instead of
//!   spinning, so an idle (or network-bound) tunnel costs ~0% CPU.

use std::{
    collections::VecDeque,
    io::{ErrorKind, Read, Write},
    net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream},
    os::fd::{AsRawFd, RawFd},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use anyhow::anyhow;
use common_port_forward::{expand_home_dir, get_args, setup_tracing};
use ssh2::{BlockDirections, Channel, ErrorCode, Session};
use tracing::{debug, error, info, trace, warn};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

const LOCALHOST: IpAddr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
/// Per-direction, per-connection buffer size.
const BUFFER_SIZE: usize = 32 * 1024;
/// Upper bound on how long the loop blocks in `poll(2)` with nothing to do.
/// Also bounds how quickly Ctrl-C is noticed.
const POLL_TIMEOUT_MS: libc::c_int = 100;
/// How long a channel open may stay pending before the client is dropped.
const OPEN_TIMEOUT: Duration = Duration::from_secs(30);
/// How long we keep retrying a non-blocking `channel.close()` before giving up.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

/// Is this error libssh2's (or the socket's) "try again later"?
fn would_block(err: &std::io::Error) -> bool {
    err.kind() == ErrorKind::WouldBlock
}

/// `LIBSSH2_ERROR_EAGAIN`; not re-exported by the `ssh2` crate.
const LIBSSH2_ERROR_EAGAIN: libc::c_int = -37;

/// Same, for the `ssh2::Error` returned by the non-`io` APIs.
fn ssh_would_block(err: &ssh2::Error) -> bool {
    err.code() == ErrorCode::Session(LIBSSH2_ERROR_EAGAIN)
}

fn ssh_poll_events(session: &Session) -> libc::c_short {
    match session.block_directions() {
        BlockDirections::Inbound => libc::POLLIN,
        BlockDirections::Outbound => libc::POLLOUT,
        BlockDirections::Both => libc::POLLIN | libc::POLLOUT,
        BlockDirections::None => libc::POLLIN,
    }
}

/// A single-producer/single-consumer byte buffer for one direction of a
/// connection. Refilled only once fully drained, which keeps ordering trivial
/// and bounds memory to `BUFFER_SIZE` per direction.
struct Buffer {
    data: Box<[u8]>,
    start: usize,
    end: usize,
}

impl Buffer {
    fn new() -> Self {
        Self {
            data: vec![0u8; BUFFER_SIZE].into_boxed_slice(),
            start: 0,
            end: 0,
        }
    }

    fn is_empty(&self) -> bool {
        self.start == self.end
    }

    fn pending(&self) -> &[u8] {
        &self.data[self.start..self.end]
    }

    /// Writable region; only valid to fill while the buffer is empty.
    fn spare(&mut self) -> &mut [u8] {
        &mut self.data[..]
    }

    fn filled(&mut self, n: usize) {
        self.start = 0;
        self.end = n;
    }

    fn consume(&mut self, n: usize) {
        self.start += n;
        if self.start == self.end {
            self.start = 0;
            self.end = 0;
        }
    }
}

/// One forwarded TCP connection: a client socket paired with its own
/// `direct-tcpip` channel.
struct Connection {
    id: u64,
    stream: TcpStream,
    channel: Channel,
    /// Bytes read from the client, waiting to be written to the channel.
    to_remote: Buffer,
    /// Bytes read from the channel, waiting to be written to the client.
    to_local: Buffer,
    /// The client half-closed (read returned 0).
    local_eof: bool,
    /// `send_eof` has been acknowledged by libssh2.
    eof_sent: bool,
    /// The remote half-closed (channel read returned 0 at EOF).
    remote_eof: bool,
    /// We have shut down the write half of the client socket.
    local_shutdown: bool,
    /// Set once both directions are finished; we then retry `close()`.
    closing_since: Option<Instant>,
    /// Ready to be reaped by the event loop.
    finished: bool,
}

impl Connection {
    fn new(id: u64, stream: TcpStream, channel: Channel) -> Self {
        Self {
            id,
            stream,
            channel,
            to_remote: Buffer::new(),
            to_local: Buffer::new(),
            local_eof: false,
            eof_sent: false,
            remote_eof: false,
            local_shutdown: false,
            closing_since: None,
            finished: false,
        }
    }

    fn fail(&mut self, direction: &str, err: &std::io::Error) {
        debug!(
            "connection {}: {} failed ({}), tearing down",
            self.id, direction, err
        );
        let _ = self.stream.shutdown(Shutdown::Both);
        self.finished = true;
    }

    /// Move as much data as possible in both directions without blocking.
    ///
    /// Returns `true` if anything at all happened; the event loop only sleeps
    /// in `poll(2)` once every connection reports `false`, which guarantees we
    /// never park while libssh2 still has buffered data for us.
    fn pump(&mut self) -> bool {
        let mut progress = false;

        if !self.local_eof && self.to_remote.is_empty() {
            match self.stream.read(self.to_remote.spare()) {
                Ok(0) => {
                    trace!("connection {}: client sent EOF", self.id);
                    self.local_eof = true;
                    progress = true;
                }
                Ok(n) => {
                    self.to_remote.filled(n);
                    progress = true;
                }
                Err(ref e) if e.kind() == ErrorKind::Interrupted => {
                    progress = true;
                }
                Err(ref e) if would_block(e) => {}
                Err(e) => {
                    self.fail("client read", &e);
                    return true;
                }
            }
        }

        if !self.to_remote.is_empty() {
            match self.channel.write(self.to_remote.pending()) {
                Ok(0) => {
                    self.fail("channel write", &std::io::Error::from(ErrorKind::WriteZero));
                    return true;
                }
                Ok(n) => {
                    self.to_remote.consume(n);
                    progress = true;
                }
                Err(ref e) if e.kind() == ErrorKind::Interrupted => {
                    progress = true;
                }
                Err(ref e) if would_block(e) => {}
                Err(e) => {
                    self.fail("channel write", &e);
                    return true;
                }
            }
        }

        if self.local_eof && self.to_remote.is_empty() && !self.eof_sent {
            match self.channel.send_eof() {
                Ok(()) => {
                    self.eof_sent = true;
                    progress = true;
                }
                Err(ref e) if ssh_would_block(e) => {}
                Err(e) => {
                    self.fail("channel send_eof", &std::io::Error::from(e));
                    return true;
                }
            }
        }

        if !self.remote_eof && self.to_local.is_empty() {
            match self.channel.read(self.to_local.spare()) {
                // A zero-byte libssh2 read can mean no payload arrived, so
                // confirm the remote sent EOF before half-closing the client.
                Ok(0) => {
                    if self.channel.eof() {
                        trace!("connection {}: remote sent EOF", self.id);
                        self.remote_eof = true;
                        progress = true;
                    }
                }
                Ok(n) => {
                    self.to_local.filled(n);
                    progress = true;
                }
                Err(ref e) if e.kind() == ErrorKind::Interrupted => {
                    progress = true;
                }
                Err(ref e) if would_block(e) => {}
                Err(e) => {
                    self.fail("channel read", &e);
                    return true;
                }
            }
        }

        if !self.to_local.is_empty() {
            match self.stream.write(self.to_local.pending()) {
                Ok(0) => {
                    self.fail("client write", &std::io::Error::from(ErrorKind::WriteZero));
                    return true;
                }
                Ok(n) => {
                    self.to_local.consume(n);
                    progress = true;
                }
                Err(ref e) if e.kind() == ErrorKind::Interrupted => {
                    progress = true;
                }
                Err(ref e) if would_block(e) => {}
                Err(e) => {
                    self.fail("client write", &e);
                    return true;
                }
            }
        }

        if self.remote_eof && self.to_local.is_empty() && !self.local_shutdown {
            let _ = self.stream.shutdown(Shutdown::Write);
            self.local_shutdown = true;
            progress = true;
        }

        if self.eof_sent && self.local_shutdown {
            let started = *self.closing_since.get_or_insert_with(Instant::now);
            match self.channel.close() {
                Ok(()) => {
                    self.finished = true;
                    progress = true;
                }
                Err(ref e) if ssh_would_block(e) => {
                    if started.elapsed() > CLOSE_TIMEOUT {
                        warn!("connection {}: channel close timed out", self.id);
                        self.finished = true;
                        progress = true;
                    }
                }
                Err(e) => {
                    debug!("connection {}: channel close failed: {}", self.id, e);
                    self.finished = true;
                    progress = true;
                }
            }
        }

        progress
    }

    /// Poll flags this connection currently cares about on its client socket.
    fn poll_events(&self) -> libc::c_short {
        let mut events = 0;
        if !self.local_eof && self.to_remote.is_empty() {
            events |= libc::POLLIN;
        }
        if !self.to_local.is_empty() {
            events |= libc::POLLOUT;
        }
        events
    }
}

/// Accept loop + pump loop for the whole tunnel. Runs on the calling thread
/// until `should_exit` is set.
fn run_tunnel(
    session: &Session,
    ssh_fd: RawFd,
    should_exit: &AtomicBool,
    local_port: u16,
    remote_host: &str,
    remote_port: u16,
) -> anyhow::Result<()> {
    let listener = TcpListener::bind((LOCALHOST, local_port))?;
    listener.set_nonblocking(true)?;
    let listener_fd = listener.as_raw_fd();
    info!(
        "forwarding 127.0.0.1:{} -> {}:{} over ssh",
        local_port, remote_host, remote_port
    );

    // Every channel call must be non-blocking, otherwise a single stalled
    // connection would park the thread while holding the session mutex.
    session.set_blocking(false);

    let mut connections: Vec<Connection> = Vec::new();
    // libssh2 keeps the direct-tcpip handshake in *session*-global state, so
    // only one open may be in flight at a time; the rest queue up here and are
    // retried, front first, with identical arguments (as libssh2 requires).
    let mut pending: VecDeque<(TcpStream, Instant)> = VecDeque::new();
    let mut next_id: u64 = 0;
    let mut poll_fds: Vec<libc::pollfd> = Vec::new();

    while !should_exit.load(Ordering::SeqCst) {
        let mut progress = false;

        loop {
            match listener.accept() {
                Ok((stream, peer)) => {
                    trace!("accepted connection from {}", peer);
                    pending.push_back((stream, Instant::now()));
                    progress = true;
                }
                Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(ref e) if would_block(e) => break,
                Err(e) => {
                    error!("accept failed: {}", e);
                    break;
                }
            }
        }

        if let Some((_, queued_at)) = pending.front() {
            match session.channel_direct_tcpip(remote_host, remote_port, None) {
                Ok(channel) => {
                    let (stream, _) = pending.pop_front().expect("front exists");
                    if let Err(e) = stream.set_nonblocking(true) {
                        error!("failed to set client socket non-blocking: {}", e);
                    } else {
                        let _ = stream.set_nodelay(true);
                        let id = next_id;
                        next_id += 1;
                        debug!("connection {}: channel open", id);
                        connections.push(Connection::new(id, stream, channel));
                    }
                    progress = true;
                }
                Err(ref e) if ssh_would_block(e) => {
                    if queued_at.elapsed() > OPEN_TIMEOUT {
                        warn!("timed out opening channel for a pending connection");
                        pending.pop_front();
                        progress = true;
                    }
                }
                Err(e) => {
                    error!("failed to open direct-tcpip channel: {}", e);
                    pending.pop_front();
                    progress = true;
                }
            }
        }

        for connection in &mut connections {
            if connection.pump() {
                progress = true;
            }
        }
        connections.retain(|c| {
            if c.finished {
                debug!("connection {}: closed", c.id);
            }
            !c.finished
        });

        if progress {
            // Something moved, so libssh2 may still hold buffered data: do
            // another pass instead of parking in poll().
            continue;
        }

        poll_fds.clear();
        poll_fds.push(libc::pollfd {
            fd: listener_fd,
            events: libc::POLLIN,
            revents: 0,
        });
        poll_fds.push(libc::pollfd {
            fd: ssh_fd,
            events: ssh_poll_events(session),
            revents: 0,
        });
        for connection in &connections {
            let events = connection.poll_events();
            if events != 0 {
                poll_fds.push(libc::pollfd {
                    fd: connection.stream.as_raw_fd(),
                    events,
                    revents: 0,
                });
            }
        }

        let rc = unsafe {
            libc::poll(
                poll_fds.as_mut_ptr(),
                poll_fds.len() as libc::nfds_t,
                POLL_TIMEOUT_MS,
            )
        };
        if rc < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == ErrorKind::Interrupted {
                continue;
            }
            return Err(err.into());
        }
    }

    info!("tunnel stopped, {} connection(s) dropped", connections.len());
    Ok(())
}

/// Install a subscriber.
///
/// `common_port_forward::setup_tracing` spawns a `console-subscriber` (which
/// binds a fixed TCP port) and writes `trace.json` into the current directory,
/// and its debug-level default badly distorts throughput measurements, so it is
/// opt-in via `PORT_FORWARD_TRACE`; otherwise a plain stderr subscriber is used
/// (`RUST_LOG` still applies, defaulting to `info`).
fn init_tracing() {
    if std::env::var_os("PORT_FORWARD_TRACE").is_some() {
        setup_tracing();
        return;
    }

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(std::io::stderr))
        .init();
}

fn main() -> anyhow::Result<()> {
    init_tracing();
    let args = get_args();

    let exit_signal = Arc::new(AtomicBool::new(false));
    let ctrlc_flag = Arc::clone(&exit_signal);
    ctrlc::set_handler(move || {
        info!("received Ctrl-C, shutting down");
        ctrlc_flag.store(true, Ordering::SeqCst);
    })
    .expect("Error setting Ctrl-C handler");

    let tcp = TcpStream::connect(SocketAddr::new(IpAddr::V4(args.ip), 22))?;
    // The session owns the socket from here on, but the descriptor stays valid
    // for the lifetime of the session and is what we poll for readiness.
    let ssh_fd = tcp.as_raw_fd();

    let mut session = Session::new()?;
    session.set_tcp_stream(tcp);
    // Handshake and auth run in blocking mode; the tunnel switches the session
    // to non-blocking before pumping any data.
    session.handshake()?;
    session.userauth_pubkey_file(
        &args.user,
        None,
        &expand_home_dir(&args.private_key_path).map_err(|e| anyhow!(e))?,
        None,
    )?;
    if !session.authenticated() {
        return Err(anyhow!("failed to authenticate with public key"));
    }
    info!("authenticated as {}", args.user);
    session.set_keepalive(true, 30);

    run_tunnel(
        &session,
        ssh_fd,
        &exit_signal,
        args.local_port,
        // The literal address, not "localhost": sshd resolves the forward
        // target itself and "localhost" yields ::1 first on hosts with an IPv6
        // loopback, so opening the channel intermittently fails with
        // "Channel open failure (connect failed)" against a server bound only
        // to 127.0.0.1.
        "127.0.0.1",
        args.remote_port,
    )?;

    session.set_blocking(true);
    let _ = session.disconnect(None, "tunnel closed", None);

    Ok(())
}
