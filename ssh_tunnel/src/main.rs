/*
cargo run
*/

use std::{str, thread, time};
use std::io::{Error, ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use async_io::Async;
use async_ssh2_lite::AsyncSession;
use futures::{AsyncReadExt, AsyncWriteExt};
use futures::executor::block_on;

const LOCAL_ADDRESS: &str = "localhost:1234";
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

fn socket_address_from_str_slice(str_address: &str) -> SocketAddr {
    str_address
        .to_string()
        .to_socket_addrs()
        .unwrap()
        .next()
        .unwrap()
}

/// Read the stream data and return stream data & its length.
fn read_stream<R: Read>(mut stream: R) -> (Vec<u8>, usize) {
    let buffer_size = 512;
    let mut request_buffer = vec![];
    // let us loop & try to read the whole request data
    let mut request_len = 0usize;
    loop {
        let mut buffer = vec![0; buffer_size];
        // println!("Reading stream data");
        match stream.read(&mut buffer) {
            Ok(n) => {
                // Added these lines for verification of reading requests correctly
                if n == 0 {
                    // Added these lines for verification of reading requests correctly
                    break;
                } else {
                    request_len += n;

                    // we need not read more data in case we have read less data than buffer size
                    if n < buffer_size {
                        // let us only append the data how much we have read rather than complete existing buffer data
                        // as n is less than buffer size
                        request_buffer.append(&mut buffer[..n].to_vec()); // convert slice into vec
                        break;
                    } else {
                        // append complete buffer vec data into request_buffer vec as n == buffer_size
                        request_buffer.append(&mut buffer);
                    }
                }
            }
            Err(e) => {
                println!("Error in reading stream data: {:?}", e);
                break;
            }
        }
    }

    (request_buffer, request_len)
}

/// Read the stream data and return stream data & its length.
async fn read_async_channel<R: AsyncReadExt + std::marker::Unpin>(stream: &mut R) -> (Vec<u8>, usize) {
    let buffer_size = 512;
    let mut request_buffer = vec![];
    // let us loop & try to read the whole request data
    let mut request_len = 0usize;
    loop {
        let mut buffer = vec![0; buffer_size];
        // println!("Reading stream data");
        match stream.read(&mut buffer).await {
            Ok(n) => {
                // Added these lines for verification of reading requests correctly
                if n == 0 {
                    // Added these lines for verification of reading requests correctly
                    break;
                } else {
                    request_len += n;

                    // we need not read more data in case we have read less data than buffer size
                    if n < buffer_size {
                        // let us only append the data how much we have read rather than complete existing buffer data
                        // as n is less than buffer size
                        request_buffer.append(&mut buffer[..n].to_vec()); // convert slice into vec
                        break;
                    } else {
                        // append complete buffer vec data into request_buffer vec as n == buffer_size
                        request_buffer.append(&mut buffer);
                    }
                }
            }
            Err(e) => {
                println!("Error in reading stream data: {:?}", e);
                break;
            }
        }
    }

    (request_buffer, request_len)
}

async fn handle_req(session: &AsyncSession<TcpStream>, mut stream: TcpStream) {
    let remote_port: u16 = SERVER_PORT_ON_REMOTE;
    let mut channel = session
        .channel_direct_tcpip("localhost", remote_port, None)
        .await
        .unwrap();

    let (request, req_bytes) = read_stream(&mut stream);

    println!("REQUEST ({} BYTES): ", req_bytes);
    // send the incoming request over ssh on to the remote localhost and port
    // where an HTTP server is listening
    channel.write_all(&request[..req_bytes]).await.unwrap();
    channel.flush().await.unwrap();

    let (response, res_bytes) = read_async_channel(&mut channel).await;
    channel.flush().await.unwrap();

    stream.write_all(&response).unwrap();
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
            .unwrap_or_else(|| Error::new(ErrorKind::Other, "unknown userauth error")))
    } else {
        Ok(session)
    }
}

async fn local_port_forward(
    ssh_session: AsyncSession<TcpStream>,
    local_address: SocketAddr,
    should_exit: Arc<AtomicBool>,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(local_address).unwrap();

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

    println!("TCP Listener stopped");

    Ok(())
}

async fn run() -> std::io::Result<()> {
    let username = REMOTE_USERNAME;
    let local_address = socket_address_from_str_slice(LOCAL_ADDRESS);
    let remote_address = socket_address_from_str_slice(REMOTE_ADDRESS);

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

    let handler = thread::spawn(move || {
        block_on(local_port_forward(
            session,
            local_address,
            listener_should_exit,
        ))
    });

    println!("sleeping from main thread");
    thread::sleep(time::Duration::from_secs(6000));
    println!("sleep ended, sending abort message");

    should_exit.store(true, Ordering::SeqCst);
    let _ = TcpStream::connect(local_address)?;

    handler.join().unwrap()
}

fn main() -> std::io::Result<()> {
    block_on(run())
}
