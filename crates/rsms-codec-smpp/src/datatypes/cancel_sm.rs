use bytes::{Buf, BufMut, BytesMut};
use std::io::Cursor;

use super::CommandId;
use crate::codec::{decode_cstring, encode_cstring, CodecError, Decodable, Encodable, PduHeader};

#[derive(Clone, Debug, PartialEq)]
pub struct CancelSm {
    pub service_type: String,
    pub message_id: String,
    pub source_addr_ton: u8,
    pub source_addr_npi: u8,
    pub source_addr: String,
    pub dest_addr_ton: u8,
    pub dest_addr_npi: u8,
    pub destination_addr: String,
}

impl CancelSm {
    pub fn new(message_id: &str, dest_addr: &str) -> Self {
        Self {
            service_type: String::new(),
            message_id: message_id.to_string(),
            source_addr_ton: 0,
            source_addr_npi: 0,
            source_addr: String::new(),
            dest_addr_ton: 0,
            dest_addr_npi: 0,
            destination_addr: dest_addr.to_string(),
        }
    }
}

impl Encodable for CancelSm {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        encode_cstring(buf, &self.service_type, 6)?;
        encode_cstring(buf, &self.message_id, 65)?;
        buf.put_u8(self.source_addr_ton);
        buf.put_u8(self.source_addr_npi);
        encode_cstring(buf, &self.source_addr, 21)?;
        buf.put_u8(self.dest_addr_ton);
        buf.put_u8(self.dest_addr_npi);
        encode_cstring(buf, &self.destination_addr, 21)?;
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        6 + 65 + 1 + 1 + 21 + 1 + 1 + 21
    }
}

impl Decodable for CancelSm {
    fn decode(header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if header.command_id != CommandId::CANCEL_SM {
            return Err(CodecError::FieldValidation {
                field: "command_id",
                reason: "mismatch".to_string(),
            });
        }
        let service_type = decode_cstring(buf, 6, "service_type")?;
        let message_id = decode_cstring(buf, 65, "message_id")?;
        let source_addr_ton = buf.get_u8();
        let source_addr_npi = buf.get_u8();
        let source_addr = decode_cstring(buf, 21, "source_addr")?;
        let dest_addr_ton = buf.get_u8();
        let dest_addr_npi = buf.get_u8();
        let destination_addr = decode_cstring(buf, 21, "destination_addr")?;
        Ok(Self {
            service_type,
            message_id,
            source_addr_ton,
            source_addr_npi,
            source_addr,
            dest_addr_ton,
            dest_addr_npi,
            destination_addr,
        })
    }

    fn command_id() -> CommandId {
        CommandId::CANCEL_SM
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CancelSmResp;

impl Encodable for CancelSmResp {
    fn encode(&self, _buf: &mut BytesMut) -> Result<(), CodecError> {
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        0
    }
}

impl Decodable for CancelSmResp {
    fn decode(header: PduHeader, _buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if header.command_id != CommandId::CANCEL_SM_RESP {
            return Err(CodecError::FieldValidation {
                field: "command_id",
                reason: "mismatch".to_string(),
            });
        }
        Ok(Self)
    }

    fn command_id() -> CommandId {
        CommandId::CANCEL_SM_RESP
    }
}
