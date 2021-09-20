/*
cargo run
*/

use std::io::{Error, ErrorKind, BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::{str, thread, time};

use async_io::Async;
use async_ssh2_lite::AsyncSession;
use futures::executor::block_on;
use futures::{AsyncReadExt, AsyncWriteExt};

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

async fn handle_req(session: &AsyncSession<TcpStream>, mut stream: TcpStream) {
    let remote_port: u16 = 5000;
    // Wrap the stream in a BufReader, so we can use the BufRead methods
    let mut reader = BufReader::new(&mut stream);
    // Read current current data in the TcpStream
    let request: Vec<u8> = reader.fill_buf().unwrap().to_vec();
    println!(
        "REQUEST ({} BYTES):\n{}",
        request.len(),
        str::from_utf8(&request).unwrap()
    );
    // send the incoming request over ssh on to the remote localhost and port
    // where an HTTP server is listening

    println!("Sending request to localhost:{}", remote_port);
    let mut channel = session
        .channel_direct_tcpip("localhost", remote_port, None)
        .await
        .unwrap();
    channel.write(&request).await.unwrap();
    println!("Request sent");
    // read the remote server's response (all of it, for simplicity's sake)
    // and forward it to the local TCP connection's stream
    let mut response = Vec::new();
    let read_bytes = channel.read_to_end(&mut response).await.unwrap();
    stream.write_all(&response).unwrap();
    println!("SENT {} BYTES AS RESPONSE\n", read_bytes);
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
    let username = "msamdars";
    let local_address = socket_address_from_str_slice("localhost:1234");
    let remote_address = socket_address_from_str_slice("");

    let key_pair = SSHKeyPair {
        public_key: Option::from(Path::new("")),
        private_key: Option::from(Path::new("")),
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
    thread::sleep(time::Duration::from_secs(600));
    println!("sleep ended, sending abort message");

    should_exit.store(true, Ordering::SeqCst);
    let _ = TcpStream::connect(local_address)?;

    handler.join().unwrap()
}

fn main() -> std::io::Result<()> {
    block_on(run())
}
