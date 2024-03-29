use std::{
    fmt::Debug,
    net::{IpAddr, SocketAddr},
    path::Path,
    sync::Arc,
};

use anyhow::{anyhow, Result};
use common_port_forward::{expand_home_dir, get_args, read_buf_bytes, setup_tracing};
use russh::{client, client::Msg, Channel, ChannelMsg, Disconnect};
use russh_keys::load_secret_key;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    select,
    sync::Mutex,
};
use tracing::{debug, debug_span, error, instrument, Instrument};
use uuid::Uuid;

mod scp;

const BUFFER_SIZE: usize = 16_384;

struct Client {}

impl client::Handler for Client {
    type Error = russh::Error;
}

pub struct Session {
    session: client::Handle<Client>,
}

impl Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Session")
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
                eprintln!("Error reading stream: {e}");
                break;
            }
        }
    }

    (request_buffer, request_len)
}

impl Session {
    #[instrument]
    async fn connect<P: AsRef<Path> + Debug>(
        user: impl Into<String> + Debug,
        addr: SocketAddr,
        private_key_path: P,
    ) -> Result<Self> {
        let key_pair = load_secret_key(private_key_path, None)?;
        let config = Arc::new(client::Config::default());
        let sh = Client {};
        let mut session = client::connect(config, addr, sh).await?;
        let auth_res = session
            .authenticate_publickey(user, Arc::new(key_pair))
            .await
            .unwrap();

        if !auth_res {
            eprintln!("Authentication failed");
            std::process::exit(1);
        }

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

#[instrument(skip(channel))]
async fn handle_req(mut channel: Channel<Msg>, mut incoming_stream: TcpStream, unique_id: String) {
    debug!("Splitting stream");
    let (mut read_half, mut write_half) = incoming_stream.split();

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

    // debug!("Sending EOF to server");
    // if let Err(e) = channel.eof().in_current_span().await {
    //     error!("Error in sending EOF to server: {:?}", e);
    // }

    let mut received_response = false;

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

                if !received_response {
                    received_response = true;
                    debug!("Sending EOF to server");
                    if let Err(e) = channel.eof().in_current_span().await {
                        error!("Error in sending EOF to server: {:?}", e);
                    }
                }

                debug!("Response written to client");
            }
            ChannelMsg::Eof => {
                debug!("Received EOF from server");
                break;
            }
            ChannelMsg::Close => {
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
    let user_facing_socket = TcpListener::bind(format!("127.0.0.1:{local_port}"))
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
            let session_guard = sess.lock().await;
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

#[instrument]
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    setup_tracing();
    let args = get_args();

    let ssh = Session::connect(
        &args.user,
        SocketAddr::new(IpAddr::V4(args.ip), 22),
        expand_home_dir(&args.private_key_path).map_err(|e| anyhow!(e))?,
    )
    .await?;

    let e = Arc::new(Mutex::new(ssh));
    let cloned_e = Arc::clone(&e);

    let t1 = tokio::spawn(listen_on_forwarded_port(
        cloned_e,
        u32::from(args.local_port),
        u32::from(args.remote_port),
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
