use bytes::{Buf, BufMut, BytesMut};
use rsms_core::{decode_pstring, encode_pstring};
use std::io::Cursor;

use crate::codec::{CodecError, Decodable, Encodable, PduHeader};
use crate::datatypes::CommandId;

/// SGIP Submit 请求（String 字段版）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submit {
    pub sp_number: String,
    pub charge_number: String,
    pub user_count: u8,
    pub user_numbers: Vec<String>,
    pub corp_id: String,
    pub service_type: String,
    pub fee_type: u8,
    pub fee_value: String,
    pub given_value: String,
    pub agent_flag: u8,
    pub morelate_to_mt_flag: u8,
    pub priority: u8,
    pub expire_time: String,
    pub schedule_time: String,
    pub report_flag: u8,
    pub tppid: u8,
    pub tpudhi: u8,
    pub msg_fmt: u8,
    pub message_type: u8,
    pub message_content: Vec<u8>,
    pub reserve: [u8; 8],
}

impl Submit {
    pub fn new() -> Self {
        Self {
            sp_number: String::new(),
            charge_number: String::new(),
            user_count: 0,
            user_numbers: Vec::new(),
            corp_id: String::new(),
            service_type: String::new(),
            fee_type: 0,
            fee_value: String::new(),
            given_value: String::new(),
            agent_flag: 0,
            morelate_to_mt_flag: 0,
            priority: 0,
            expire_time: String::new(),
            schedule_time: String::new(),
            report_flag: 0,
            tppid: 0,
            tpudhi: 0,
            msg_fmt: 0,
            message_type: 0,
            message_content: Vec::new(),
            reserve: [0u8; 8],
        }
    }

    pub fn with_message(mut self, sp: &str, user: &str, content: &[u8]) -> Self {
        self.sp_number = sp.to_string();
        if !user.is_empty() {
            self.user_numbers.push(user.to_string());
            self.user_count = 1;
        }
        self.message_content = content.to_vec();
        self
    }
}

impl Default for Submit {
    fn default() -> Self {
        Self::new()
    }
}

impl Encodable for Submit {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        encode_pstring(buf, &self.sp_number, 21, "sp_number").map_err(|e| {
            CodecError::FieldValidation {
                field: "sp_number",
                reason: e,
            }
        })?;
        encode_pstring(buf, &self.charge_number, 21, "charge_number").map_err(|e| {
            CodecError::FieldValidation {
                field: "charge_number",
                reason: e,
            }
        })?;
        buf.put_u8(self.user_count);
        for user_num in &self.user_numbers {
            encode_pstring(buf, user_num, 21, "user_number").map_err(|e| {
                CodecError::FieldValidation {
                    field: "user_number",
                    reason: e,
                }
            })?;
        }
        encode_pstring(buf, &self.corp_id, 5, "corp_id").map_err(|e| {
            CodecError::FieldValidation {
                field: "corp_id",
                reason: e,
            }
        })?;
        encode_pstring(buf, &self.service_type, 10, "service_type").map_err(|e| {
            CodecError::FieldValidation {
                field: "service_type",
                reason: e,
            }
        })?;
        buf.put_u8(self.fee_type);
        encode_pstring(buf, &self.fee_value, 6, "fee_value").map_err(|e| {
            CodecError::FieldValidation {
                field: "fee_value",
                reason: e,
            }
        })?;
        encode_pstring(buf, &self.given_value, 6, "given_value").map_err(|e| {
            CodecError::FieldValidation {
                field: "given_value",
                reason: e,
            }
        })?;
        buf.put_u8(self.agent_flag);
        buf.put_u8(self.morelate_to_mt_flag);
        buf.put_u8(self.priority);
        encode_pstring(buf, &self.expire_time, 16, "expire_time").map_err(|e| {
            CodecError::FieldValidation {
                field: "expire_time",
                reason: e,
            }
        })?;
        encode_pstring(buf, &self.schedule_time, 16, "schedule_time").map_err(|e| {
            CodecError::FieldValidation {
                field: "schedule_time",
                reason: e,
            }
        })?;
        buf.put_u8(self.report_flag);
        buf.put_u8(self.tppid);
        buf.put_u8(self.tpudhi);
        buf.put_u8(self.msg_fmt);
        buf.put_u8(self.message_type);
        buf.put_u32(self.message_content.len() as u32);
        buf.put_slice(&self.message_content);
        buf.put_slice(&self.reserve);
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        21 + 21
            + 1
            + (self.user_numbers.len() * 21)
            + 5
            + 10
            + 1
            + 6
            + 6
            + 1
            + 1
            + 1
            + 16
            + 16
            + 1
            + 1
            + 1
            + 1
            + 1
            + 4
            + self.message_content.len()
            + 8
    }
}

