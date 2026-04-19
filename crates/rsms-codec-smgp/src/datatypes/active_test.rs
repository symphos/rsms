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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveTestResp {
    pub reserved: u8,
}

impl ActiveTestResp {
    pub const BODY_SIZE: usize = 1;
}

impl Encodable for ActiveTestResp {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        buf.put_u8(self.reserved);
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        Self::BODY_SIZE
    }
}

impl Decodable for ActiveTestResp {
    fn decode(header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if header.total_length != (PduHeader::SIZE + Self::BODY_SIZE) as u32 {
            return Err(CodecError::InvalidPduLength {
                length: header.total_length,
                min: (PduHeader::SIZE + Self::BODY_SIZE) as u32,
                max: (PduHeader::SIZE + Self::BODY_SIZE) as u32,
            });
        }
        if !buf.has_remaining() {
            return Err(CodecError::Incomplete);
        }
        let reserved = buf.get_u8();
        Ok(ActiveTestResp { reserved })
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitResp {
    pub reserved: u8,
}

impl ExitResp {
    pub const BODY_SIZE: usize = 1;
}

impl Encodable for ExitResp {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        buf.put_u8(self.reserved);
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        Self::BODY_SIZE
    }
}

impl Decodable for ExitResp {
    fn decode(header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if header.total_length != (PduHeader::SIZE + Self::BODY_SIZE) as u32 {
            return Err(CodecError::InvalidPduLength {
                length: header.total_length,
                min: (PduHeader::SIZE + Self::BODY_SIZE) as u32,
                max: (PduHeader::SIZE + Self::BODY_SIZE) as u32,
            });
        }
        if !buf.has_remaining() {
            return Err(CodecError::Incomplete);
        }
        let reserved = buf.get_u8();
        Ok(ExitResp { reserved })
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
        let resp = ActiveTestResp { reserved: 5 };
        let bytes = Pdu::from(resp.clone()).to_pdu_bytes(1);
        let decoded = decode_pdu::<ActiveTestResp>(&bytes.as_slice()).unwrap();
        assert_eq!(decoded.reserved, resp.reserved);
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
        let resp = ExitResp { reserved: 5 };
        let bytes = Pdu::from(resp.clone()).to_pdu_bytes(1);
        let decoded = decode_pdu::<ExitResp>(&bytes.as_slice()).unwrap();
        assert_eq!(decoded.reserved, resp.reserved);
    }
}
