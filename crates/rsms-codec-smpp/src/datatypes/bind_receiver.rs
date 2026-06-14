use bytes::{Buf, BufMut, BytesMut};
use std::io::Cursor;

use super::CommandId;
use crate::codec::{decode_cstring, encode_cstring, CodecError, Decodable, Encodable, PduHeader};

pub const MAX_SYSTEM_ID_LENGTH: usize = 16;
pub const MAX_PASSWORD_LENGTH: usize = 9;
pub const MAX_SYSTEM_TYPE_LENGTH: usize = 13;

#[derive(Clone, Debug, PartialEq)]
pub struct BindReceiver {
    pub system_id: String,
    pub password: String,
    pub system_type: String,
    pub interface_version: u8,
    pub addr_ton: u8,
    pub addr_npi: u8,
    pub address_range: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BindReceiverResp {
    pub system_id: String,
    pub sc_interface_version: u8,
}

impl BindReceiver {
    pub fn new(system_id: &str, password: &str, system_type: &str, interface_version: u8) -> Self {
        Self {
            system_id: system_id.to_string(),
            password: password.to_string(),
            system_type: system_type.to_string(),
            interface_version,
            addr_ton: 0,
            addr_npi: 0,
            address_range: String::new(),
        }
    }
}

impl Encodable for BindReceiver {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        encode_cstring(buf, &self.system_id, MAX_SYSTEM_ID_LENGTH)?;
        encode_cstring(buf, &self.password, MAX_PASSWORD_LENGTH)?;
        encode_cstring(buf, &self.system_type, MAX_SYSTEM_TYPE_LENGTH)?;
        buf.put_u8(self.interface_version);
        buf.put_u8(self.addr_ton);
        buf.put_u8(self.addr_npi);
        encode_cstring(buf, &self.address_range, 41)?;
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        MAX_SYSTEM_ID_LENGTH + MAX_PASSWORD_LENGTH + MAX_SYSTEM_TYPE_LENGTH + 1 + 1 + 1 + 41
    }
}

impl Decodable for BindReceiver {
    fn decode(header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if header.command_id != CommandId::BIND_RECEIVER {
            return Err(CodecError::FieldValidation {
                field: "command_id",
                reason: "mismatch".to_string(),
            });
        }
        let system_id = decode_cstring(buf, MAX_SYSTEM_ID_LENGTH, "system_id")?;
        let password = decode_cstring(buf, MAX_PASSWORD_LENGTH, "password")?;
        let system_type = decode_cstring(buf, MAX_SYSTEM_TYPE_LENGTH, "system_type")?;
        let interface_version = buf.try_get_u8().map_err(|_| CodecError::Incomplete)?;
        let addr_ton = buf.try_get_u8().map_err(|_| CodecError::Incomplete)?;
        let addr_npi = buf.try_get_u8().map_err(|_| CodecError::Incomplete)?;
        let address_range = decode_cstring(buf, 41, "address_range")?;
        Ok(Self {
            system_id,
            password,
            system_type,
            interface_version,
            addr_ton,
            addr_npi,
            address_range,
        })
    }

    fn command_id() -> CommandId {
        CommandId::BIND_RECEIVER
    }
}

impl Encodable for BindReceiverResp {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        encode_cstring(buf, &self.system_id, MAX_SYSTEM_ID_LENGTH)?;
        // sc_interface_version 是 SMPP 可选参数 TLV(0x0210, len 1)，不是裸字节（见 bind_transceiver 同修）。
        buf.put_u16(0x0210);
        buf.put_u16(1);
        buf.put_u8(self.sc_interface_version);
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        MAX_SYSTEM_ID_LENGTH + 5
    }
}

impl Decodable for BindReceiverResp {
    fn decode(header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if header.command_id != CommandId::BIND_RECEIVER_RESP {
            return Err(CodecError::FieldValidation {
                field: "command_id",
                reason: "mismatch".to_string(),
            });
        }
        let system_id = decode_cstring(buf, MAX_SYSTEM_ID_LENGTH, "system_id")?;
        // sc_interface_version 作为可选 TLV(0x0210, len 1) 解析；对端可省略，省略则默认 0。
        let sc_interface_version = if buf.remaining() >= 5 {
            let _tag = buf.try_get_u16().map_err(|_| CodecError::Incomplete)?;
            let _len = buf.try_get_u16().map_err(|_| CodecError::Incomplete)?;
            buf.try_get_u8().map_err(|_| CodecError::Incomplete)?
        } else {
            0
        };
        Ok(Self {
            system_id,
            sc_interface_version,
        })
    }

    fn command_id() -> CommandId {
        CommandId::BIND_RECEIVER_RESP
    }
}
