use bytes::{Buf, Bytes, BytesMut};
use serde::{Deserialize, Serialize};
use tokio_util::codec::{Decoder, Encoder};
use tracing::{error, trace};

use super::config::Config;

#[derive(Serialize, Deserialize)]
pub struct Frame {
    pub sport: u16,
    pub dport: u16,
    pub flag: Flag,
    pub seq: u32,
    pub data: Bytes,
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Frame")
            .field("sport", &self.sport)
            .field("dport", &self.dport)
            .field("flag", &self.flag)
            .field("seq", &self.seq)
            .field("data.len", &self.data.len())
            .finish()
    }
}

impl Frame {
    pub fn new_no_data(sport: u16, dport: u16, flag: Flag, seq: u32) -> Self {
        Self {
            sport,
            dport,
            flag,
            seq,
            data: Bytes::new(),
        }
    }
    pub fn new_init(sport: u16, dport: u16, flag: Flag) -> Self {
        Self {
            sport,
            dport,
            flag,
            seq: 0,
            data: Bytes::new(),
        }
    }

    pub fn new_reply(frame: &Frame, flag: Flag, seq: u32) -> Self {
        Self {
            sport: frame.dport,
            dport: frame.sport,
            flag,
            seq,
            data: Bytes::new(),
        }
    }

    pub fn new_data(sport: u16, dport: u16, seq: u32, data: &[u8]) -> Self {
        Self {
            sport,
            dport,
            flag: Flag::Unset,
            seq,
            data: Bytes::copy_from_slice(data),
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub enum Flag {
    Syn,
    SynAck,
    Ack,
    Rst,
    Fin,
    Unset,
}

pub struct FrameDecoder {
    config: Config,
}

impl FrameDecoder {
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl std::fmt::Debug for FrameDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameDecoder")
            .field("id", &self.config.identifier)
            .finish()
    }
}

impl Decoder for FrameDecoder {
    type Item = Frame;
    type Error = std::io::Error;

    #[tracing::instrument(skip(src))]
    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.len() < std::mem::size_of::<u64>() {
            return Ok(None);
        }
        let mut length_bytes = [0u8; std::mem::size_of::<u64>()];
        length_bytes.copy_from_slice(&src[0..std::mem::size_of::<u64>()]);
        let length = u64::from_le_bytes(length_bytes);

        if length as usize > self.config.max_frame_size {
            error!("Frame too large, dropping");
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
        }

        if src.len() < std::mem::size_of::<u64>() + length as usize {
            src.reserve(std::mem::size_of::<u64>() + length as usize - src.len());
            return Ok(None);
        }
        src.advance(std::mem::size_of::<u64>());
        let data = src[..length as usize].to_vec();
        src.advance(length as usize);

        let item: Self::Item = bincode::deserialize(&data)
            .map_err(|e| Self::Error::new(std::io::ErrorKind::InvalidData, e))?;

        trace!("Decoded frame {:?}", item);

        Ok(Some(item))
    }
}

pub struct FrameEncoder {
    config: Config,
}

impl FrameEncoder {
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl std::fmt::Debug for FrameEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameEncoder")
            .field("id", &self.config.identifier)
            .finish()
    }
}

impl Encoder<Frame> for FrameEncoder {
    type Error = std::io::Error;

    #[tracing::instrument(skip(item, dst))]
    fn encode(&mut self, item: Frame, dst: &mut BytesMut) -> Result<(), Self::Error> {
        trace!("Encoding frame {:?}", item);
        let encoded = bincode::serialize(&item)
            .map_err(|e| Self::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        let len = encoded.len();

        if len > self.config.max_frame_size {
            error!("Frame too large, dropping");
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
        }

        dst.reserve(std::mem::size_of::<u64>() + len);

        dst.extend_from_slice(&u64::to_le_bytes(len as u64));
        dst.extend_from_slice(&encoded);

        Ok(())
    }
}
