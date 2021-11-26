mod config;
mod frame;

use std::{
    collections::HashMap,
    fmt::{
        Debug,
        Formatter,
        Result as FmtResult,
    },
    io,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc,
    },
};

extern crate async_channel;

use futures::StreamExt;
use futures_util::sink::SinkExt;
use rand::Rng;
pub use tokio::io::DuplexStream;
use tokio::{
    io::{duplex, split, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, WriteHalf},
    sync::{mpsc, watch, RwLock},
};
use tokio_util::codec::{FramedRead, FramedWrite};
use tracing::{error, trace};

pub use config::Config;
use frame::{Flag, Frame, FrameDecoder, FrameEncoder};

type Result<T> = std::result::Result<T, io::Error>;

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
    /// Constructs a new StreamMultiplexor<T>
    pub fn new(inner: T, config: Config) -> Self {
        let (reader, writer) = split(inner);

        let framed_reader = FramedRead::new(reader, FrameDecoder::new(config));
        let framed_writer = FramedWrite::new(writer, FrameEncoder::new(config));
        let (send, recv) = mpsc::channel(10);
        let (watch_connected_send, _) = watch::channel(true);
        let inner = Arc::from(StreamMultiplexorInner {
            config,
            connected: AtomicBool::from(true),
            port_connections: RwLock::from(HashMap::new()),
            port_listeners: RwLock::from(HashMap::new()),
            watch_connected_send: watch_connected_send,
            send: RwLock::from(send),
        });

        tokio::spawn(async move {
            let mut recv = recv;
            let mut framed_writer = framed_writer;
            loop {
                let frame = match recv.recv().await {
                    Some(v) => v,
                    error => {
                        error!("Error {:?} reading from stream", error);
                        break;
                    }
                };
                if let Err(error) = framed_writer.send(frame).await {
                    error!("Error {:?} reading from stream", error);
                    break;
                }
            }
        });

        let inner_clone = inner.clone();
        tokio::spawn(async move {
            let mut framed_reader = framed_reader;
            loop {
                let frame: Frame = match framed_reader.next().await {
                    Some(Ok(v)) => v,
                    error => {
                        error!("Error {:?} reading from framed_reader", error);

                        inner_clone.watch_connected_send.send_replace(false);
                        inner_clone.connected.store(false, Ordering::Relaxed);

                        for (_, connection) in inner_clone.port_connections.write().await.drain() {
                            if let Some(sender) = connection.external_stream_sender.write().await.as_ref() {
                                if let Err(error) = sender.send(Err(io::Error::from(io::ErrorKind::BrokenPipe))).await {
                                    error!("Error {:?} dropping port_connections", error);
                                }
                            }
                        }
                        inner_clone.port_listeners.write().await.clear();

                        break;
                    }
                };
                if matches!(frame.flag, Flag::Syn)
                    && inner_clone
                        .port_listeners
                        .read()
                        .await
                        .contains_key(&frame.dport)
                {
                    let socket =
                        MuxSocket::new(inner_clone.clone(), frame.dport, frame.sport, true);
                    inner_clone
                        .port_connections
                        .write()
                        .await
                        .insert((frame.dport, frame.sport), socket.clone());
                    socket.recv_frame(frame).await;
                } else if let Some(socket) = inner_clone
                    .port_connections
                    .read()
                    .await
                    .get(&(frame.dport, frame.sport))
                {
                    socket.recv_frame(frame).await;
                } else if !matches!(frame.flag, Flag::Rst) {
                    let framed_writer = inner_clone.send.write().await;
                    if let Err(error) = framed_writer
                        .send(Frame::new_reply(&frame, Flag::Rst, 0))
                        .await
                    {
                        error!("Error {:?} sending Rst", error);
                    }
                }
            }
        });

        Self { inner }
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
        Ok(MuxListener {
            inner: self.inner.clone(),
            port,
            recv,
        })
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
            .ok_or(io::Error::from(io::ErrorKind::Other))?
    }

    #[tracing::instrument]
    pub fn watch_connected(&self) -> watch::Receiver<bool> {
        trace!("");
        self.inner.watch_connected_send.subscribe()
    }
}

#[derive(Debug, Copy, Clone)]
enum PortState {
    Closed,
    SynAck,
    Ack,
    Open,
}

type PortPair = (u16, u16);

