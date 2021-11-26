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
        let (send, recv) = mpsc::channel(10);
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
        let (send, recv) = async_channel::bounded(8);
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
