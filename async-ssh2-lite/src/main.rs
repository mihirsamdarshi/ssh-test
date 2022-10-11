use std::{
    io::{Error, ErrorKind, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs},
    ops::Range,
    path::Path,
    str,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{channel, Sender},
        Arc,
    },
    thread, time,
    time::Duration,
};

use async_io::Async;
use async_ssh2_lite::AsyncSession;
use futures::{executor::block_on, AsyncReadExt, AsyncWriteExt};

const BUFFER_SIZE: usize = 8192;
const REMOTE_USERNAME: &str = "";
// include port, something like "123.123.123.123:22"
const REMOTE_ADDRESS: &str = "";
const SERVER_PORT_ON_REMOTE: u16 = 5000;
// key to access remote server
// something like "/home/me/.ssh/mykey.pub"
const PUBLIC_KEY_FULL_PATH: &str = "";
// something like "/home/me/.ssh/mykey", should have proper chmod 400 permissions on -nix systems
const PRIVATE_KEY_FULL_PATH: &str = "";

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
            // let us only append the data how much we have read rather than complete existing buffer data
            // as n is less than buffer size
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
fn read_stream<R: Read>(mut stream: R) -> (Vec<u8>, usize) {
    let mut request_buffer = vec![];
    // let us loop & try to read the whole request data
    let mut request_len = 0usize;
    loop {
        let mut buffer = vec![0; BUFFER_SIZE];
        // println!("Reading stream data");
        match stream.read(&mut buffer) {
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
async fn read_async_channel<R: AsyncReadExt + std::marker::Unpin>(
    stream: &mut R,
) -> (Vec<u8>, usize) {
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

async fn handle_req(session: &AsyncSession<TcpStream>, mut stream: TcpStream) {
    let remote_port: u16 = SERVER_PORT_ON_REMOTE;
    let mut channel = session
        .channel_direct_tcpip("localhost", remote_port, None)
        .await
        .unwrap();

    let (request, req_bytes) = read_stream(&mut stream);

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

    stream.write_all(&response[..res_bytes]).unwrap();
    stream.flush().unwrap();
    println!("SENT {} BYTES AS RESPONSE\n", res_bytes);
}

async fn create_ssh_session(
    username: &str,
    remote_address: SocketAddr,
    key_pair: SSHKeyPair<'_>,
) -> Result<AsyncSession<TcpStream>, Error> {
    let stream = Async::<TcpStream>::connect(remote_address).await?;
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

fn get_listener_in_port_range(port_range: Range<u16>) -> Result<TcpListener, String> {
    for port in port_range.clone() {
        if let Ok(listener) = TcpListener::bind(("127.0.0.1", port)) {
            return Ok(listener);
        } else {
            continue;
        };
    }
    Err(format!(
        "No ports in range {} - {} available",
        &port_range.start, &port_range.end
    ))
}

async fn local_port_forward(
    ssh_session: AsyncSession<TcpStream>,
    sender: Sender<Result<String, String>>,
    should_exit: Arc<AtomicBool>,
) -> std::io::Result<()> {
    match get_listener_in_port_range(1000..2000) {
        Ok(listener) => {
            if let Ok(address) = listener.local_addr() {
                sender.send(Ok(address.to_string())).unwrap();
            } else {
                sender
                    .send(Err("Could not retrieve address".to_string()))
                    .unwrap();
            }

            for stream in listener.incoming() {
                if should_exit.load(Ordering::SeqCst) {
                    break;
                }

                match stream {
                    Ok(stream) => {
                        handle_req(&ssh_session, stream).await;
                    }
                    Err(e) => panic!("encountered error: {}", e),
                }
            }
        }
        Err(e) => sender.send(Err(e)).unwrap()
    }

    println!("TCP Listener stopped");

    Ok(())
}

async fn run() -> std::io::Result<()> {
    let username = REMOTE_USERNAME;
    let remote_address = make_socket_address(REMOTE_ADDRESS);

    let key_pair = SSHKeyPair {
        public_key: Option::from(Path::new(PUBLIC_KEY_FULL_PATH)),
        private_key: Option::from(Path::new(PRIVATE_KEY_FULL_PATH)),
    };

    let session = match create_ssh_session(username, remote_address, key_pair).await {
        Ok(sess) => sess,
        Err(e) => return Err(e),
    };

    let should_exit = Arc::new(AtomicBool::new(false));
    let listener_should_exit = Arc::clone(&should_exit);

    let (tx, rx) = channel::<Result<String, String>>();

    let handler =
        thread::spawn(move || block_on(local_port_forward(session, tx, listener_should_exit)));

    let address = match rx.recv().unwrap() {
        Ok(msg) => {
            println!("{}", msg);
            msg
        }
        Err(e) => {
            should_exit.store(true, Ordering::SeqCst);
            handler.join().unwrap();
            panic!("{}", e)
        }
    };

    println!("sleeping from main thread");
    thread::sleep(time::Duration::from_secs(6000));
    println!("sleep ended, sending abort message");

    should_exit.store(true, Ordering::SeqCst);
    let _ = TcpStream::connect(make_socket_address(address))?;

    handler.join().unwrap()
}

fn main() -> std::io::Result<()> {
    block_on(run())
}
