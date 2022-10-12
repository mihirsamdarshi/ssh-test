use std::{
    borrow::Cow,
    fs::OpenOptions,
    net::Ipv4Addr,
    path::{Path, PathBuf},
};

use clap::Parser;
use lazy_static::lazy_static;
use tracing::instrument;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

const BUFFER_SIZE: usize = 16_384;

pub fn expand_home_dir<P: AsRef<Path> + ?Sized>(path: &P) -> Result<Cow<Path>, String> {
    let path = path.as_ref();

    if !path.starts_with("~") {
        return Ok(path.into());
    }

    lazy_static! {
        static ref HOME_DIR: String = std::env::var("HOME").unwrap();
    }

    let home_dir = Path::new(&*HOME_DIR);

    Ok(home_dir
        .join(path.strip_prefix("~").map_err(|e| e.to_string())?)
        .into())
}

/// Simple program to forward a local port to a remote port on a remote host.
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Arguments {
    /// The username to connect as on the remote host (e.g. root).
    #[arg(short, long)]
    pub user: String,
    /// The IPV4 address of the remote host (e.g. 80.69.420.85).
    #[arg(short, long)]
    pub ip: Ipv4Addr,
    /// The port on the remote host to connect to (e.g. 8000).
    #[arg(short, long)]
    pub remote_port: u16,
    /// The local port to listen on (e.g 9876).
    #[arg(short, long)]
    pub local_port: u16,
    /// The path to the private key to use for authentication.
    #[arg(short, long)]
    pub private_key_path: PathBuf,
    /// The path to the public key to use for authentication.
    #[arg(short = 'k', long)]
    pub public_key_path: Option<PathBuf>,
}

/// Get arguments from the command line.
pub fn get_args() -> Arguments {
    Arguments::parse()
}

#[instrument(skip(reader_buf))]
pub fn read_buf_bytes(
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

/// Setup tracing for any program that uses this library.
pub fn setup_tracing() {
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
}
