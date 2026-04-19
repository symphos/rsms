use bytes::{Buf, BytesMut};
use rsms_core::RsmsError;

/// 从流缓冲区中切出完整 CMPP PDU（首 4 字节大端为 `Total_Length`，与整包长度一致）。
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

    /// 追加读入数据并尽可能解析出多帧。
    pub fn decode_frames(&mut self, chunk: &[u8]) -> Result<Vec<Vec<u8>>, RsmsError> {
        self.buf.extend_from_slice(chunk);
        decode_frames(&mut self.buf)
    }
}

/// 对 `buf` 原地消费：返回所有完整帧，残留半包保留在 `buf` 中。
pub fn decode_frames(buf: &mut BytesMut) -> Result<Vec<Vec<u8>>, RsmsError> {
    let mut out = Vec::new();
    loop {
        if buf.len() < 4 {
            break;
        }
        let total = u32::from_be_bytes(buf[0..4].try_into().unwrap()) as usize;
        if total < 12 {
            return Err(RsmsError::Codec(format!("invalid total_length {total}")));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sticky_two_frames() {
        let mut b = BytesMut::from(
            &[
                0x00, 0x00, 0x00, 0x0c, // 12
                0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x0c, 0x00, 0x00,
                0x00, 0x08, 0x00, 0x00, 0x00, 0x02,
            ][..],
        );
        let frames = decode_frames(&mut b).unwrap();
        assert_eq!(frames.len(), 2);
        assert!(b.is_empty());
    }

    #[test]
    fn half_packet() {
        let mut b = BytesMut::from(&[0x00, 0x00, 0x00, 0x0c, 0x00][..]);
        let frames = decode_frames(&mut b).unwrap();
        assert!(frames.is_empty());
        assert_eq!(b.len(), 5);
    }
}
