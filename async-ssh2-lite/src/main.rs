//! Local port forwarding (`ssh -L`) built on `async-ssh2-lite` (libssh2).
//!
//! Each accepted TCP connection is spliced byte-for-byte onto a
//! `direct-tcpip` SSH channel in *both* directions concurrently. There is no
//! request parsing, no "a short read means the request ended" heuristic and no
//! EOF-after-the-first-response hack, so HTTP keep-alive, pipelining, request
//! bodies larger than one read and arbitrarily large responses all work.
//!
//! Caveat inherent to libssh2: every channel shares one session lock, so many
//! concurrent transfers are serialized at the transport layer. That is a
//! throughput limit, not a correctness one.

use std::{
    io::Error,
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    path::Path,
    sync::Arc,
};

use async_ssh2_lite::AsyncSession;
use common_port_forward::{expand_home_dir, get_args, setup_tracing};
use tokio::{
    io::{copy, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    select,
};
use tracing::{debug, error, instrument, Instrument};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};
use uuid::Uuid;

#[derive(Debug)]
struct SSHKeyPair<'a> {
    public_key: Option<&'a Path>,
    private_key: Option<&'a Path>,
}

fn make_socket_address<A: ToSocketAddrs>(address: A) -> SocketAddr {
    address.to_socket_addrs().unwrap().next().unwrap()
}

/// Splice one accepted local connection onto a fresh `direct-tcpip` channel.
///
/// Both directions run concurrently on the same task (`try_join!`), which keeps
/// all libssh2 calls for this connection on one thread while still allowing
/// full-duplex traffic. When the local side reaches EOF we send channel EOF so
/// the remote peer sees the half-close; when the remote side reaches EOF we
/// shut down the write half of the local socket.
#[instrument(skip(session, stream), err)]
async fn handle_req(
    remote_port: u16,
    session: Arc<AsyncSession<TcpStream>>,
    stream: TcpStream,
    unique_id: String,
) -> std::io::Result<()> {
    // Connect to the literal loopback address rather than "localhost". sshd
    // resolves this name itself, and `localhost` resolves to ::1 before
    // 127.0.0.1 on many hosts, so a target bound only to IPv4 is reached only
    // via sshd's fallback from the refused ::1 attempt. Naming the address we
    // actually mean removes that dependency. (Hardening, not a fix for any
    // observed failure -- sshd's fallback does work here.)
    let mut channel = session
        .channel_direct_tcpip("127.0.0.1", remote_port, None)
        .await
        .map_err(|e| Error::other(format!("channel_direct_tcpip: {e}")))?;

    // `AsyncChannel::stream(0)` hands out an independent reader for the same
    // channel, so the two copy futures below never need `&mut` at the same time.
    let mut channel_reader = channel.stream(0);
    let (mut local_reader, mut local_writer) = stream.into_split();

    let local_to_remote = async {
        let n = copy(&mut local_reader, &mut channel).await?;
        // Propagate the half-close upstream; ignore failures caused by the
        // channel already being torn down by the peer.
        if let Err(e) = channel.send_eof().await {
            debug!("send_eof after {n} bytes: {e}");
        }
        Ok::<u64, Error>(n)
    };

    let remote_to_local = async {
        let n = copy(&mut channel_reader, &mut local_writer).await?;
        let _ = local_writer.shutdown().await;
        Ok::<u64, Error>(n)
    };

    let (up, down) = tokio::try_join!(local_to_remote, remote_to_local)?;
    debug!("forwarded {up} bytes up, {down} bytes down");

    let _ = channel.close().await;
    Ok(())
}

#[instrument]
async fn create_ssh_session(
    username: &str,
    remote_address: SocketAddr,
    key_pair: SSHKeyPair<'_>,
) -> Result<AsyncSession<TcpStream>, Error> {
    let stream = TcpStream::connect(remote_address).await?;
    let mut session = AsyncSession::new(stream, None)?;
    session.handshake().await?;
    session
        .userauth_pubkey_file(
            username,
            key_pair.public_key,
            key_pair.private_key.unwrap(),
            None,
        )
        .await?;

    if session.authenticated() {
        Ok(session)
    } else {
        Err(session.last_error().map_or_else(
            || Error::other("unknown user auth error"),
            Error::from,
        ))
    }
}

#[instrument(skip(ssh_session))]
async fn local_port_forward(
    local_listener: TcpListener,
    remote_port: u16,
    ssh_session: AsyncSession<TcpStream>,
) -> std::io::Result<()> {
    let ssh_session = Arc::from(ssh_session);

    loop {
        let (stream, peer) = local_listener.accept().await?;
        let _ = stream.set_nodelay(true);

        let unique_id = Uuid::new_v4().to_string();
        let cloned_session = Arc::clone(&ssh_session);
        let span = tracing::debug_span!("handle_req", unique_id = %unique_id, peer = %peer);

        tokio::spawn(
            async move {
                if let Err(e) = handle_req(remote_port, cloned_session, stream, unique_id).await {
                    error!("connection from {peer} failed: {e}");
                }
            }
            .instrument(span),
        );
    }
}

/// Install a subscriber.
///
/// `common_port_forward::setup_tracing` spawns a `console-subscriber` (which
/// binds a fixed TCP port, so two of these binaries cannot run at once) and
/// writes `trace.json` into the current directory. Its `debug` default also
/// badly distorts throughput measurements. So it is opt-in via
/// `PORT_FORWARD_TRACE`; otherwise a plain stderr subscriber is used
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

#[tokio::main]
async fn main() -> std::io::Result<()> {
    init_tracing();
    let args = get_args();

    let remote_address = SocketAddr::new(IpAddr::V4(args.ip), 22);

    let private_key = Some(expand_home_dir(&args.private_key_path).unwrap());
    let public_key = args
        .public_key_path
        .as_ref()
        .map(expand_home_dir)
        .transpose()
        .unwrap();

    let key_pair = SSHKeyPair {
        public_key: public_key.as_ref().map(AsRef::as_ref),
        private_key: private_key.as_ref().map(AsRef::as_ref),
    };

    let session = create_ssh_session(&args.user, remote_address, key_pair).await?;

    let local_address = make_socket_address(("127.0.0.1", args.local_port));
    let local_listener = TcpListener::bind(local_address).await.map_err(|e| {
        error!("error binding to local port {}: {e}", args.local_port);
        e
    })?;

    debug!("listening on {}", local_listener.local_addr()?);

    select! {
        res = local_port_forward(local_listener, args.remote_port, session) => res?,
        res = tokio::signal::ctrl_c() => {
            res?;
            debug!("ctrl-c received, shutting down");
        }
    }

    Ok(())
}