struct StreamMultiplexorInner<T> {
    config: Config,
    connected: AtomicBool,
    port_connections: RwLock<HashMap<PortPair, Arc<MuxSocket<T>>>>,
    port_listeners: RwLock<HashMap<u16, async_channel::Sender<DuplexStream>>>,
    watch_connected_send: watch::Sender<bool>,
    send: RwLock<mpsc::Sender<Frame>>,
}

impl<T> Debug for StreamMultiplexorInner<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        f.debug_struct("StreamMultiplexorInner")
            .field("id", &self.config.identifier)
            .field("connected", &self.connected)
            .finish()
    }
}

impl<T> Drop for StreamMultiplexorInner<T> {
    fn drop(&mut self) {
        trace!("drop {:?}", self);
    }
}

pub struct MuxListener<T> {
    inner: Arc<StreamMultiplexorInner<T>>,
    port: u16,
    recv: async_channel::Receiver<DuplexStream>,
}

impl<T> Debug for MuxListener<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        f.debug_struct("MuxListener")
            .field("id", &self.inner.config.identifier)
            .field("port", &self.port)
            .finish()
    }
}

impl<T> Drop for MuxListener<T> {
    fn drop(&mut self) {
        trace!("drop {:?}", self);
    }
}

impl<T: AsyncRead + AsyncWrite + Send + Unpin + 'static> MuxListener<T> {
    /// Accept a connection from the remote side
    #[tracing::instrument]
    pub async fn accept(&self) -> Result<DuplexStream> {
        trace!("");
        Ok(self.recv.recv().await.map_err(|e| io::Error::new(io::ErrorKind::Other, e))?)
    }
}

struct MuxSocket<T> {
    inner: Arc<StreamMultiplexorInner<T>>,
    accepting: bool,
    sport: u16,
    dport: u16,
    state: RwLock<PortState>,
    seq: AtomicU32,
    write_half: RwLock<Option<WriteHalf<DuplexStream>>>,
    external_stream_sender: RwLock<Option<mpsc::Sender<Result<DuplexStream>>>>,
}

impl<T> Debug for MuxSocket<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        f.debug_struct("MuxSocket")
            .field("id", &self.inner.config.identifier)
            .field("sport", &self.sport)
            .field("dport", &self.dport)
            .finish()
    }
}

impl<T> Drop for MuxSocket<T> {
    fn drop(&mut self) {
        trace!("drop {:?}", self);
    }
}

