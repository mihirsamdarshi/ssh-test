use std::{
    fmt::Debug,
    io::{Read, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use anyhow::anyhow;
use common_port_forward::{expand_home_dir, get_args, setup_tracing};
use ssh2::Session;
use tracing::{
    info, instrument,
    log::{debug, error},
};

const LOCALHOST: IpAddr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
const BUFFER_SIZE: usize = 128;

#[instrument]
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
#[instrument]
fn read_stream<R: Read + Debug>(mut stream: R) -> (Vec<u8>, usize) {
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
                error!("Error in reading request data: {:?}", e);
                break;
            }
        }
    }

    (request_buffer, request_len)
}

/// Read the stream data and return stream data & its length.
fn read_channel<R: Read>(channel: &mut R) -> (Vec<u8>, usize) {
    let mut response_buffer = vec![];
    // let us loop & try to read the whole request data
    let mut response_len = 0usize;
    loop {
        let mut buffer = vec![0; BUFFER_SIZE];
        // println!("Reading stream data");
        let future_stream = channel.read(&mut buffer);
        std::thread::sleep(Duration::from_millis(10));

        match future_stream {
            Ok(n) => {
                if !read_buf_bytes(&mut response_len, &mut response_buffer, n, buffer) {
                    break;
                }
            }
            Err(e) => {
                error!("Error in reading response data: {:?}", e);
                break;
            }
        }
    }

    (response_buffer, response_len)
}

#[instrument(skip(session))]
fn handle_req(session: Arc<Session>, mut stream: TcpStream, remote_port: u16) {
    if let Ok(channel) = session.channel_direct_tcpip("localhost", remote_port, None) {
        let mut channel = Box::new(channel);
        // read the user-facing TCPStream
        let (request, req_bytes) = read_stream(&mut stream);

        debug!(
            "REQUEST ({} BYTES): {}",
            req_bytes,
            String::from_utf8_lossy(&request[..])
        );
        // send the incoming request over the channel to the remote localhost and port
        match channel.write_all(&request[..req_bytes]) {
            Ok(_) => (),
            Err(e) => error!("Failed to forward request, error: {}", e),
        };
        channel.flush().unwrap();

        // read the response from the channel to the remote server
        let (response, res_bytes) = read_channel(&mut channel);

        // then forward the response to the user-facing TCPStream
        match stream.write_all(&response[..res_bytes]) {
            Ok(_) => (),
            Err(e) => error!("Failed to write response, error: {}", e),
        };
        stream.flush().unwrap();
        debug!("SENT {} BYTES AS RESPONSE\n", res_bytes);
        channel.close().expect("Failed to close channel");
    } else {
        panic!("backend_error: Failed to open channel")
    };
}

#[instrument(skip(ssh_session))]
fn listen_on_forwarded_port(
    ssh_session: Arc<Session>,
    should_exit: Arc<AtomicBool>,
    local_port: u16,
    remote_port: u16,
) -> std::io::Result<()> {
    match TcpListener::bind((LOCALHOST, local_port)) {
        Ok(listener) => {
            info!("Listening on port {}", local_port);
            // loop over incoming TCPStreams (requests)
            for stream in listener.incoming() {
                let cloned_session = Arc::clone(&ssh_session);
                // check that the shared AtomicBool does not say to exit the TCPStream
                if should_exit.load(Ordering::SeqCst) {
                    println!("Received close connection signal");
                    break;
                }

                match stream {
                    Ok(stream) => {
                        std::thread::spawn(move || handle_req(cloned_session, stream, remote_port));
                    }
                    Err(e) => panic!("encountered error: {e}"),
                }
            }
        }
        Err(e) => panic!("encountered error while getting listener: {e}"),
    }

    println!("TCP Listener stopped");

    Ok(())
}

fn main() -> anyhow::Result<()> {
    setup_tracing();
    let args = get_args();

    let tcp = TcpStream::connect(SocketAddr::new(IpAddr::V4(args.ip), 22)).unwrap();
    let mut sess = Session::new().unwrap();

    let exit_signal = Arc::new(AtomicBool::new(false));
    let tx = Arc::clone(&exit_signal);
    ctrlc::set_handler(move || {
        tx.store(true, Ordering::SeqCst);
        TcpStream::connect(SocketAddr::new(LOCALHOST, args.local_port)).unwrap();
        info!("Received Ctrl-C, exiting");
    })
    .expect("Error setting Ctrl-C handler");

    info!("Session created");
    sess.set_tcp_stream(tcp);
    info!("TCP Stream set");
    sess.handshake().unwrap();
    sess.userauth_pubkey_file(
        &args.user,
        None,
        &expand_home_dir(&args.private_key_path).map_err(|e| anyhow!(e))?,
        None,
    )
    .expect("failed to authenticate with public key");
    if sess.authenticated() {
        info!("Authenticated with public key");
    } else {
        panic!("Failed to authenticate with public key");
    }
    sess.set_keepalive(true, 30);

    listen_on_forwarded_port(
        Arc::new(sess),
        Arc::clone(&exit_signal),
        args.local_port,
        args.remote_port,
    )
    .unwrap();

    Ok(())
}
