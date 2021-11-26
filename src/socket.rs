use std::{
    fmt::{Debug, Formatter, Result as FmtResult},
    io,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
};

extern crate async_channel;

pub use tokio::io::DuplexStream;
use tokio::{
    io::{duplex, split, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadHalf, WriteHalf},
    sync::{mpsc, watch, RwLock},
};
use tracing::{error, info, trace};

use crate::{
    frame::{Flag, Frame},
    inner::StreamMultiplexorInner,
    Result,
};

#[derive(Debug, Copy, Clone)]
enum PortState {
    Closed,
    SynAck,
    Ack,
    Open,
}

pub(crate) struct MuxSocket<T> {
    inner: Arc<StreamMultiplexorInner<T>>,
    accepting: bool,
    sport: u16,
    dport: u16,
    state: RwLock<PortState>,
    seq: AtomicU32,
    write_half: RwLock<Option<WriteHalf<DuplexStream>>>,
    pub(crate) rst: watch::Sender<bool>,
    pub(crate) external_stream_sender: RwLock<Option<mpsc::Sender<Result<DuplexStream>>>>,
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
        error!("drop {:?}", self);
    }
}

impl<T: AsyncRead + AsyncWrite + Send + Unpin + 'static> MuxSocket<T> {
    pub fn new(
        inner: Arc<StreamMultiplexorInner<T>>,
        sport: u16,
        dport: u16,
        accepting: bool,
    ) -> Arc<Self> {
        let (rst, _) = watch::channel(false);
        Arc::from(Self {
            inner,
            accepting,
            sport,
            dport,
            state: RwLock::from(PortState::Closed),
            seq: AtomicU32::new(0),
            write_half: RwLock::from(None),
            rst,
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
            .send(Frame::new_init(self.sport, self.dport, Flag::Syn))
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

        tokio::spawn(self.clone().stream_read(read_half));

        s1
    }

    #[tracing::instrument]
    async fn stream_read(self: Arc<Self>, read_half: ReadHalf<DuplexStream>) {
        trace!("");
        let mut rst = self.rst.subscribe();
        let mut connected = self.inner.watch_connected_send.subscribe();
        let mut read_half = read_half;
        let mut buf: Vec<u8> = vec![];
        buf.resize(self.inner.config.buf_size, 0);
        loop {
            info!("stream_read loop");
            if *rst.borrow() {
                trace!("Rst is true");
                break;
            }
            if !*connected.borrow() {
                trace!("Connected is false");
                break;
            }
            let bytes = tokio::select! {
                res = read_half.read(&mut buf) => {
                    match res {
                        Ok(bytes) => bytes,
                        Err(error) => {
                            error!("Error {:?} reading bytes from read_half", error);
                            break;
                        },
                    }
                }
                _ = rst.changed() => {
                    trace!("Rst changed");
                    continue;
                }
                _ = connected.changed() => {
                    trace!("Connected changed");
                    continue;
                }
            };
            trace!("bytes = {}", bytes);
            if bytes == 0 {
                trace!("bytes == 0; closed");
                break;
            }
            if let Err(error) = self
                .inner
                .send
                .write()
                .await
                .send(Frame::new_data(
                    self.sport,
                    self.dport,
                    self.seq.fetch_add(1, Ordering::Relaxed),
                    &buf[..bytes],
                ))
                .await
            {
                error!("Error {:?} sending data frame", error);
            }
        }
        trace!("Drop write_half");
        *self.write_half.write().await = None;
        self.inner
            .port_connections
            .write()
            .await
            .remove(&(self.sport, self.dport));

        trace!("Send Fin");
        if let Err(error) = self
            .inner
            .send
            .write()
            .await
            .send(Frame::new_no_data(
                self.sport,
                self.dport,
                Flag::Fin,
                self.seq.fetch_add(1, Ordering::Relaxed),
            ))
            .await
        {
            error!("Error {:?} sending Fin", error);
        }
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
                    let _ = self.rst.send(true);
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
                let _ = self.rst.send(true);
            }
        }
    }
}
