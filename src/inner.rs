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

use futures::StreamExt;
use futures_util::sink::SinkExt;
use tokio::{
    io::{AsyncRead, AsyncWrite, DuplexStream, ReadHalf, WriteHalf},
    sync::{mpsc, watch, RwLock},
};
use tokio_util::codec::{FramedRead, FramedWrite};
use tracing::{error, trace};

use crate::{
    config::Config,
    frame::{Flag, Frame, FrameDecoder, FrameEncoder},
    socket::MuxSocket,
};

type PortPair = (u16, u16);

pub(crate) struct StreamMultiplexorInner<T> {
    pub config: Config,
    pub connected: AtomicBool,
    pub port_connections: RwLock<HashMap<PortPair, Arc<MuxSocket<T>>>>,
    pub port_listeners: RwLock<HashMap<u16, async_channel::Sender<DuplexStream>>>,
    pub watch_connected_send: watch::Sender<bool>,
    pub send: RwLock<mpsc::Sender<Frame>>,
    pub running: watch::Sender<bool>,
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

impl<T: AsyncRead + AsyncWrite + Send + Unpin + 'static> StreamMultiplexorInner<T> {
    #[tracing::instrument(skip(recv, framed_writer))]
    pub async fn framed_writer_sender(
        self: Arc<Self>,
        mut recv: mpsc::Receiver<Frame>,
        mut framed_writer: FramedWrite<WriteHalf<T>, FrameEncoder>,
    ) {
        trace!("");

        let mut running = self.running.subscribe();
        while !*running.borrow() {
            if let Err(error) = running.changed().await {
                error!("Error {:?} receiving running state", error);
            }
        }

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
    }

    #[tracing::instrument(skip(framed_reader))]
    pub async fn framed_reader_sender(
        self: Arc<Self>,
        mut framed_reader: FramedRead<ReadHalf<T>, FrameDecoder>,
    ) {
        trace!("");

        let mut running = self.running.subscribe();
        while !*running.borrow() {
            if let Err(error) = running.changed().await {
                error!("Error {:?} receiving running state", error);
            }
        }

        loop {
            let frame: Frame = match framed_reader.next().await {
                Some(Ok(v)) => v,
                error => {
                    error!("Error {:?} reading from framed_reader", error);

                    self.watch_connected_send.send_replace(false);
                    self.connected.store(false, Ordering::Relaxed);

                    for (_, connection) in self.port_connections.write().await.drain() {
                        trace!("Send rst to {:?}", connection);
                        if connection.rst.send(true).is_err() {
                            error!("Error sending rst to connection {:?}", connection);
                        }
                        if let Some(sender) =
                            connection.external_stream_sender.write().await.as_ref()
                        {
                            trace!("Send Error to {:?} external_stream_reader", connection);
                            if let Err(error) = sender
                                .send(Err(io::Error::from(io::ErrorKind::BrokenPipe)))
                                .await
                            {
                                error!("Error {:?} dropping port_connections", error);
                            }
                        }
                    }
                    self.port_listeners.write().await.clear();

                    break;
                }
            };
            if matches!(frame.flag, Flag::Syn)
                && self.port_listeners.read().await.contains_key(&frame.dport)
            {
                trace!("Syn received for listener, vending MuxSocket");
                let socket = MuxSocket::new(self.clone(), frame.dport, frame.sport, true);
                self.port_connections
                    .write()
                    .await
                    .insert((frame.dport, frame.sport), socket.clone());
                socket.recv_frame(frame).await;
            } else if let Some(socket) = self
                .port_connections
                .read()
                .await
                .get(&(frame.dport, frame.sport))
            {
                trace!("Frame received for active socket {:?}", socket);
                socket.recv_frame(frame).await;
            } else if !matches!(frame.flag, Flag::Rst) {
                trace!(
                    "Frame received for unknown (dport, sport) ({}, {}), sending Rst",
                    frame.dport,
                    frame.sport
                );
                let framed_writer = self.send.write().await;
                if let Err(error) = framed_writer
                    .send(Frame::new_reply(&frame, Flag::Rst, 0))
                    .await
                {
                    error!("Error {:?} sending Rst", error);
                }
            }
        }
    }
}
