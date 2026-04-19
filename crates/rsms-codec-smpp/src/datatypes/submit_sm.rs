use bytes::{Buf, BufMut, BytesMut};
use std::io::Cursor;

use super::{CommandId, Tlv};
use crate::codec::{decode_cstring, encode_cstring, CodecError, Decodable, Encodable, PduHeader};

#[derive(Clone, Debug, PartialEq)]
pub struct SubmitSm {
    pub service_type: String,
    pub source_addr_ton: u8,
    pub source_addr_npi: u8,
    pub source_addr: String,
    pub dest_addr_ton: u8,
    pub dest_addr_npi: u8,
    pub destination_addr: String,
    pub esm_class: u8,
    pub protocol_id: u8,
    pub priority_flag: u8,
    pub schedule_delivery_time: String,
    pub validity_period: String,
    pub registered_delivery: u8,
    pub replace_if_present_flag: u8,
    pub data_coding: u8,
    pub sm_default_msg_id: u8,
    pub short_message: Vec<u8>,
    pub tlvs: Vec<Tlv>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SubmitSmResp {
    pub message_id: String,
}

impl SubmitSm {
    pub fn new() -> Self {
        Self {
            service_type: String::new(),
            source_addr_ton: 0,
            source_addr_npi: 0,
            source_addr: String::new(),
            dest_addr_ton: 0,
            dest_addr_npi: 0,
            destination_addr: String::new(),
            esm_class: 0,
            protocol_id: 0,
            priority_flag: 0,
            schedule_delivery_time: String::new(),
            validity_period: String::new(),
            registered_delivery: 0,
            replace_if_present_flag: 0,
            data_coding: 0,
            sm_default_msg_id: 0,
            short_message: Vec::new(),
            tlvs: Vec::new(),
        }
    }

    pub fn with_message(mut self, dest: &str, source: &str, message: &[u8]) -> Self {
        self.destination_addr = dest.to_string();
        self.source_addr = source.to_string();
        self.short_message = message.to_vec();
        self
    }
}

impl Default for SubmitSm {
    fn default() -> Self {
        Self::new()
    }
}

impl Encodable for SubmitSm {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        encode_cstring(buf, &self.service_type, 6)?;
        buf.put_u8(self.source_addr_ton);
        buf.put_u8(self.source_addr_npi);
        encode_cstring(buf, &self.source_addr, 21)?;
        buf.put_u8(self.dest_addr_ton);
        buf.put_u8(self.dest_addr_npi);
        encode_cstring(buf, &self.destination_addr, 21)?;
        buf.put_u8(self.esm_class);
        buf.put_u8(self.protocol_id);
        buf.put_u8(self.priority_flag);
        encode_cstring(buf, &self.schedule_delivery_time, 17)?;
        encode_cstring(buf, &self.validity_period, 17)?;
        buf.put_u8(self.registered_delivery);
        buf.put_u8(self.replace_if_present_flag);
        buf.put_u8(self.data_coding);
        buf.put_u8(self.sm_default_msg_id);
        buf.put_u8(self.short_message.len() as u8);
        buf.put_slice(&self.short_message);
        for tlv in &self.tlvs {
            tlv.encode(buf)?;
        }
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        let mut size = 6
            + 1
            + 1
            + 21
            + 1
            + 1
            + 21
            + 1
            + 1
            + 1
            + 17
            + 17
            + 1
            + 1
            + 1
            + 1
            + 1
            + self.short_message.len();
        for tlv in &self.tlvs {
            size += tlv.encoded_size();
        }
        size
    }
}

impl Decodable for SubmitSm {
    fn decode(header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if header.command_id != CommandId::SUBMIT_SM {
            return Err(CodecError::FieldValidation {
                field: "command_id",
                reason: "mismatch".to_string(),
            });
        }
        let service_type = decode_cstring(buf, 6, "service_type")?;
        let source_addr_ton = buf.get_u8();
        let source_addr_npi = buf.get_u8();
        let source_addr = decode_cstring(buf, 21, "source_addr")?;
        let dest_addr_ton = buf.get_u8();
        let dest_addr_npi = buf.get_u8();
        let destination_addr = decode_cstring(buf, 21, "destination_addr")?;
        let esm_class = buf.get_u8();
        let protocol_id = buf.get_u8();
        let priority_flag = buf.get_u8();
        let schedule_delivery_time = decode_cstring(buf, 17, "schedule_delivery_time")?;
        let validity_period = decode_cstring(buf, 17, "validity_period")?;
        let registered_delivery = buf.get_u8();
        let replace_if_present_flag = buf.get_u8();
        let data_coding = buf.get_u8();
        let sm_default_msg_id = buf.get_u8();
        let sm_length = buf.get_u8();
        let mut short_message = vec![0u8; sm_length as usize];
        buf.copy_to_slice(&mut short_message);

        let mut tlvs = Vec::new();
        while buf.has_remaining() {
            match Tlv::decode(buf) {
                Ok(tlv) => tlvs.push(tlv),
                Err(CodecError::Incomplete) => break,
                Err(e) => return Err(e),
            }
        }

        Ok(Self {
            service_type,
            source_addr_ton,
            source_addr_npi,
            source_addr,
            dest_addr_ton,
            dest_addr_npi,
            destination_addr,
            esm_class,
            protocol_id,
            priority_flag,
            schedule_delivery_time,
            validity_period,
            registered_delivery,
            replace_if_present_flag,
            data_coding,
            sm_default_msg_id,
            short_message,
            tlvs,
        })
    }

    fn command_id() -> CommandId {
        CommandId::SUBMIT_SM
    }
}

impl Encodable for SubmitSmResp {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        encode_cstring(buf, &self.message_id, 65)?;
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        65
    }
}

impl Decodable for SubmitSmResp {
    fn decode(header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if header.command_id != CommandId::SUBMIT_SM_RESP {
            return Err(CodecError::FieldValidation {
                field: "command_id",
                reason: "mismatch".to_string(),
            });
        }
        let message_id = decode_cstring(buf, 65, "message_id")?;
        Ok(Self { message_id })
    }

    fn command_id() -> CommandId {
        CommandId::SUBMIT_SM_RESP
    }
}