impl<T: AsyncRead + AsyncWrite + Send + Unpin + 'static> MuxSocket<T> {
    pub fn new(
        inner: Arc<StreamMultiplexorInner<T>>,
        sport: u16,
        dport: u16,
        accepting: bool,
    ) -> Arc<Self> {
        Arc::from(Self {
            inner,
            accepting,
            sport,
            dport,
            state: RwLock::from(PortState::Closed),
            seq: AtomicU32::new(0),
            write_half: RwLock::from(None),
            external_stream_sender: RwLock::from(None),
        })
    }

    pub async fn stream(self: &Arc<Self>) -> mpsc::Receiver<Result<DuplexStream>> {
        trace!("");
        let (sender, receiver) = mpsc::channel(1);
        *self.external_stream_sender.write().await = Some(sender);
        receiver
    }

    #[tracing::instrument]
    pub async fn start(self: &Arc<Self>) {
        trace!("");
        if let Err(error) = self
            .inner
            .send
            .write()
            .await
            .send(Frame::new_init(
                self.sport,
                self.dport,
                Flag::Syn,
            ))
            .await
        {
            error!("Error {:?} sending Syn", error);
        }
        *self.state.write().await = PortState::Ack;
    }

    #[tracing::instrument]
    async fn spawn_stream(self: &Arc<Self>) -> DuplexStream {
        trace!("");
        let (s1, s2) = duplex(self.inner.config.max_frame_size);

        let (read_half, write_half) = split(s2);

        *self.write_half.write().await = Some(write_half);

        let self_clone = self.clone();
        tokio::spawn(async move {
            let mut read_half = read_half;
            let mut buf: Vec<u8> = vec![];
            buf.resize(self_clone.inner.config.buf_size, 0);
            loop {
                let bytes = match read_half.read(&mut buf).await {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        error!("Error {:?} reading bytes from read_half", error);
                        break;
                    }
                };
                if bytes == 0 {
                    break;
                }
                if let Err(error) = self_clone
                    .inner
                    .send
                    .write()
                    .await
                    .send(Frame::new_data(
                        self_clone.sport,
                        self_clone.dport,
                        self_clone.seq.fetch_add(1, Ordering::Relaxed),
                        &buf[..bytes],
                    ))
                    .await
                {
                    error!("Error {:?} sending data frame", error);
                }
            }
            if let Err(error) = self_clone
                .inner
                .send
                .write()
                .await
                .send(Frame::new_rst(
                    self_clone.sport,
                    self_clone.dport,
                    self_clone.seq.fetch_add(1, Ordering::Relaxed),
                ))
                .await
            {
                error!("Error {:?} sending Rst", error);
            }
            *self_clone.write_half.write().await = None;
            self_clone
                .inner
                .port_connections
                .write()
                .await
                .remove(&(self_clone.sport, self_clone.dport));
        });

        s1
    }

    #[tracing::instrument]
    pub async fn recv_frame(self: &Arc<Self>, frame: Frame) {
        trace!("");
        let state: PortState = *self.state.read().await;
        match frame.flag {
            Flag::Syn => {
                if let PortState::Closed = state {
                    trace!("{:?} {:?}", frame.flag, state);
                    if let Err(error) = self
                        .inner
                        .send
                        .write()
                        .await
                        .send(Frame::new_reply(
                            &frame,
                            Flag::SynAck,
                            self.seq.fetch_add(1, Ordering::Relaxed),
                        ))
                        .await
                    {
                        error!("Error {:?} sending SynAck", error);
                    }
                    *self.state.write().await = PortState::SynAck;
                }
            }
            Flag::SynAck | Flag::Ack => match state {
                PortState::SynAck | PortState::Ack => {
                    trace!("{:?} {:?}", frame.flag, state);
                    if let Err(error) = self
                        .inner
                        .send
                        .write()
                        .await
                        .send(Frame::new_reply(
                            &frame,
                            Flag::Ack,
                            self.seq.fetch_add(1, Ordering::Relaxed),
                        ))
                        .await
                    {
                        error!("Error {:?} sending Ack", error);
                    }
                    *self.state.write().await = PortState::Open;
                    if self.accepting {
                        if let Some(sender) =
                            self.inner.port_listeners.write().await.get(&frame.dport)
                        {
                            let stream = self.spawn_stream().await;
                            if let Err(error) = sender.send(stream).await {
                                error!("Error {:?} sending DuplexStream to acceptor", error);
                            }
                        }
                    } else if let Some(sender) = self.external_stream_sender.write().await.as_ref()
                    {
                        let stream = self.spawn_stream().await;
                        if let Err(error) = sender.send(Ok(stream)).await {
                            error!("Error {:?} sending DuplexStream to connector", error);
                        }
                    }
                }
                _ => {}
            },
            Flag::Unset => {
                if let PortState::Open = state {
                    trace!("{:?} {:?}", frame.flag, state);
                    if let Some(write_half) = self.write_half.write().await.as_mut() {
                        if let Err(error) = write_half.write_all(&frame.data).await {
                            error!("Error {:?} writing data to write_half", error);
                        }
                    }
                }
            }
            Flag::Fin => {
                if let PortState::Open = state {
                    trace!("{:?} {:?}", frame.flag, state);
                    if let Err(error) = self
                        .inner
                        .send
                        .write()
                        .await
                        .send(Frame::new_reply(
                            &frame,
                            Flag::Fin,
                            self.seq.fetch_add(1, Ordering::Relaxed),
                        ))
                        .await
                    {
                        error!("Error {:?} sending Fin", error);
                    }
                    *self.state.write().await = PortState::Closed;
                    *self.write_half.write().await = None;
                }
            }
            Flag::Rst => {
                if matches!(state, PortState::Closed | PortState::Ack) {
                    trace!("{:?} {:?}", frame.flag, state);
                    if let Some(stream_sender) = self.external_stream_sender.write().await.as_ref()
                    {
                        if let Err(error) = stream_sender
                            .send(Err(io::Error::from(io::ErrorKind::AddrNotAvailable)))
                            .await
                        {
                            error!("Error {:?} sending Error to connection", error);
                        }
                    }
                }
                *self.state.write().await = PortState::Closed;
                *self.write_half.write().await = None;
            }
        }
    }
}

#[cfg(test)]
mod test;
