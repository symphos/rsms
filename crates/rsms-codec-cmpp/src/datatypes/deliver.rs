use bytes::{Buf, BufMut, BytesMut};
use rsms_core::{decode_pstring, encode_pstring};
use std::io::Cursor;

use crate::codec::{CodecError, Decodable, Encodable, PduHeader};
use crate::datatypes::CommandId;

/// CMPP Deliver 请求（String 字段版）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deliver {
    pub msg_id: [u8; 8],
    pub dest_id: String,
    pub service_id: String,
    pub tppid: u8,
    pub tpudhi: u8,
    pub msg_fmt: u8,
    pub src_terminal_id: String,
    pub src_terminal_type: u8,
    pub registered_delivery: u8,
    pub msg_content: Vec<u8>,
    pub link_id: String,
}

impl Deliver {
    pub fn new() -> Self {
        Self {
            msg_id: [0u8; 8],
            dest_id: String::new(),
            service_id: String::new(),
            tppid: 0,
            tpudhi: 0,
            msg_fmt: 0,
            src_terminal_id: String::new(),
            src_terminal_type: 0,
            registered_delivery: 0,
            msg_content: Vec::new(),
            link_id: String::new(),
        }
    }
}

impl Default for Deliver {
    fn default() -> Self {
        Self::new()
    }
}

impl Encodable for Deliver {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        buf.put_slice(&self.msg_id);
        encode_pstring(buf, &self.dest_id, 21, "dest_id").map_err(|e| {
            CodecError::FieldValidation {
                field: "dest_id",
                reason: e,
            }
        })?;
        encode_pstring(buf, &self.service_id, 10, "service_id").map_err(|e| {
            CodecError::FieldValidation {
                field: "service_id",
                reason: e,
            }
        })?;
        buf.put_u8(self.tppid);
        buf.put_u8(self.tpudhi);
        buf.put_u8(self.msg_fmt);
        encode_pstring(buf, &self.src_terminal_id, 32, "src_terminal_id").map_err(|e| {
            CodecError::FieldValidation {
                field: "src_terminal_id",
                reason: e,
            }
        })?;
        buf.put_u8(self.src_terminal_type);
        buf.put_u8(self.registered_delivery);
        buf.put_u8(self.msg_content.len() as u8);
        buf.put_slice(&self.msg_content);
        encode_pstring(buf, &self.link_id, 20, "link_id").map_err(|e| {
            CodecError::FieldValidation {
                field: "link_id",
                reason: e,
            }
        })?;
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        8 + 21 + 10 + 1 + 1 + 1 + 32 + 1 + 1 + 1 + self.msg_content.len() + 20
    }
}

impl Decodable for Deliver {
    fn decode(header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        let body_len = (header.total_length - PduHeader::SIZE as u32) as usize;
        if buf.remaining() < body_len {
            return Err(CodecError::Incomplete);
        }

        let mut msg_id = [0u8; 8];
        buf.copy_to_slice(&mut msg_id);
        let dest_id = decode_pstring(buf, 21).map_err(|_| CodecError::Incomplete)?;
        let service_id = decode_pstring(buf, 10).map_err(|_| CodecError::Incomplete)?;
        let tppid = buf.get_u8();
        let tpudhi = buf.get_u8();
        let msg_fmt = buf.get_u8();
        let src_terminal_id = decode_pstring(buf, 32).map_err(|_| CodecError::Incomplete)?;
        let src_terminal_type = buf.get_u8();
        let registered_delivery = buf.get_u8();
        let msg_length = buf.get_u8() as usize;
        let mut msg_content = vec![0u8; msg_length];
        buf.copy_to_slice(&mut msg_content);
        let link_id = decode_pstring(buf, 20).map_err(|_| CodecError::Incomplete)?;

        Ok(Deliver {
            msg_id,
            dest_id,
            service_id,
            tppid,
            tpudhi,
            msg_fmt,
            src_terminal_id,
            src_terminal_type,
            registered_delivery,
            msg_content,
            link_id,
        })
    }

    fn command_id() -> CommandId {
        CommandId::Deliver
    }
}

/// CMPP Deliver 响应
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliverResp {
    pub msg_id: [u8; 8],
    pub result: u32,
}

impl DeliverResp {
    pub const BODY_SIZE: usize = 8 + 4;
}

impl Encodable for DeliverResp {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        buf.put_slice(&self.msg_id);
        buf.put_u32(self.result);
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        Self::BODY_SIZE
    }
}

