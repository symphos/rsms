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
        if self.message_content.len() > u32::MAX as usize {
            return Err(CodecError::FieldValidation {
                field: "message_content",
                reason: format!(
                    "正文长度 {} 超过 MessageLength(u32) 上限 {}",
                    self.message_content.len(),
                    u32::MAX
                ),
            });
        }
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
        let user_count = buf.try_get_u8().map_err(|_| CodecError::Incomplete)?;

        let mut user_numbers = Vec::with_capacity(user_count as usize);
        for _ in 0..user_count {
            let user_num = decode_pstring(buf, 21).map_err(|_| CodecError::Incomplete)?;
            user_numbers.push(user_num);
        }

        let corp_id = decode_pstring(buf, 5).map_err(|_| CodecError::Incomplete)?;
        let service_type = decode_pstring(buf, 10).map_err(|_| CodecError::Incomplete)?;
        let fee_type = buf.try_get_u8().map_err(|_| CodecError::Incomplete)?;
        let fee_value = decode_pstring(buf, 6).map_err(|_| CodecError::Incomplete)?;
        let given_value = decode_pstring(buf, 6).map_err(|_| CodecError::Incomplete)?;
        let agent_flag = buf.try_get_u8().map_err(|_| CodecError::Incomplete)?;
        let morelate_to_mt_flag = buf.try_get_u8().map_err(|_| CodecError::Incomplete)?;
        let priority = buf.try_get_u8().map_err(|_| CodecError::Incomplete)?;
        let expire_time = decode_pstring(buf, 16).map_err(|_| CodecError::Incomplete)?;
        let schedule_time = decode_pstring(buf, 16).map_err(|_| CodecError::Incomplete)?;
        let report_flag = buf.try_get_u8().map_err(|_| CodecError::Incomplete)?;
        let tppid = buf.try_get_u8().map_err(|_| CodecError::Incomplete)?;
        let tpudhi = buf.try_get_u8().map_err(|_| CodecError::Incomplete)?;
        let msg_fmt = buf.try_get_u8().map_err(|_| CodecError::Incomplete)?;
        let message_type = buf.try_get_u8().map_err(|_| CodecError::Incomplete)?;
        let message_length = buf.try_get_u32().map_err(|_| CodecError::Incomplete)? as usize;
        if buf.remaining() < message_length {
            return Err(CodecError::Incomplete);
        }
        let mut message_content = vec![0u8; message_length];
        buf.try_copy_to_slice(&mut message_content)
            .map_err(|_| CodecError::Incomplete)?;
        if buf.remaining() < 8 {
            return Err(CodecError::Incomplete);
        }
        let mut reserve = [0u8; 8];
        buf.try_copy_to_slice(&mut reserve)
            .map_err(|_| CodecError::Incomplete)?;

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
    // SGIP 1.2 Submit_Resp body = Result(1B) + Reserve(8B) = 9B。
    // 旧版误用 Result=u32(4B) 且无 Reserve，导致第三方(cmos)解码越界（读 1B result 后只剩 3B 无法读 8B reserve）。
    pub const BODY_SIZE: usize = 1 + 8;
}

impl Encodable for SubmitResp {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        buf.put_u8(self.result as u8); // Result 1 字节（SGIP 结果码 0=成功）
        buf.put_slice(&[0u8; 8]); // Reserve 8 字节
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
        let result = buf.try_get_u8().map_err(|_| CodecError::Incomplete)? as u32;
        let mut reserve = [0u8; 8];
        buf.try_copy_to_slice(&mut reserve).map_err(|_| CodecError::Incomplete)?;
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

    #[test]
    fn encode_oversized_msg_content_ok_u32_field() {
        // 4A.6：MessageLength 是 u32，256 字节正文远在上限内，encode 应正常 Ok
        // （不误伤回归；u32 域容得下，无需像 u8 协议那样报错）。
        let submit = Submit::new().with_message("10655000000", "13800138000", &vec![0x41u8; 256]);
        let mut buf = BytesMut::new();
        assert!(submit.encode(&mut buf).is_ok());
    }

    #[test]
    fn submit_decode_oversized_message_length_is_err_not_panic() {
        // 回归（P0.2）：message_length 是 u32，谎报超大值（如 u32::MAX）时既不能
        // 预分配 ~4GB，也不能在 copy_to_slice 处 panic —— 必须返回 Err。
        // total_length 不变 ⇒ body_len 守卫通过，命中长度校验。
        let marker = b"SGIP_RSMS_MARKER";
        let submit = Submit::new().with_message("10655000000", "13800138000", marker);
        let mut bytes = submit.to_pdu_bytes(1, 0x04051200, 10).to_vec();
        let pos = bytes
            .windows(marker.len())
            .position(|w| w == marker)
            .expect("marker present");
        // message_length 是内容前的大端 u32
        bytes[pos - 4..pos].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(decode_pdu::<Submit>(&bytes).is_err());
    }
}
