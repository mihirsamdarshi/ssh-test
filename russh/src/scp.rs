//! SCP helpers built on top of a russh client session.
//!
//! Kept as a demonstration of the `exec`/`ChannelMsg` API; the port-forwarding
//! binary itself does not use it.
#![allow(dead_code)]

use std::{
    fs::File,
    io::{Cursor, Write},
};

use async_trait::async_trait;
use russh::{client, ChannelMsg};
use tokio::io::{AsyncRead, AsyncReadExt};

const CONFIRM: &[u8] = &[0];

#[async_trait]
pub trait Scp {
    async fn send_file<R: AsyncRead + Send>(
        &mut self,
        dirname: &str,
        basename: &str,
        contents: R,
        contents_len: usize,
        permissions: usize,
    ) -> anyhow::Result<(), russh::Error>;

    async fn receive_file<W: Write + Send>(
        &mut self,
        source: &str,
        target: &str,
    ) -> anyhow::Result<(), russh::Error>;
}

#[async_trait]
impl<H: client::Handler> Scp for client::Handle<H> {
    async fn send_file<R: AsyncRead + Send>(
        &mut self,
        dirname: &str,
        basename: &str,
        contents: R,
        contents_len: usize,
        permissions: usize,
    ) -> anyhow::Result<(), russh::Error> {
        // Request a channel, and wait until it completes.
        let channel = self.channel_open_session().await?;
        eprintln!("channel open: {:?}", channel.id());
        // Actually send the file.
        channel.exec(false, &*(format!("scp -t {dirname}"))).await?;

        // SCP needs the contents to be prefixed with the permission, length and base
        // name. https://blogs.oracle.com/janp/entry/how_the_scp_protocol_works
        let contents = Cursor::new(format!("C0{permissions:o} {contents_len} {basename}\n"))
            .chain(contents)
            .chain(CONFIRM);

        let pinned = Box::pin(contents);

        channel.data(pinned).await?;
        // Run the event loop until the channel closes.
        Ok(())
    }

    async fn receive_file<W: Write + Send>(
        &mut self,
        source: &str,
        target: &str,
    ) -> anyhow::Result<(), russh::Error> {
        // Request a channel, and wait until it completes.
        let mut channel = self.channel_open_session().await?;
        eprintln!("channel open: {:?}", channel.id());
        // Actually send the file.
        channel.exec(false, &*(format!("scp -f {source}"))).await?;
        // Run the event loop until the channel closes.
        let mut file = File::create(target).unwrap();

        loop {
            match channel.wait().await {
                Some(ChannelMsg::Data { ref data }) => {
                    // `ChannelMsg::Data` carries `Bytes` in russh 0.62 (it was a
                    // `CryptoVec` with `write_all_from` before).
                    file.write_all(data).unwrap();
                }
                Some(ChannelMsg::Eof | ChannelMsg::Close) => {
                    break;
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => {
                    eprintln!("exit status: {exit_status}");
                    break;
                }
                Some(ChannelMsg::ExitSignal {
                    signal_name,
                    core_dumped,
                    error_message,
                    ref lang_tag,
                }) => {
                    eprintln!(
                        "exit signal: {signal_name:?}, core dumped: {core_dumped}, error: \
                         {error_message:?}, lang tag: {lang_tag:?}"
                    );
                    break;
                }
                _ => {}
            };
        }
        Ok(())
    }
}
