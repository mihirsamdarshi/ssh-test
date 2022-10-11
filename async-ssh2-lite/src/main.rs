use std::{
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

use async_ssh2_lite::AsyncSession;
use common_port_forward::{get_args, setup_tracing};
use futures::executor::block_on;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

const BUFFER_SIZE: usize = 8192;

struct SSHKeyPair<'a> {
    public_key: Option<&'a Path>,
    private_key: Option<&'a Path>,
}

fn make_socket_address<A: ToSocketAddrs>(address: A) -> SocketAddr {
    address.to_socket_addrs().unwrap().next().unwrap()
}

fn read_buf_bytes(
    full_req_len: &mut usize,
    full_req_buf: &mut Vec<u8>,
    reader_buf_len: usize,
    mut reader_buf: Vec<u8>,
) -> bool {
    // Added these lines for verification of reading requests correctly
    if reader_buf_len == 0 {
        // Added these lines for verification of reading requests correctly
        println!("No bytes read from response");
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

/// Read the stream data and return stream data & its length.
async fn read_stream<R: AsyncRead + Unpin>(mut stream: R) -> (Vec<u8>, usize) {
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

async fn handle_req(remote_port: u16, session: &AsyncSession<TcpStream>, mut stream: TcpStream) {
    let mut channel = session
        .channel_direct_tcpip("localhost", remote_port, None)
        .await
        .unwrap();

    let (request, req_bytes) = read_stream(&mut stream).await;

    println!(
        "REQUEST ({} BYTES): {}",
        req_bytes,
        String::from_utf8_lossy(&request[..])
    );
    // send the incoming request over ssh on to the remote localhost and port
    // where an HTTP server is listening
    channel.write_all(&request[..req_bytes]).await.unwrap();
    channel.flush().await.unwrap();

    let (response, res_bytes) = read_async_channel(&mut channel).await;

    stream.write_all(&response[..res_bytes]).await.unwrap();
    stream.flush().await.unwrap();
    println!("SENT {} BYTES AS RESPONSE\n", res_bytes);
}

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

async fn local_port_forward(
    local_listener: TcpListener,
    remote_port: u16,
    ssh_session: AsyncSession<TcpStream>,
    should_exit: Arc<AtomicBool>,
) -> std::io::Result<()> {
    loop {
        if should_exit.load(Ordering::SeqCst) {
            break;
        }

        match local_listener.accept().await {
            Ok((stream, _a)) => {
                handle_req(remote_port, &ssh_session, stream).await;
            }
            Err(e) => panic!("encountered error: {}", e),
        }
    }

    println!("TCP Listener stopped");

    Ok(())
}

async fn run() -> std::io::Result<()> {
    setup_tracing();
    let args = get_args();

    let remote_address = SocketAddr::new(IpAddr::V4(args.ip), 22);

    let private_key = Some(Path::new(&args.private_key_path));
    let public_key = match &args.public_key_path {
        Some(path) => Some(Path::new(path)),
        None => None,
    };

    let key_pair = SSHKeyPair {
        public_key,
        private_key,
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
                println!("{}", address);
                listener
            } else {
                println!("Could not get local address");
                std::process::exit(1);
            }
        }
        Err(e) => {
            println!("Error in binding to local port: {}", e);
            std::process::exit(1);
        }
    };

    let handler = tokio::spawn(local_port_forward(
        local_listener,
        args.remote_port,
        session,
        listener_should_exit,
    ));

    println!("sleeping from main thread");
    thread::sleep(Duration::from_secs(6000));
    println!("sleep ended, sending abort message");

    should_exit.store(true, Ordering::SeqCst);
    let _ = TcpStream::connect(make_socket_address(local_address)).await?;

    handler.await.unwrap()
}

fn main() -> std::io::Result<()> {
    block_on(run())
}
