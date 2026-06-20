use bytes::{Buf, BufMut, BytesMut};
use std::io::Cursor;

use crate::codec::{CodecError, Decodable, Encodable, PduHeader};
use crate::datatypes::CommandId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveTest;

impl ActiveTest {
    pub const BODY_SIZE: usize = 0;
}

impl Encodable for ActiveTest {
    fn encode(&self, _buf: &mut BytesMut) -> Result<(), CodecError> {
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        Self::BODY_SIZE
    }
}

impl Decodable for ActiveTest {
    fn decode(header: PduHeader, _buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if header.total_length != (PduHeader::SIZE + Self::BODY_SIZE) as u32 {
            return Err(CodecError::InvalidPduLength {
                length: header.total_length,
                min: (PduHeader::SIZE + Self::BODY_SIZE) as u32,
                max: (PduHeader::SIZE + Self::BODY_SIZE) as u32,
            });
        }
        Ok(ActiveTest)
    }

    fn command_id() -> CommandId {
        CommandId::ActiveTest
    }
}

/// SMGP 3.0.3 Active_Test_Resp **无 body**（12B PDU，同 Active_Test）。
/// 旧实现多 1 字节 reserved(13B) 不合规——真实网关(cmos)按定长解码会 ArrayIndexOutOfBounds、
/// 打断连接（真机联调暴露：服务端心跳应答 13B 致 cmos 长连接每次心跳即断）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveTestResp;

impl ActiveTestResp {
    pub const BODY_SIZE: usize = 0;
}

impl Encodable for ActiveTestResp {
    fn encode(&self, _buf: &mut BytesMut) -> Result<(), CodecError> {
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        Self::BODY_SIZE
    }
}

impl Decodable for ActiveTestResp {
    fn decode(header: PduHeader, _buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if header.total_length != (PduHeader::SIZE + Self::BODY_SIZE) as u32 {
            return Err(CodecError::InvalidPduLength {
                length: header.total_length,
                min: (PduHeader::SIZE + Self::BODY_SIZE) as u32,
                max: (PduHeader::SIZE + Self::BODY_SIZE) as u32,
            });
        }
        Ok(ActiveTestResp)
    }

    fn command_id() -> CommandId {
        CommandId::ActiveTestResp
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exit;

impl Exit {
    pub const BODY_SIZE: usize = 0;
}

impl Encodable for Exit {
    fn encode(&self, _buf: &mut BytesMut) -> Result<(), CodecError> {
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        Self::BODY_SIZE
    }
}

impl Decodable for Exit {
    fn decode(header: PduHeader, _buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if header.total_length != (PduHeader::SIZE + Self::BODY_SIZE) as u32 {
            return Err(CodecError::InvalidPduLength {
                length: header.total_length,
                min: (PduHeader::SIZE + Self::BODY_SIZE) as u32,
                max: (PduHeader::SIZE + Self::BODY_SIZE) as u32,
            });
        }
        Ok(Exit)
    }

    fn command_id() -> CommandId {
        CommandId::Exit
    }
}

/// SMGP 3.0.3 Exit_Resp：body 为空（与 Exit 对称，总长 12B）。
/// 权威参考实现 cmos(lihuanghe/SMSGate) 发的 Exit_Resp 即 12B；旧版误加 1 字节 reserved
/// （BODY_SIZE=1、总长 13B）导致解码 cmos 的 12B Exit_Resp 报 `Invalid PDU length:12, must be 13-13`、
/// 关闭握手时静默吞掉对端 Exit_Resp——现纠正为 0 字节体（联调发现，2026-06-14）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitResp;

impl ExitResp {
    pub const BODY_SIZE: usize = 0;
}

impl Encodable for ExitResp {
    fn encode(&self, _buf: &mut BytesMut) -> Result<(), CodecError> {
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        Self::BODY_SIZE
    }
}

impl Decodable for ExitResp {
    fn decode(header: PduHeader, _buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if header.total_length != (PduHeader::SIZE + Self::BODY_SIZE) as u32 {
            return Err(CodecError::InvalidPduLength {
                length: header.total_length,
                min: (PduHeader::SIZE + Self::BODY_SIZE) as u32,
                max: (PduHeader::SIZE + Self::BODY_SIZE) as u32,
            });
        }
        Ok(ExitResp)
    }

    fn command_id() -> CommandId {
        CommandId::ExitResp
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::Pdu;

    fn decode_pdu<T: Decodable>(bytes: &[u8]) -> Result<T, CodecError> {
        let mut cursor = Cursor::new(bytes);
        let header = PduHeader::decode(&mut cursor)?;
        T::decode(header, &mut cursor)
    }

    #[test]
    fn active_test_roundtrip() {
        let at = ActiveTest;
        let bytes = Pdu::from(at).to_pdu_bytes(1);
        let decoded = decode_pdu::<ActiveTest>(&bytes.as_slice()).unwrap();
        let _ = decoded;
    }

    #[test]
    fn active_test_resp_roundtrip() {
        let resp = ActiveTestResp;
        let bytes = Pdu::from(resp.clone()).to_pdu_bytes(1);
        // SMGP Active_Test_Resp 合规为 12B（无 body）。
        assert_eq!(bytes.len(), 12, "SMGP ActiveTestResp 应为 12B（无保留字节）");
        let decoded = decode_pdu::<ActiveTestResp>(&bytes.as_slice()).unwrap();
        assert_eq!(decoded, resp);
    }

    #[test]
    fn exit_roundtrip() {
        let exit = Exit;
        let bytes = Pdu::from(exit).to_pdu_bytes(1);
        let decoded = decode_pdu::<Exit>(&bytes.as_slice()).unwrap();
        let _ = decoded;
    }

    #[test]
    fn exit_resp_roundtrip() {
        let resp = ExitResp;
        let bytes = Pdu::from(resp).to_pdu_bytes(1);
        // 总长应为 12B（PduHeader 12 + body 0），与 cmos 一致
        assert_eq!(bytes.as_slice().len(), 12);
        let _decoded = decode_pdu::<ExitResp>(bytes.as_slice()).unwrap();
    }
}
