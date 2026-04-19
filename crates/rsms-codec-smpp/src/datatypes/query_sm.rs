use bytes::{Buf, BufMut, BytesMut};
use std::io::Cursor;

use super::CommandId;
use crate::codec::{decode_cstring, encode_cstring, CodecError, Decodable, Encodable, PduHeader};

#[derive(Clone, Debug, PartialEq)]
pub struct QuerySm {
    pub message_id: String,
    pub source_addr_ton: u8,
    pub source_addr_npi: u8,
    pub source_addr: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct QuerySmResp {
    pub message_id: String,
    pub message_state: u8,
    pub message_state_text: Option<String>,
}

impl QuerySm {
    pub fn new(message_id: &str, source_addr: &str) -> Self {
        Self {
            message_id: message_id.to_string(),
            source_addr_ton: 0,
            source_addr_npi: 0,
            source_addr: source_addr.to_string(),
        }
    }
}

impl Encodable for QuerySm {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        encode_cstring(buf, &self.message_id, 65)?;
        buf.put_u8(self.source_addr_ton);
        buf.put_u8(self.source_addr_npi);
        encode_cstring(buf, &self.source_addr, 21)?;
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        65 + 1 + 1 + 21
    }
}

impl Decodable for QuerySm {
    fn decode(header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if header.command_id != CommandId::QUERY_SM {
            return Err(CodecError::FieldValidation {
                field: "command_id",
                reason: "mismatch".to_string(),
            });
        }
        let message_id = decode_cstring(buf, 65, "message_id")?;
        let source_addr_ton = buf.get_u8();
        let source_addr_npi = buf.get_u8();
        let source_addr = decode_cstring(buf, 21, "source_addr")?;
        Ok(Self {
            message_id,
            source_addr_ton,
            source_addr_npi,
            source_addr,
        })
    }

    fn command_id() -> CommandId {
        CommandId::QUERY_SM
    }
}

impl Encodable for QuerySmResp {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        encode_cstring(buf, &self.message_id, 65)?;
        buf.put_u8(self.message_state);
        if let Some(ref text) = self.message_state_text {
            encode_cstring(buf, text, 65)?;
        }
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        65 + 1
            + self
                .message_state_text
                .as_ref()
                .map(|s| s.len() + 1)
                .unwrap_or(0)
    }
}

impl Decodable for QuerySmResp {
    fn decode(header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if header.command_id != CommandId::QUERY_SM_RESP {
            return Err(CodecError::FieldValidation {
                field: "command_id",
                reason: "mismatch".to_string(),
            });
        }
        let message_id = decode_cstring(buf, 65, "message_id")?;
        let message_state = buf.get_u8();
        let message_state_text = if buf.has_remaining() {
            Some(decode_cstring(buf, 65, "message_state_text")?)
        } else {
            None
        };
        Ok(Self {
            message_id,
            message_state,
            message_state_text,
        })
    }

    fn command_id() -> CommandId {
        CommandId::QUERY_SM_RESP
    }
}
