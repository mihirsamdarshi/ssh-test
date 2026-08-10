//! Local port forwarding (`ssh -L`) implemented with russh 0.62.
//!
//! Each accepted TCP connection gets its own `direct-tcpip` channel, and the
//! two are spliced together with [`tokio::io::copy_bidirectional`]. There is no
//! request parsing, no "stop reading on a short read" heuristic and no
//! EOF-after-the-first-response hack: bytes flow in both directions until one
//! side shuts down, which is what makes keep-alive, pipelining and large
//! transfers work.
//!
//! `client::Handle` methods take `&self` in russh 0.62, so the session is
//! shared through an `Arc` without a `Mutex` — opening a channel never blocks
//! data flowing on the other channels.

use std::{
    fmt::Debug,
    net::{IpAddr, SocketAddr},
    path::Path,
    sync::Arc,
};

use anyhow::{anyhow, Context, Result};
use common_port_forward::{expand_home_dir, get_args, setup_tracing};
use russh::{
    client::{self, Handle},
    keys::{load_secret_key, PrivateKeyWithHashAlg},
    Disconnect,
};
use tokio::{
    net::{TcpListener, TcpStream},
    select,
};
use tracing::{debug, error, info, instrument};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

mod scp;

struct Client;

impl client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        // Demo only: accept any host key. Production code must verify against
        // known_hosts / instance metadata.
        Ok(true)
    }
}

pub struct Session {
    session: Handle<Client>,
}

impl Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Session")
    }
}

impl Session {
    #[instrument]
    async fn connect<P: AsRef<Path> + Debug>(
        user: impl Into<String> + Debug,
        addr: SocketAddr,
        private_key_path: P,
    ) -> Result<Self> {
        let key_pair = load_secret_key(private_key_path, None).context("loading private key")?;
        let config = Arc::new(client::Config::default());
        let mut session = client::connect(config, addr, Client)
            .await
            .context("connecting to the SSH server")?;

        let auth_res = session
            .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key_pair), None))
            .await
            .context("authenticating")?;

        anyhow::ensure!(auth_res.success(), "public key authentication failed");

        Ok(Self { session })
    }

    #[instrument]
    async fn close(&self) -> Result<()> {
        self.session
            .disconnect(Disconnect::ByApplication, "", "en-US")
            .await?;
        Ok(())
    }
}

/// Splice one accepted TCP connection onto its own `direct-tcpip` channel.
#[instrument(skip(sess))]
async fn handle_conn(
    sess: Arc<Session>,
    mut stream: TcpStream,
    peer: SocketAddr,
    remote_port: u32,
) -> Result<()> {
    let channel = sess
        .session
        .channel_open_direct_tcpip(
            "localhost",
            remote_port,
            &peer.ip().to_string(),
            peer.port().into(),
        )
        .await
        .context("opening direct-tcpip channel")?;

    let mut channel_stream = channel.into_stream();

    let (to_server, to_client) = tokio::io::copy_bidirectional(&mut stream, &mut channel_stream)
        .await
        .context("forwarding data")?;

    debug!("connection closed: {to_server} bytes sent, {to_client} bytes received");
    Ok(())
}

#[instrument(skip(sess))]
async fn listen_on_forwarded_port(
    sess: Arc<Session>,
    local_port: u16,
    remote_port: u32,
) -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", local_port))
        .await
        .with_context(|| format!("binding 127.0.0.1:{local_port}"))?;
    info!("listening on 127.0.0.1:{local_port} -> localhost:{remote_port}");

    loop {
        let (stream, peer) = listener.accept().await.context("accepting connection")?;
        debug!("accepted connection from {peer}");

        let sess = Arc::clone(&sess);
        tokio::spawn(async move {
            if let Err(e) = handle_conn(sess, stream, peer, remote_port).await {
                error!("connection {peer}: {e:#}");
            }
        });
    }
}

/// Install a subscriber.
///
/// `common_port_forward::setup_tracing` spawns a `console-subscriber` (which
/// binds a fixed TCP port) and writes `trace.json` into the current directory,
/// so it is opt-in via `PORT_FORWARD_TRACE`; otherwise a plain stderr
/// subscriber is used (`RUST_LOG` still applies, defaulting to `info`).
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
async fn main() -> Result<()> {
    init_tracing();
    let args = get_args();

    let ssh = Arc::new(
        Session::connect(
            &args.user,
            SocketAddr::new(IpAddr::V4(args.ip), 22),
            expand_home_dir(&args.private_key_path).map_err(|e| anyhow!(e))?,
        )
        .await?,
    );

    let listener = tokio::spawn(listen_on_forwarded_port(
        Arc::clone(&ssh),
        args.local_port,
        u32::from(args.remote_port),
    ));

    let shutdown = tokio::spawn(async move {
        tokio::signal::ctrl_c().await.unwrap();
        info!("shutting down");
        if let Err(e) = ssh.close().await {
            error!("error closing session: {e:#}");
        }
    });

    select! {
        r = listener => r??,
        _ = shutdown => {},
    }

    Ok(())
}
