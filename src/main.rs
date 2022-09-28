use std::{
    fmt::Debug,
    fs::OpenOptions,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
};

use anyhow::Result;
use clap::Parser;
use lazy_static::lazy_static;
use russh::{client, client::Msg, Channel, ChannelMsg, Disconnect};
use russh_keys::{key::PublicKey, load_secret_key};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    select,
    sync::Mutex,
};
use tracing::{debug, debug_span, error, instrument, Instrument};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};
use uuid::Uuid;

mod scp;

struct Client {}

impl client::Handler for Client {
    type Error = russh::Error;
    type FutureBool = futures::future::Ready<Result<(Self, bool), Self::Error>>;
    type FutureUnit = futures::future::Ready<Result<(Self, client::Session), Self::Error>>;

    fn finished_bool(self, b: bool) -> Self::FutureBool {
        futures::future::ready(Ok((self, b)))
    }
    fn finished(self, session: client::Session) -> Self::FutureUnit {
        futures::future::ready(Ok((self, session)))
    }
    fn check_server_key(self, _server_public_key: &PublicKey) -> Self::FutureBool {
        self.finished_bool(true)
    }
}

pub struct Session {
    session: client::Handle<Client>,
}

impl Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Session")
    }
}

const BUFFER_SIZE: usize = 16_384;

#[instrument(skip(reader_buf))]
fn read_buf_bytes(
    full_req_len: &mut usize,
    full_req_buf: &mut Vec<u8>,
    reader_buf_len: usize,
    mut reader_buf: Vec<u8>,
) -> bool {
    if reader_buf_len == 0 {
        false
    } else {
        *full_req_len += reader_buf_len;
        // we need not read more data in case we have read less data than buffer size
        if reader_buf_len < BUFFER_SIZE {
            // let us only append the data how much we have read rather than complete
            // existing buffer data as n is less than buffer size
            full_req_buf.append(&mut reader_buf[..reader_buf_len].to_vec()); // convert slice into vec
            false
        } else {
            // append complete buffer vec data into request_buffer vec as n == buffer_size
            full_req_buf.append(&mut reader_buf);
            true
        }
    }
}

#[instrument]
async fn read_stream<R: AsyncReadExt + Debug + Unpin>(mut stream: R) -> (Vec<u8>, usize) {
    let mut request_buffer = vec![];
    // let us loop & try to read the whole request data
    let mut request_len = 0usize;
    loop {
        let mut buffer = vec![0; BUFFER_SIZE];
        // read the stream into the buffer, while the response length is not 0
        match stream.read(&mut buffer).await {
            Ok(n) => {
                if !read_buf_bytes(&mut request_len, &mut request_buffer, n, buffer) {
                    break;
                }
            }
            Err(e) => {
                eprintln!("Error reading stream: {}", e);
                break;
            }
        }
    }

    (request_buffer, request_len)
}

impl Session {
    #[instrument]
    async fn connect(user: impl Into<String> + Debug, addr: SocketAddr) -> Result<Self> {
        let home_dir = &*HOME_DIR;
        let key_pair = load_secret_key(
            format!("{}/.ssh/id_ed25519", home_dir.trim_end_matches('/')),
            None,
        )?;
        let config = Arc::new(client::Config::default());
        let sh = Client {};
        let mut session = client::connect(config, addr, sh).await?;
        let auth_res = session
            .authenticate_publickey(user, Arc::new(key_pair))
            .await
            .unwrap();
        dbg!(auth_res);
        Ok(Self { session })
    }

    #[instrument]
    async fn close(&mut self) -> Result<()> {
        self.session
            .disconnect(Disconnect::ByApplication, "", "en-US")
            .await?;
        Ok(())
    }
}

