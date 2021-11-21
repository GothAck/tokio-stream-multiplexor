mod config;
mod frame;

use std::{
    collections::HashMap,
    io,
    sync::{
        atomic::{AtomicU32, Ordering},
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
    sync::{mpsc, RwLock},
};
use tokio_util::codec::{FramedRead, FramedWrite};
use tracing::error;

pub use config::Config;
use frame::{Flag, Frame, FrameDecoder, FrameEncoder};

type Result<T> = std::result::Result<T, io::Error>;

#[derive(Debug)]
pub struct StreamMultiplexor<T> {
    inner: Arc<StreamMultiplexorInner<T>>,
}

impl<T: AsyncRead + AsyncWrite + Send + Unpin + 'static> StreamMultiplexor<T> {
    pub fn new(inner: T, config: Config) -> Self {
        let (reader, writer) = split(inner);

        let framed_reader = FramedRead::new(reader, FrameDecoder::new(config));
        let framed_writer = FramedWrite::new(writer, FrameEncoder::new(config));
        let (send, recv) = mpsc::channel(10);
        let inner = Arc::from(StreamMultiplexorInner {
            config,
            port_connections: RwLock::from(HashMap::new()),
            port_listeners: RwLock::from(HashMap::new()),
            send: RwLock::from(send),
        });

        tokio::spawn(async move {
            let mut recv = recv;
            let mut framed_writer = framed_writer;
            loop {
                let frame = match recv.recv().await {
                    Some(v) => v,
                    _ => {
                        return;
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
                    _ => {
                        return;
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

    pub async fn bind(&self, port: u16) -> Result<MuxListener<T>> {
        let mut port = port;
        if port == 0 {
            while port < 1024 || self.inner.port_listeners.read().await.contains_key(&port) {
                port = rand::thread_rng().gen();
            }
        } else if self.inner.port_listeners.read().await.contains_key(&port) {
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

    pub async fn connect(&self, port: u16) -> Result<DuplexStream> {
        let mut local_port = 0;
        while local_port < 1024
            || self
                .inner
                .port_connections
                .read()
                .await
                .contains_key(&(local_port, port))
        {
            local_port = rand::thread_rng().gen();
        }

        let mux_socket = MuxSocket::new(self.inner.clone(), local_port, port, false);
        let rx = mux_socket.stream().await;
        self.inner
            .port_connections
            .write()
            .await
            .insert((local_port, port), mux_socket.clone());
        mux_socket.start().await;

        rx.recv()
            .await
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?
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

#[derive(Debug)]
struct StreamMultiplexorInner<T> {
    config: Config,
    port_connections: RwLock<HashMap<PortPair, Arc<MuxSocket<T>>>>,
    port_listeners: RwLock<HashMap<u16, async_channel::Sender<DuplexStream>>>,
    send: RwLock<mpsc::Sender<Frame>>,
}

#[derive(Debug)]
pub struct MuxListener<T> {
    inner: Arc<StreamMultiplexorInner<T>>,
    port: u16,
    recv: async_channel::Receiver<DuplexStream>,
}

impl<T: AsyncRead + AsyncWrite + Send + Unpin + 'static> MuxListener<T> {
    pub async fn accept(&self) -> Option<DuplexStream> {
        self.recv.recv().await.ok()
    }
}

#[derive(Debug)]
struct MuxSocket<T> {
    inner: Arc<StreamMultiplexorInner<T>>,
    accepting: bool,
    local_port: u16,
    remote_port: u16,
    state: RwLock<PortState>,
    seq: AtomicU32,
    write_half: RwLock<Option<WriteHalf<DuplexStream>>>,
    external_stream_sender: RwLock<Option<async_channel::Sender<Result<DuplexStream>>>>,
}

impl<T: AsyncRead + AsyncWrite + Send + Unpin + 'static> MuxSocket<T> {
    pub fn new(
        inner: Arc<StreamMultiplexorInner<T>>,
        local_port: u16,
        remote_port: u16,
        accepting: bool,
    ) -> Arc<Self> {
        Arc::from(Self {
            inner,
            accepting,
            local_port,
            remote_port,
            state: RwLock::from(PortState::Closed),
            seq: AtomicU32::new(0),
            write_half: RwLock::from(None),
            external_stream_sender: RwLock::from(None),
        })
    }

    pub async fn stream(self: &Arc<Self>) -> async_channel::Receiver<Result<DuplexStream>> {
        let (sender, receiver) = async_channel::bounded(1);
        *self.external_stream_sender.write().await = Some(sender);
        receiver
    }

    pub async fn start(self: &Arc<Self>) {
        if let Err(error) = self
            .inner
            .send
            .write()
            .await
            .send(Frame::new_init(
                self.local_port,
                self.remote_port,
                Flag::Syn,
            ))
            .await
        {
            error!("Error {:?} sending Syn", error);
        }
        *self.state.write().await = PortState::Ack;
    }

    async fn spawn_stream(self: &Arc<Self>) -> DuplexStream {
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
                        self_clone.local_port,
                        self_clone.remote_port,
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
                    self_clone.local_port,
                    self_clone.remote_port,
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
                .remove(&(self_clone.local_port, self_clone.remote_port));
        });

        s1
    }

    pub async fn recv_frame(self: &Arc<Self>, frame: Frame) {
        let state: PortState = *self.state.read().await;
        match frame.flag {
            Flag::Syn => {
                if let PortState::Closed = state {
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
                    if let Some(write_half) = self.write_half.write().await.as_mut() {
                        if let Err(error) = write_half.write_all(&frame.data).await {
                            error!("Error {:?} writing data to write_half", error);
                        }
                    }
                }
            }
            Flag::Fin => {
                if let PortState::Open = state {
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
                if matches!(*self.state.read().await, PortState::Closed | PortState::Ack) {
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
#[ctor::ctor]
fn init_tests() {
    tracing_subscriber::fmt::init();
}

#[tokio::test]
async fn connect_no_listen_fails() {
    let (a, b) = duplex(10);

    let sm_a = StreamMultiplexor::new(a, Config::default());
    let _sm_b = StreamMultiplexor::new(b, Config::default());

    assert!(matches!(sm_a.connect(22).await, Err(_)));
}

#[tokio::test]
async fn connect_listen_succeeds() {
    let (a, b) = duplex(10);

    let sm_a = StreamMultiplexor::new(a, Config::default());
    let sm_b = StreamMultiplexor::new(b, Config::default());

    tokio::spawn(async move {
        let _connection = sm_b.bind(22).await.unwrap().accept().await;
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
    });

    assert!(matches!(sm_a.connect(22).await, Ok(_)));
}

#[tokio::test]
async fn dropped_connection_rsts() {
    let (a, b) = duplex(10);

    let sm_a = StreamMultiplexor::new(a, Config::default());
    let sm_b = StreamMultiplexor::new(b, Config::default());

    tokio::spawn(async move {
        sm_b.bind(22).await.unwrap().accept().await;
    });

    assert!(matches!(sm_a.connect(22).await, Ok(_)));
}

#[tokio::test]
async fn connected_stream_passes_data() {
    let (a, b) = duplex(10);

    let input_bytes: Vec<u8> = (0..(1024 * 1024)).map(|_| rand::random::<u8>()).collect();
    let len = input_bytes.len();

    let sm_a = StreamMultiplexor::new(a, Config::default());
    let sm_b = StreamMultiplexor::new(b, Config::default());

    let input_bytes_clone = input_bytes.clone();
    tokio::spawn(async move {
        let mut conn = sm_b.bind(22).await.unwrap().accept().await.unwrap();
        let mut i = 0;
        while i < input_bytes_clone.len() {
            let res = conn.write_all(&input_bytes_clone[i..i + 1024]).await;
            assert!(matches!(res, Ok(..)));
            i += 1024;
        }
    });

    let mut output_bytes: Vec<u8> = vec![];

    let mut conn = sm_a.connect(22).await.unwrap();
    while output_bytes.len() < len {
        let mut buf = [0u8; 2048];
        let bytes = conn.read(&mut buf).await.unwrap();
        output_bytes.extend_from_slice(&buf[..bytes]);
    }

    assert_eq!(input_bytes, output_bytes);
}
