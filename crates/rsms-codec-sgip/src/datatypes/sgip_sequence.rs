use bytes::{Buf, BufMut, BytesMut};
use std::io::Cursor;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SgipSequence {
    pub node_id: u32,
    pub timestamp: u32,
    pub number: u32,
}

impl SgipSequence {
    pub fn new(node_id: u32, timestamp: u32, number: u32) -> Self {
        Self {
            node_id,
            timestamp,
            number,
        }
    }

    pub fn decode(buf: &mut Cursor<&[u8]>) -> Result<Self, std::io::Error> {
        let node_id = buf.get_u32();
        let timestamp = buf.get_u32();
        let number = buf.get_u32();
        Ok(Self {
            node_id,
            timestamp,
            number,
        })
    }

    pub fn encode(&self, buf: &mut BytesMut) {
        buf.put_u32(self.node_id);
        buf.put_u32(self.timestamp);
        buf.put_u32(self.number);
    }

    pub const SIZE: usize = 12;
}
