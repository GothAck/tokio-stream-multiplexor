use bytes::{Buf, Bytes, BytesMut};
use serde::{Deserialize, Serialize};
use tokio_util::codec::{Decoder, Encoder};

use super::config::Config;

#[derive(Serialize, Deserialize, Debug)]
pub struct Frame {
    pub sport: u16,
    pub dport: u16,
    pub flag: Flag,
    pub seq: u32,
    pub data: Bytes,
}

impl Frame {
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

    pub fn new_rst(sport: u16, dport: u16, seq: u32) -> Self {
        Self {
            sport,
            dport,
            flag: Flag::Rst,
            seq,
            data: Bytes::new(),
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

#[derive(Debug)]
pub struct FrameDecoder {
    config: Config,
}

impl FrameDecoder {
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Decoder for FrameDecoder {
    type Item = Frame;
    type Error = std::io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.len() < std::mem::size_of::<u64>() {
            return Ok(None);
        }
        let mut length_bytes = [0u8; std::mem::size_of::<u64>()];
        length_bytes.copy_from_slice(&src[0..std::mem::size_of::<u64>()]);
        let length = u64::from_le_bytes(length_bytes);

        if length as usize > self.config.max_frame_size {
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

        Ok(Some(item))
    }
}

#[derive(Debug)]
pub struct FrameEncoder {
    config: Config,
}

impl FrameEncoder {
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Encoder<Frame> for FrameEncoder {
    type Error = std::io::Error;

    fn encode(&mut self, item: Frame, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let encoded = bincode::serialize(&item)
            .map_err(|e| Self::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        let len = encoded.len();

        if len > self.config.max_frame_size {
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
        }

        dst.reserve(std::mem::size_of::<u64>() + len);

        dst.extend_from_slice(&u64::to_le_bytes(len as u64));
        dst.extend_from_slice(&encoded);

        Ok(())
    }
}
