use std::{
    fmt::Debug,
    io::{Error, ErrorKind},
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    path::Path,
    str,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

use anyhow::Result;
use async_ssh2_lite::AsyncSession;
use common_port_forward::{expand_home_dir, get_args, read_buf_bytes, setup_tracing};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    select,
};
use tracing::{debug, debug_span, instrument, Instrument};
use uuid::Uuid;

const BUFFER_SIZE: usize = 8192;

#[derive(Debug)]
struct SSHKeyPair<'a> {
    public_key: Option<&'a Path>,
    private_key: Option<&'a Path>,
}

fn make_socket_address<A: ToSocketAddrs>(address: A) -> SocketAddr {
    address.to_socket_addrs().unwrap().next().unwrap()
}

/// Read the stream data and return stream data & its length.
#[instrument]
async fn read_stream<R: AsyncRead + Unpin + Debug>(mut stream: R) -> (Vec<u8>, usize) {
    let mut request_buffer = vec![];
    // let us loop & try to read the whole request data
    let mut request_len = 0usize;
    loop {
        let mut buffer = vec![0; BUFFER_SIZE];
        // println!("Reading stream data");
        match stream.read(&mut buffer).await {
            Ok(n) => {
                if !read_buf_bytes(&mut request_len, &mut request_buffer, n, buffer) {
                    break;
                }
            }
            Err(e) => {
                println!("Error in reading request data: {:?}", e);
                break;
            }
        }
    }

    (request_buffer, request_len)
}

/// Read the stream data and return stream data & its length.
#[instrument(skip(stream))]
async fn read_async_channel<R: AsyncReadExt + Unpin>(stream: &mut R) -> (Vec<u8>, usize) {
    let mut response_buffer = vec![];
    // let us loop & try to read the whole request data
    let mut response_len = 0usize;
    loop {
        let mut buffer = vec![0; BUFFER_SIZE];
        // println!("Reading stream data");
        let future_stream = stream.read(&mut buffer);
        thread::sleep(Duration::from_millis(10));

        match future_stream.await {
            Ok(n) => {
                if !read_buf_bytes(&mut response_len, &mut response_buffer, n, buffer) {
                    break;
                }
            }
            Err(e) => {
                println!("Error in reading response data: {:?}", e);
                break;
            }
        }
    }

    (response_buffer, response_len)
}

#[instrument(skip(session))]
async fn handle_req(
    remote_port: u16,
    session: Arc<AsyncSession<TcpStream>>,
    mut stream: TcpStream,
    _unique_id: String,
) {
    let mut channel = session
        .channel_direct_tcpip("localhost", remote_port, None)
        .await
        .unwrap();

    let (request, req_bytes) = read_stream(&mut stream).await;

    debug!(
        "REQUEST ({} BYTES): {}",
        req_bytes,
        String::from_utf8_lossy(&request[..])
    );
    // send the incoming request over ssh on to the remote localhost and port
    // where an HTTP server is listening
    channel.write_all(&request[..req_bytes]).await.unwrap();
    channel.flush().await.unwrap();
    channel.eof();

    let (response, res_bytes) = read_async_channel(&mut channel).await;

    stream.write_all(&response[..res_bytes]).await.unwrap();
    stream.flush().await.unwrap();
    debug!("SENT {} BYTES AS RESPONSE\n", res_bytes);
}

#[instrument]
async fn create_ssh_session(
    username: &str,
    remote_address: SocketAddr,
    key_pair: SSHKeyPair<'_>,
) -> Result<AsyncSession<TcpStream>, Error> {
    let stream = TcpStream::connect(remote_address).await?;
    let mut session = AsyncSession::new(stream, None)?;
    session.handshake().await.unwrap();
    session
        .userauth_pubkey_file(
            username,
            key_pair.public_key,
            key_pair.private_key.unwrap(),
            None,
        )
        .await?;

    if !session.authenticated() {
        Err(session
            .last_error()
            .map(Error::from)
            .unwrap_or_else(|| Error::new(ErrorKind::Other, "unknown user auth error")))
    } else {
        Ok(session)
    }
}

#[instrument(skip(ssh_session))]
async fn local_port_forward(
    local_listener: TcpListener,
    remote_port: u16,
    ssh_session: AsyncSession<TcpStream>,
    should_exit: Arc<AtomicBool>,
) -> std::io::Result<()> {
    let ssh_session = Arc::from(ssh_session);
    loop {
        if should_exit.load(Ordering::SeqCst) {
            break;
        }

        match local_listener.accept().await {
            Ok((stream, _a)) => {
                let unique_id = Uuid::new_v4().to_string();
                let span = debug_span!("handle_req", unique_id = unique_id);
                let _enter = span.enter();
                let cloned_session = Arc::clone(&ssh_session);
                tokio::spawn(
                    handle_req(remote_port, cloned_session, stream, unique_id).in_current_span(),
                );
            }
            Err(e) => panic!("encountered error: {}", e),
        }
    }

    println!("TCP Listener stopped");

    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    setup_tracing();
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
        public_key: public_key.as_ref().map(|p| p.as_ref()),
        private_key: private_key.as_ref().map(|p| p.as_ref()),
    };

    let session = match create_ssh_session(&args.user, remote_address, key_pair).await {
        Ok(sess) => sess,
        Err(e) => return Err(e),
    };

    let should_exit = Arc::new(AtomicBool::new(false));
    let listener_should_exit = Arc::clone(&should_exit);

    let local_address = make_socket_address(("127.0.0.1", args.local_port));

    let local_listener = match TcpListener::bind(local_address).await {
        Ok(listener) => {
            if let Ok(address) = listener.local_addr() {
                debug!("{}", address);
                listener
            } else {
                debug!("Could not get local address");
                std::process::exit(1);
            }
        }
        Err(e) => {
            debug!("Error in binding to local port: {}", e);
            std::process::exit(1);
        }
    };

    let t1 = tokio::spawn(local_port_forward(
        local_listener,
        args.remote_port,
        session,
        listener_should_exit,
    ));

    let t2 = tokio::spawn(async move {
        tokio::signal::ctrl_c().await.unwrap();
        {
            should_exit.store(true, Ordering::SeqCst);
            let _ = TcpStream::connect(make_socket_address(local_address))
                .await
                .unwrap();
        }
    });

    select! {
        _ = t1 => {},
        _ = t2 => {},
    }

    Ok(())
}
