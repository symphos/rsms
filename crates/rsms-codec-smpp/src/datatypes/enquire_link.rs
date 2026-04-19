use bytes::BytesMut;
use std::io::Cursor;

use super::CommandId;
use crate::codec::{CodecError, Decodable, Encodable, PduHeader};

#[derive(Clone, Debug, PartialEq)]
pub struct EnquireLink;

#[derive(Clone, Debug, PartialEq)]
pub struct EnquireLinkResp;

impl EnquireLink {
    pub fn new() -> Self {
        Self
    }
}

impl Default for EnquireLink {
    fn default() -> Self {
        Self::new()
    }
}

impl Encodable for EnquireLink {
    fn encode(&self, _buf: &mut BytesMut) -> Result<(), CodecError> {
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        0
    }
}

impl Decodable for EnquireLink {
    fn decode(header: PduHeader, _buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if header.command_id != CommandId::ENQUIRE_LINK {
            return Err(CodecError::FieldValidation {
                field: "command_id",
                reason: "mismatch".to_string(),
            });
        }
        Ok(Self)
    }

    fn command_id() -> CommandId {
        CommandId::ENQUIRE_LINK
    }
}

impl Encodable for EnquireLinkResp {
    fn encode(&self, _buf: &mut BytesMut) -> Result<(), CodecError> {
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        0
    }
}

impl Decodable for EnquireLinkResp {
    fn decode(header: PduHeader, _buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if header.command_id != CommandId::ENQUIRE_LINK_RESP {
            return Err(CodecError::FieldValidation {
                field: "command_id",
                reason: "mismatch".to_string(),
            });
        }
        Ok(Self)
    }

    fn command_id() -> CommandId {
        CommandId::ENQUIRE_LINK_RESP
    }
}
