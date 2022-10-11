use std::{fs::OpenOptions, net::Ipv4Addr};

use clap::Parser;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Simple program to forward a local port to a remote port on a remote host.
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
pub struct Arguments {
    /// The username to connect as on the remote host (e.g. root).
    #[clap(short, long, value_parser)]
    pub user: String,
    /// The IPV4 address of the remote host (e.g. 80.69.420.85).
    #[clap(short, long, value_parser)]
    pub ip: Ipv4Addr,
    /// The port on the remote host to connect to (e.g. 8000).
    #[clap(short, long, value_parser)]
    pub remote_port: u16,
    /// The local port to listen on (e.g 9876).
    #[clap(short, long, value_parser)]
    pub local_port: u16,
    /// The path to the private key to use for authentication.
    #[clap(short, long, value_parser)]
    pub private_key_path: String,
    /// The path to the public key to use for authentication.
    #[clap(short, long, value_parser)]
    pub public_key_path: Option<String>,
}

/// Get arguments from the command line.
pub fn get_args() -> Arguments {
    Arguments::parse()
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