impl Decodable for Submit {
    fn decode(header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        let body_len = (header.total_length - PduHeader::SIZE as u32) as usize;
        if buf.remaining() < body_len {
            return Err(CodecError::Incomplete);
        }

        let sp_number = decode_pstring(buf, 21).map_err(|_| CodecError::Incomplete)?;
        let charge_number = decode_pstring(buf, 21).map_err(|_| CodecError::Incomplete)?;
        let user_count = buf.get_u8();

        let mut user_numbers = Vec::with_capacity(user_count as usize);
        for _ in 0..user_count {
            let user_num = decode_pstring(buf, 21).map_err(|_| CodecError::Incomplete)?;
            user_numbers.push(user_num);
        }

        let corp_id = decode_pstring(buf, 5).map_err(|_| CodecError::Incomplete)?;
        let service_type = decode_pstring(buf, 10).map_err(|_| CodecError::Incomplete)?;
        let fee_type = buf.get_u8();
        let fee_value = decode_pstring(buf, 6).map_err(|_| CodecError::Incomplete)?;
        let given_value = decode_pstring(buf, 6).map_err(|_| CodecError::Incomplete)?;
        let agent_flag = buf.get_u8();
        let morelate_to_mt_flag = buf.get_u8();
        let priority = buf.get_u8();
        let expire_time = decode_pstring(buf, 16).map_err(|_| CodecError::Incomplete)?;
        let schedule_time = decode_pstring(buf, 16).map_err(|_| CodecError::Incomplete)?;
        let report_flag = buf.get_u8();
        let tppid = buf.get_u8();
        let tpudhi = buf.get_u8();
        let msg_fmt = buf.get_u8();
        let message_type = buf.get_u8();
        let message_length = buf.get_u32() as usize;
        let mut message_content = vec![0u8; message_length];
        buf.copy_to_slice(&mut message_content);
        let mut reserve = [0u8; 8];
        buf.copy_to_slice(&mut reserve);

        Ok(Submit {
            sp_number,
            charge_number,
            user_count,
            user_numbers,
            corp_id,
            service_type,
            fee_type,
            fee_value,
            given_value,
            agent_flag,
            morelate_to_mt_flag,
            priority,
            expire_time,
            schedule_time,
            report_flag,
            tppid,
            tpudhi,
            msg_fmt,
            message_type,
            message_content,
            reserve,
        })
    }

    fn command_id() -> CommandId {
        CommandId::Submit
    }
}

/// SGIP Submit 响应
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitResp {
    pub result: u32,
}

impl SubmitResp {
    pub const BODY_SIZE: usize = 4;
}

impl Encodable for SubmitResp {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        buf.put_u32(self.result);
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        Self::BODY_SIZE
    }
}

impl Decodable for SubmitResp {
    fn decode(_header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if buf.remaining() < Self::BODY_SIZE {
            return Err(CodecError::Incomplete);
        }
        let result = buf.get_u32();
        Ok(SubmitResp { result })
    }

    fn command_id() -> CommandId {
        CommandId::SubmitResp
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datatypes::SgipSequence;

    fn decode_pdu<T: Decodable>(bytes: &[u8]) -> Result<T, CodecError> {
        let mut cursor = Cursor::new(bytes);
        let header = PduHeader::decode(&mut cursor)?;
        T::decode(header, &mut cursor)
    }

    #[test]
    fn submit_resp_roundtrip() {
        let resp = SubmitResp { result: 0 };
        let seq = SgipSequence::new(1, 0x04051200, 42);
        let mut buf = BytesMut::new();
        let header = PduHeader {
            total_length: 0,
            command_id: CommandId::SubmitResp,
            sequence: seq,
        };
        header.encode(&mut buf).unwrap();
        resp.encode(&mut buf).unwrap();
        let total = buf.len() as u32;
        buf[0..4].copy_from_slice(&total.to_be_bytes());
        let bytes = buf.to_vec();
        let decoded = decode_pdu::<SubmitResp>(&bytes).unwrap();
        assert_eq!(decoded.result, 0);
    }

    #[test]
    fn submit_roundtrip_single_user() {
        let submit = Submit::new().with_message("10655000000", "13800138000", b"Test");
        let bytes = submit.to_pdu_bytes(1, 0x04051200, 10);
        let decoded = decode_pdu::<Submit>(&bytes).unwrap();
        assert_eq!(decoded.user_count, 1);
        assert_eq!(decoded.sp_number, "10655000000");
        assert_eq!(decoded.user_numbers, vec!["13800138000"]);
        assert_eq!(decoded.message_content, b"Test");
    }

    #[test]
    fn submit_string_fields_decode_correctly() {
        let mut submit = Submit::new();
        submit.sp_number = "10655000000".to_string();
        submit.service_type = "SMS".to_string();
        submit.fee_value = "000100".to_string();
        submit.user_numbers.push("13800138000".to_string());
        submit.user_count = 1;
        submit.message_content = b"Hello".to_vec();

        let bytes = submit.to_pdu_bytes(1, 0x04051200, 11);
        let decoded = decode_pdu::<Submit>(&bytes).unwrap();
        assert_eq!(decoded.sp_number, "10655000000");
        assert_eq!(decoded.service_type, "SMS");
        assert_eq!(decoded.fee_value, "000100");
        assert_eq!(decoded.user_numbers, vec!["13800138000"]);
    }
}