impl Decodable for DeliverResp {
    fn decode(header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if header.total_length != (PduHeader::SIZE + Self::BODY_SIZE) as u32 {
            return Err(CodecError::InvalidPduLength {
                length: header.total_length,
                min: (PduHeader::SIZE + Self::BODY_SIZE) as u32,
                max: (PduHeader::SIZE + Self::BODY_SIZE) as u32,
            });
        }
        if buf.remaining() < Self::BODY_SIZE {
            return Err(CodecError::Incomplete);
        }
        let mut msg_id = [0u8; 8];
        buf.copy_to_slice(&mut msg_id);
        let result = buf.get_u32();
        Ok(DeliverResp { msg_id, result })
    }

    fn command_id() -> CommandId {
        CommandId::DeliverResp
    }
}

/// CMPP 状态报告（从 Deliver 的 msg_content 解析）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmppReport {
    pub msg_id: String,
    pub stat: String,
    pub submit_time: String,
    pub done_time: String,
    pub dest_terminal_id: String,
    pub smsc_sequence: u32,
}

impl CmppReport {
    pub fn parse(report_str: &str) -> Option<Self> {
        let parts: Vec<&str> = report_str.split_whitespace().collect();
        if parts.len() < 6 {
            return None;
        }

        let mut msg_id = String::new();
        let mut stat = String::new();
        let mut submit_time = String::new();
        let mut done_time = String::new();
        let mut dest_terminal_id = String::new();
        let mut smsc_sequence = 0u32;

        for part in &parts {
            if let Some((key, value)) = part.split_once(':') {
                match key {
                    "MsgId" => msg_id = value.to_string(),
                    "Stat" => stat = value.to_string(),
                    "SubmitTime" => submit_time = value.to_string(),
                    "DoneTime" => done_time = value.to_string(),
                    "DestTerminalId" => dest_terminal_id = value.to_string(),
                    "SMSCSequence" => smsc_sequence = value.parse().ok()?,
                    _ => {}
                }
            }
        }

        if msg_id.is_empty() || stat.is_empty() {
            return None;
        }

        Some(CmppReport {
            msg_id,
            stat,
            submit_time,
            done_time,
            dest_terminal_id,
            smsc_sequence,
        })
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
    fn deliver_resp_roundtrip() {
        let resp = DeliverResp {
            msg_id: [0x87, 0x65, 0x43, 0x21, 0x00, 0x00, 0x00, 0x00],
            result: 0,
        };
        let bytes = Pdu::from(resp.clone()).to_pdu_bytes(1);
        let decoded = decode_pdu::<DeliverResp>(&bytes.as_slice()).unwrap();
        assert_eq!(decoded.msg_id, resp.msg_id);
        assert_eq!(decoded.result, resp.result);
    }

    #[test]
    fn deliver_mo_roundtrip() {
        let mut deliver = Deliver::new();
        deliver.msg_fmt = 0;
        deliver.registered_delivery = 0;
        deliver.msg_content = b"MO reply".to_vec();
        deliver.src_terminal_id = "13800138000".to_string();

        let bytes = Pdu::from(deliver).to_pdu_bytes(5);
        let decoded = decode_pdu::<Deliver>(&bytes.as_slice()).unwrap();
        assert_eq!(decoded.registered_delivery, 0);
        assert_eq!(decoded.msg_content, b"MO reply");
        assert_eq!(decoded.src_terminal_id, "13800138000");
    }

    #[test]
    fn deliver_string_fields_decode_correctly() {
        let mut deliver = Deliver::new();
        deliver.dest_id = "10655000000".to_string();
        deliver.service_id = "SMS".to_string();
        deliver.src_terminal_id = "13800138000".to_string();
        deliver.link_id = "ABC123".to_string();
        deliver.msg_content = b"Hello".to_vec();

        let bytes = Pdu::from(deliver).to_pdu_bytes(1);
        let decoded = decode_pdu::<Deliver>(&bytes.as_slice()).unwrap();
        assert_eq!(decoded.dest_id, "10655000000");
        assert_eq!(decoded.service_id, "SMS");
        assert_eq!(decoded.src_terminal_id, "13800138000");
        assert_eq!(decoded.link_id, "ABC123");
    }

    #[test]
    fn deliver_status_report_parse() {
        let report_str = "MsgId:12345678 Stat:DELIVRD SubmitTime:0405120000 DoneTime:0405120100 DestTerminalId:13800138000 SMSCSequence:42";
        let report = CmppReport::parse(report_str).unwrap();
        assert_eq!(report.msg_id, "12345678");
        assert_eq!(report.stat, "DELIVRD");
        assert_eq!(report.smsc_sequence, 42);
    }

    #[test]
    fn deliver_status_report_invalid() {
        let report_str = "invalid";
        assert!(CmppReport::parse(report_str).is_none());
    }
}
