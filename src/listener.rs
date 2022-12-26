use std::{
    fmt::{Debug, Formatter, Result as FmtResult},
    io,
    sync::Arc,
};

extern crate async_channel;
pub use tokio::io::DuplexStream;
use tokio::io::{AsyncRead, AsyncWrite};
use tracing::{error, trace};

use crate::{inner::StreamMultiplexorInner, Result};

/// Listener struct returned by `StreamMultiplexor<T>::bind()`
///
/// # Drop
/// When the listener is dropped, it will free the port for reuse, but established
/// connections will not be closed.
pub struct MuxListener<T> {
    inner: Arc<StreamMultiplexorInner<T>>,
    port: u16,
    recv: async_channel::Receiver<DuplexStream>,
}

impl<T> MuxListener<T> {
    pub(crate) fn new(
        inner: Arc<StreamMultiplexorInner<T>>,
        port: u16,
        recv: async_channel::Receiver<DuplexStream>,
    ) -> Self {
        Self { inner, port, recv }
    }
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
        self.inner.may_close_listeners.send(self.port).ok();
        debug!("drop {:?}", self);
    }
}

impl<T: AsyncRead + AsyncWrite + Send + Unpin + 'static> MuxListener<T> {
    /// Accept a connection from the remote side
    #[tracing::instrument]
    pub async fn accept(&self) -> Result<DuplexStream> {
        trace!("");
        self.recv
            .recv()
            .await
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
    }

    /// Get the port number of this listener
    pub fn port(&self) -> u16 {
        self.port
    }
}
