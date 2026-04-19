use bytes::{Buf, BytesMut};
use std::io::Cursor;

use crate::codec::{CodecError, Decodable, Encodable, PduHeader};
use crate::datatypes::CommandId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Terminate;

impl Terminate {
    pub const BODY_SIZE: usize = 0;
}

impl Encodable for Terminate {
    fn encode(&self, _buf: &mut BytesMut) -> Result<(), CodecError> {
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        Self::BODY_SIZE
    }
}

impl Decodable for Terminate {
    fn decode(header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if header.total_length != PduHeader::SIZE as u32 {
            return Err(CodecError::InvalidPduLength {
                length: header.total_length,
                min: PduHeader::SIZE as u32,
                max: PduHeader::SIZE as u32,
            });
        }
        if buf.remaining() > 0 {
            return Err(CodecError::FieldValidation {
                field: "body",
                reason: "must be empty".to_string(),
            });
        }
        Ok(Terminate)
    }

    fn command_id() -> CommandId {
        CommandId::Terminate
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminateResp;

impl TerminateResp {
    pub const BODY_SIZE: usize = 0;
}

impl Encodable for TerminateResp {
    fn encode(&self, _buf: &mut BytesMut) -> Result<(), CodecError> {
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        Self::BODY_SIZE
    }
}

impl Decodable for TerminateResp {
    fn decode(header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if header.total_length != PduHeader::SIZE as u32 {
            return Err(CodecError::InvalidPduLength {
                length: header.total_length,
                min: PduHeader::SIZE as u32,
                max: PduHeader::SIZE as u32,
            });
        }
        if buf.remaining() > 0 {
            return Err(CodecError::FieldValidation {
                field: "body",
                reason: "must be empty".to_string(),
            });
        }
        Ok(TerminateResp)
    }

    fn command_id() -> CommandId {
        CommandId::TerminateResp
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
    fn terminate_roundtrip() {
        let pdu = Terminate;
        let bytes = Pdu::from(pdu.clone()).to_pdu_bytes(1);
        let decoded = decode_pdu::<Terminate>(&bytes.as_slice()).unwrap();
        assert_eq!(decoded, pdu);
    }

    #[test]
    fn terminate_resp_roundtrip() {
        let pdu = TerminateResp;
        let bytes = Pdu::from(pdu.clone()).to_pdu_bytes(2);
        let decoded = decode_pdu::<TerminateResp>(&bytes.as_slice()).unwrap();
        assert_eq!(decoded, pdu);
    }
}
