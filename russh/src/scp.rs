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
        let mut channel = self.channel_open_session().await?;
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

        loop {
            match channel.wait().await {
                Some(ChannelMsg::Data { ref data }) => {
                    let mut s: Vec<u8> = vec![];
                    data.write_all_from(0, &mut s).unwrap();
                    let mut file = File::create(target).unwrap();
                    file.write_all(&s).unwrap();
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
