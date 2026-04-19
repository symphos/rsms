use bytes::{Buf, BufMut, BytesMut};
use std::io::Cursor;

use crate::codec::{CodecError, Decodable, Encodable, PduHeader};
use crate::datatypes::CommandId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveTest;

impl Encodable for ActiveTest {
    fn encode(&self, _buf: &mut BytesMut) -> Result<(), CodecError> {
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        0
    }
}

impl Decodable for ActiveTest {
    fn decode(header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if header.total_length != PduHeader::SIZE as u32 {
            return Err(CodecError::InvalidPduLength {
                length: header.total_length,
                min: PduHeader::SIZE as u32,
                max: PduHeader::SIZE as u32,
            });
        }
        if buf.has_remaining() {
            return Err(CodecError::FieldValidation {
                field: "body",
                reason: "must be empty".to_string(),
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
        let active_test = ActiveTest;
        let bytes = Pdu::from(active_test).to_pdu_bytes(1);
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
}
