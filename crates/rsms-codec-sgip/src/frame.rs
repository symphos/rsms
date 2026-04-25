use bytes::{Buf, BytesMut};
use rsms_core::RsmsError;

pub struct FrameDecoder {
    buf: BytesMut,
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self {
            buf: BytesMut::new(),
        }
    }

    pub fn decode_frames(&mut self, chunk: &[u8]) -> Result<Vec<Vec<u8>>, RsmsError> {
        self.buf.extend_from_slice(chunk);
        decode_frames(&mut self.buf)
    }
}

pub fn decode_frames(buf: &mut BytesMut) -> Result<Vec<Vec<u8>>, RsmsError> {
    let mut out = Vec::new();
    loop {
        if buf.len() < 4 {
            break;
        }
        let total = u32::from_be_bytes(buf[0..4].try_into().unwrap()) as usize;
        if !(20..=100_000).contains(&total) {
            return Err(RsmsError::Codec(format!("invalid SGIP PDU total_length: {}", total)));
        }
        if buf.len() < total {
            break;
        }
        let frame = buf[..total].to_vec();
        buf.advance(total);
        out.push(frame);
    }
    Ok(out)
}
