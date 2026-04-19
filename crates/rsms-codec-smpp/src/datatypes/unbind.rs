use bytes::BytesMut;
use std::io::Cursor;

use super::CommandId;
use crate::codec::{CodecError, Decodable, Encodable, PduHeader};

#[derive(Clone, Debug, PartialEq)]
pub struct Unbind;

#[derive(Clone, Debug, PartialEq)]
pub struct UnbindResp;

impl Unbind {
    pub fn new() -> Self {
        Self
    }
}

impl Default for Unbind {
    fn default() -> Self {
        Self::new()
    }
}

impl Encodable for Unbind {
    fn encode(&self, _buf: &mut BytesMut) -> Result<(), CodecError> {
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        0
    }
}

impl Decodable for Unbind {
    fn decode(header: PduHeader, _buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if header.command_id != CommandId::UNBIND {
            return Err(CodecError::FieldValidation {
                field: "command_id",
                reason: "mismatch".to_string(),
            });
        }
        Ok(Self)
    }

    fn command_id() -> CommandId {
        CommandId::UNBIND
    }
}

impl Encodable for UnbindResp {
    fn encode(&self, _buf: &mut BytesMut) -> Result<(), CodecError> {
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        0
    }
}

impl Decodable for UnbindResp {
    fn decode(header: PduHeader, _buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if header.command_id != CommandId::UNBIND_RESP {
            return Err(CodecError::FieldValidation {
                field: "command_id",
                reason: "mismatch".to_string(),
            });
        }
        Ok(Self)
    }

    fn command_id() -> CommandId {
        CommandId::UNBIND_RESP
    }
}
