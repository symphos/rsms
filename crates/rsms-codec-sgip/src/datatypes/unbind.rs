use bytes::{Buf, BytesMut};
use std::io::Cursor;

use crate::codec::{CodecError, Decodable, Encodable, PduHeader};
use crate::datatypes::CommandId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unbind;

impl Encodable for Unbind {
    fn encode(&self, _buf: &mut BytesMut) -> Result<(), CodecError> {
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        0
    }
}

impl Decodable for Unbind {
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
        Ok(Unbind)
    }

    fn command_id() -> CommandId {
        CommandId::Unbind
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnbindResp;

impl Encodable for UnbindResp {
    fn encode(&self, _buf: &mut BytesMut) -> Result<(), CodecError> {
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        0
    }
}

impl Decodable for UnbindResp {
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
        Ok(UnbindResp)
    }

    fn command_id() -> CommandId {
        CommandId::UnbindResp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_pdu<T: Decodable>(bytes: &[u8]) -> Result<T, CodecError> {
        let mut cursor = Cursor::new(bytes);
        let header = PduHeader::decode(&mut cursor)?;
        T::decode(header, &mut cursor)
    }

    #[test]
    fn unbind_roundtrip() {
        let unbind = Unbind;
        let bytes = unbind.to_pdu_bytes(1, 2, 3);
        let decoded = decode_pdu::<Unbind>(&bytes).unwrap();
        let _ = decoded;
    }

    #[test]
    fn unbind_resp_roundtrip() {
        let resp = UnbindResp;
        let bytes = resp.to_pdu_bytes(1, 2, 3);
        let decoded = decode_pdu::<UnbindResp>(&bytes).unwrap();
        let _ = decoded;
    }
}