#[allow(unused_variables)]
#[instrument(skip(channel))]
async fn handle_req(mut channel: Channel<Msg>, mut stream: TcpStream, unique_id: String) {
    debug!("Splitting stream");
    let (mut read_half, mut write_half) = stream.split();
    debug!("Reading stream");
    let (request_buffer, request_len) = read_stream(&mut read_half).in_current_span().await;
    debug!("Request buffer: {:?}", std::str::from_utf8(&request_buffer));
    debug!("request_len: {}", request_len);
    if let Err(e) = channel
        .data(&request_buffer[..request_len])
        .in_current_span()
        .await
    {
        error!("Error in forwarding request to server: {:?}", e);
    };

    debug!("Waiting for response");
    let mut total_len = 0usize;
    while let Some(msg) = channel.wait().in_current_span().await {
        debug!("Received response from server = {:?}", &msg);
        match msg {
            ChannelMsg::Data { ref data } => {
                debug!("Writing response to client");
                let mut b = Vec::<u8>::new();
                data.write_all_from(0, &mut b).unwrap();
                match write_half.write_all(&b).in_current_span().await {
                    Ok(_) => {
                        total_len += b.len();
                    }
                    Err(e) => {
                        error!("Error in writing response to client: {:?}", e);
                    }
                };
                debug!("Response written to client");
            }
            ChannelMsg::Eof | ChannelMsg::Close => {
                debug!("End of data to be received");
                break;
            }
            _ => error!("Unknown message: {:?}", msg),
        }
    }
    debug!("Total response len: {}", total_len);
    debug!("Closing channel");
}

#[instrument]
async fn listen_on_forwarded_port(
    sess: Arc<Mutex<Session>>,
    local_port: u32,
    remote_port: u32,
) -> Result<()> {
    debug!("listening on forwarded port");
    let user_facing_socket = TcpListener::bind(format!("127.0.0.1:{}", local_port))
        .in_current_span()
        .await
        .unwrap();

    loop {
        let unique_id = Uuid::new_v4().to_string();
        let span = debug_span!("handle_req", unique_id = unique_id);
        let _enter = span.enter();
        let (stream, a) = user_facing_socket.accept().await.unwrap();
        debug!("Accepted connection from {:?}", a);

        let channel = {
            let mut session_guard = sess.lock().await;
            session_guard
                .session
                .channel_open_direct_tcpip(
                    "localhost",
                    remote_port,
                    &a.ip().to_string(),
                    a.port().into(),
                )
                .in_current_span()
                .await
                .unwrap()
        };
        tokio::spawn(handle_req(channel, stream, unique_id).in_current_span());
    }
}

struct Wrapper(Arc<Mutex<Session>>);

lazy_static! {
    static ref HOME_DIR: String = std::env::var("HOME").unwrap();
}

/// Simple program to forward a local port to a remote port on a remote host.
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Arguments {
    /// The username to connect as on the remote host (e.g. root).
    #[clap(short, long, value_parser)]
    user: String,
    /// The IPV4 address of the remote host (e.g. 80.69.420.85).
    #[clap(short, long, value_parser)]
    ip: Ipv4Addr,
    /// The port on the remote host to connect to (e.g. 8000).
    #[clap(short, long, value_parser)]
    remote_port: u32,
    /// The local port to listen on (e.g 9876).
    #[clap(short, long, value_parser)]
    local_port: u32,
}

#[instrument]
#[tokio::main]
async fn main() -> Result<()> {
    let args = Arguments::parse();

    let fmt_layer = fmt::layer()
        .pretty()
        .with_target(true)
        .with_level(true) // don't include levels in formatted output
        .with_thread_ids(true); // include the thread ID of the current thread

    let (non_blocking, _guard) = tracing_appender::non_blocking(
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open("trace.json")
            .unwrap(),
    );

    let json_layer = fmt::layer()
        .json()
        .with_current_span(true)
        .with_span_list(true)
        .with_target(true)
        .with_level(true) // don't include levels in formatted output
        .with_thread_ids(true) // include the thread ID of the current thread
        .with_thread_names(true)
        .with_writer(non_blocking); // include the name of the current thread

    let filter_layer = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("debug"))
        .unwrap();
    let console_layer = console_subscriber::spawn();

    tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt_layer)
        .with(console_layer)
        .with(json_layer)
        .init();

    let ssh = Session::connect(&args.user, SocketAddr::new(IpAddr::V4(args.ip), 22)).await?;

    let e = Arc::new(Mutex::new(ssh));
    let cloned_e = Arc::clone(&e);

    let t1 = tokio::spawn(listen_on_forwarded_port(
        cloned_e,
        args.local_port,
        args.remote_port,
    ));
    let w = Wrapper(e);

    let t2 = tokio::spawn(async move {
        tokio::signal::ctrl_c().await.unwrap();
        {
            let mut session_guard = w.0.lock().await;
            session_guard.close().await.unwrap();
        }
    });

    select! {
        _ = t1 => {},
        _ = t2 => {},
    }

    Ok(())
}
