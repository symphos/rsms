use bytes::BytesMut;
use std::io::Cursor;

use super::CommandId;
use crate::codec::{CodecError, Decodable, Encodable, PduHeader};

#[derive(Clone, Debug, PartialEq)]
pub struct GenericNack;

impl GenericNack {
    pub fn new() -> Self {
        Self
    }
}

impl Default for GenericNack {
    fn default() -> Self {
        Self::new()
    }
}

impl Encodable for GenericNack {
    fn encode(&self, _buf: &mut BytesMut) -> Result<(), CodecError> {
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        0
    }
}

impl Decodable for GenericNack {
    fn decode(header: PduHeader, _buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if header.command_id != CommandId::GENERIC_NACK {
            return Err(CodecError::FieldValidation {
                field: "command_id",
                reason: "mismatch".to_string(),
            });
        }
        Ok(Self)
    }

    fn command_id() -> CommandId {
        CommandId::GENERIC_NACK
    }
}
