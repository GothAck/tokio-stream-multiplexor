//! # Tokio Stream Multiplexor
//!
//! TL;DR: Multiplex multiple streams over a single stream. Has a TcpListener / TcpSocket style interface, and uses u16 ports similar to TCP itself.
//!
//! ```toml
//! [dependencies]
//! foo = "1.2.3"
//! ```
//!
//! ## Why?
//!
//! Because sometimes you wanna cram as much shite down one TCP stream as you possibly can, rather than have your application connect with multiple ports.
//!
//! ## But Whyyyyyy?
//!
//! Because I needed it. Inspired by [async-smux](https://github.com/black-binary/async-smux), written for [Tokio](https://tokio.rs/).
//!
//! ## What about performance?
//! Doesn't this whole protocol in a protocol thing hurt perf?
//!
//! Sure, but take a look at the benches:
//!
//! ```ignore
//! throughput/tcp          time:   [28.968 ms 30.460 ms 31.744 ms]
//!                         thrpt:  [7.8755 GiB/s 8.2076 GiB/s 8.6303 GiB/s]
//!
//! throughput/mux          time:   [122.24 ms 135.96 ms 158.80 ms]
//!                         thrpt:  [1.5743 GiB/s 1.8388 GiB/s 2.0451 GiB/s]
//! ```
//!
//! Approximately 4.5 times slower than TCP, but still able to shovel 1.8 GiB/s of shite... Seems alright to me. (Numbers could possibly be improved with some tuning of the config params too.)

mod config;
mod frame;
mod inner;
mod listener;
mod socket;

use std::{
    collections::HashMap,
    fmt::{Debug, Formatter, Result as FmtResult},
    io,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

extern crate async_channel;
use rand::Rng;
pub use tokio::io::DuplexStream;
use tokio::{
    io::{split, AsyncRead, AsyncWrite},
    sync::{mpsc, watch, RwLock},
};
use tokio_util::codec::{FramedRead, FramedWrite};
use tracing::{error, trace};

pub use config::StreamMultiplexorConfig;
use frame::{FrameDecoder, FrameEncoder};
use inner::StreamMultiplexorInner;
pub use listener::MuxListener;
use socket::MuxSocket;

pub type Result<T> = std::result::Result<T, io::Error>;

pub struct StreamMultiplexor<T> {
    inner: Arc<StreamMultiplexorInner<T>>,
}

impl<T> Debug for StreamMultiplexor<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        f.debug_struct("StreamMultiplexor")
            .field("id", &self.inner.config.identifier)
            .field("inner", &self.inner)
            .finish()
    }
}

impl<T> Drop for StreamMultiplexor<T> {
    fn drop(&mut self) {
        trace!("drop {:?}", self);
    }
}

impl<T: AsyncRead + AsyncWrite + Send + Unpin + 'static> StreamMultiplexor<T> {
    /// Constructs a new `StreamMultiplexor<T>`
    pub fn new(inner: T, config: StreamMultiplexorConfig) -> Self {
        Self::new_running(inner, config, true)
    }

    /// Constructs a new paused `StreamMultiplexor<T>`
    ///
    /// This allows you to bind and listen on a bunch of ports before
    /// processing any packets from the inner stream, removing race conditions
    /// between bind and connect. Call `start()` to start processing the
    /// inner stream.
    pub fn new_paused(inner: T, config: StreamMultiplexorConfig) -> Self {
        Self::new_running(inner, config, false)
    }

    fn new_running(inner: T, config: StreamMultiplexorConfig, running: bool) -> Self {
        let (reader, writer) = split(inner);

        let framed_reader = FramedRead::new(reader, FrameDecoder::new(config));
        let framed_writer = FramedWrite::new(writer, FrameEncoder::new(config));
        let (send, recv) = mpsc::channel(config.max_queued_frames);
        let (watch_connected_send, _) = watch::channel(true);
        let (running, _) = watch::channel(running);
        let inner = Arc::from(StreamMultiplexorInner {
            config,
            connected: AtomicBool::from(true),
            port_connections: RwLock::from(HashMap::new()),
            port_listeners: RwLock::from(HashMap::new()),
            watch_connected_send,
            send: RwLock::from(send),
            running,
        });

        tokio::spawn(inner.clone().framed_writer_sender(recv, framed_writer));
        tokio::spawn(inner.clone().framed_reader_sender(framed_reader));

        Self { inner }
    }

    /// Start processing the inner stream.
    ///
    /// Only effective on a paused `StreamMultiplexor<T>`.
    /// See `new_paused(inner: T, config: StreamMultiplexorConfig)`.
    pub fn start(&self) {
        if let Err(error) = self.inner.running.send(true) {
            error!("Error {:?} setting running to true", error);
        }
    }

    /// Bind to port and return a `MuxListener<T>`
    #[tracing::instrument]
    pub async fn bind(&self, port: u16) -> Result<MuxListener<T>> {
        trace!("");
        if !self.inner.connected.load(Ordering::Relaxed) {
            return Err(io::Error::from(io::ErrorKind::ConnectionReset));
        }
        let mut port = port;
        if port == 0 {
            while port < 1024 || self.inner.port_listeners.read().await.contains_key(&port) {
                port = rand::thread_rng().gen_range(1024u16..u16::MAX);
            }
            trace!("port = {}", port);
        } else if self.inner.port_listeners.read().await.contains_key(&port) {
            trace!("port_listeners already contains {}", port);
            return Err(io::Error::from(io::ErrorKind::AddrInUse));
        }
        let (send, recv) = async_channel::bounded(self.inner.config.accept_queue_len);
        self.inner.port_listeners.write().await.insert(port, send);
        Ok(MuxListener::new(self.inner.clone(), port, recv))
    }

    /// Connect to `port` on the remote end
    #[tracing::instrument]
    pub async fn connect(&self, port: u16) -> Result<DuplexStream> {
        trace!("");
        if !self.inner.connected.load(Ordering::Relaxed) {
            trace!("Not connected, raise Error");
            return Err(io::Error::from(io::ErrorKind::ConnectionReset));
        }
        let mut sport: u16 = 0;
        while sport < 1024
            || self
                .inner
                .port_connections
                .read()
                .await
                .contains_key(&(sport, port))
        {
            sport = rand::thread_rng().gen_range(1024u16..u16::MAX);
        }
        trace!("sport = {}", sport);

        let mux_socket = MuxSocket::new(self.inner.clone(), sport, port, false);
        let mut rx = mux_socket.stream().await;
        self.inner
            .port_connections
            .write()
            .await
            .insert((sport, port), mux_socket.clone());
        mux_socket.start().await;

        rx.recv()
            .await
            .ok_or_else(|| io::Error::from(io::ErrorKind::Other))?
    }

    #[tracing::instrument]
    pub fn watch_connected(&self) -> watch::Receiver<bool> {
        trace!("");
        self.inner.watch_connected_send.subscribe()
    }
}

#[cfg(test)]
mod test;
