use bytes::{Buf, BufMut, BytesMut};
use rsms_core::{decode_pstring, encode_pstring};
use std::io::Cursor;

use crate::codec::{CodecError, Decodable, Encodable, PduHeader};
use crate::datatypes::tlv::OptionalParameters;
use crate::datatypes::CommandId;
use crate::datatypes::SmgpMsgId;

/// SMGP Submit 请求（String 字段版）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submit {
    pub msg_type: u8,
    pub need_report: u8,
    pub priority: u8,
    pub service_id: String,
    pub fee_type: String,
    pub fee_code: String,
    pub fixed_fee: String,
    pub msg_fmt: u8,
    pub valid_time: String,
    pub at_time: String,
    pub src_term_id: String,
    pub charge_term_id: String,
    pub dest_term_id_count: u8,
    pub dest_term_ids: Vec<String>,
    pub msg_content: Vec<u8>,
    pub reserve: [u8; 8],
    pub optional_params: OptionalParameters,
}

impl Submit {
    pub fn new() -> Self {
        Self {
            msg_type: 0,
            need_report: 0,
            priority: 0,
            service_id: String::new(),
            fee_type: String::new(),
            fee_code: String::new(),
            fixed_fee: String::new(),
            msg_fmt: 0,
            valid_time: String::new(),
            at_time: String::new(),
            src_term_id: String::new(),
            charge_term_id: String::new(),
            dest_term_id_count: 0,
            dest_term_ids: Vec::new(),
            msg_content: Vec::new(),
            reserve: [0u8; 8],
            optional_params: OptionalParameters::new(),
        }
    }

    pub fn with_message(mut self, src: &str, dest: &str, content: &[u8]) -> Self {
        self.src_term_id = src.to_string();
        if !dest.is_empty() {
            self.dest_term_ids.push(dest.to_string());
            self.dest_term_id_count = 1;
        }
        self.msg_content = content.to_vec();
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
        buf.put_u8(self.msg_type);
        buf.put_u8(self.need_report);
        buf.put_u8(self.priority);
        encode_pstring(buf, &self.service_id, 10, "service_id").map_err(|e| {
            CodecError::FieldValidation {
                field: "service_id",
                reason: e,
            }
        })?;
        encode_pstring(buf, &self.fee_type, 2, "fee_type").map_err(|e| {
            CodecError::FieldValidation {
                field: "fee_type",
                reason: e,
            }
        })?;
        encode_pstring(buf, &self.fee_code, 6, "fee_code").map_err(|e| {
            CodecError::FieldValidation {
                field: "fee_code",
                reason: e,
            }
        })?;
        encode_pstring(buf, &self.fixed_fee, 6, "fixed_fee").map_err(|e| {
            CodecError::FieldValidation {
                field: "fixed_fee",
                reason: e,
            }
        })?;
        buf.put_u8(self.msg_fmt);
        encode_pstring(buf, &self.valid_time, 17, "valid_time").map_err(|e| {
            CodecError::FieldValidation {
                field: "valid_time",
                reason: e,
            }
        })?;
        encode_pstring(buf, &self.at_time, 17, "at_time").map_err(|e| {
            CodecError::FieldValidation {
                field: "at_time",
                reason: e,
            }
        })?;
        encode_pstring(buf, &self.src_term_id, 21, "src_term_id").map_err(|e| {
            CodecError::FieldValidation {
                field: "src_term_id",
                reason: e,
            }
        })?;
        encode_pstring(buf, &self.charge_term_id, 21, "charge_term_id").map_err(|e| {
            CodecError::FieldValidation {
                field: "charge_term_id",
                reason: e,
            }
        })?;
        buf.put_u8(self.dest_term_id_count);
        for dest_id in &self.dest_term_ids {
            encode_pstring(buf, dest_id, 21, "dest_term_id").map_err(|e| {
                CodecError::FieldValidation {
                    field: "dest_term_id",
                    reason: e,
                }
            })?;
        }
        buf.put_u8(self.msg_content.len() as u8);
        buf.put_slice(&self.msg_content);
        buf.put_slice(&self.reserve);
        self.optional_params.encode(buf)?;
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        1 + 1
            + 1
            + 10
            + 2
            + 6
            + 6
            + 1
            + 17
            + 17
            + 21
            + 21
            + 1
            + (self.dest_term_ids.len() * 21)
            + 1
            + self.msg_content.len()
            + 8
            + self.optional_params.encoded_size()
    }
}

impl Decodable for Submit {
    fn decode(header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        let body_len = (header.total_length - PduHeader::SIZE as u32) as usize;
        if buf.remaining() < body_len {
            return Err(CodecError::Incomplete);
        }

        let msg_type = buf.get_u8();
        let need_report = buf.get_u8();
        let priority = buf.get_u8();
        let service_id = decode_pstring(buf, 10).map_err(|_| CodecError::Incomplete)?;
        let fee_type = decode_pstring(buf, 2).map_err(|_| CodecError::Incomplete)?;
        let fee_code = decode_pstring(buf, 6).map_err(|_| CodecError::Incomplete)?;
        let fixed_fee = decode_pstring(buf, 6).map_err(|_| CodecError::Incomplete)?;
        let msg_fmt = buf.get_u8();
        let valid_time = decode_pstring(buf, 17).map_err(|_| CodecError::Incomplete)?;
        let at_time = decode_pstring(buf, 17).map_err(|_| CodecError::Incomplete)?;
        let src_term_id = decode_pstring(buf, 21).map_err(|_| CodecError::Incomplete)?;
        let charge_term_id = decode_pstring(buf, 21).map_err(|_| CodecError::Incomplete)?;
        let dest_term_id_count = buf.get_u8();

        let mut dest_term_ids = Vec::with_capacity(dest_term_id_count as usize);
        for _ in 0..dest_term_id_count {
            let dest_id = decode_pstring(buf, 21).map_err(|_| CodecError::Incomplete)?;
            dest_term_ids.push(dest_id);
        }

        let msg_length = buf.get_u8() as usize;
        let mut msg_content = vec![0u8; msg_length];
        buf.copy_to_slice(&mut msg_content);
        let mut reserve = [0u8; 8];
        buf.copy_to_slice(&mut reserve);

        let optional_params = OptionalParameters::decode_remaining(buf)?;

        Ok(Submit {
            msg_type,
            need_report,
            priority,
            service_id,
            fee_type,
            fee_code,
            fixed_fee,
            msg_fmt,
            valid_time,
            at_time,
            src_term_id,
            charge_term_id,
            dest_term_id_count,
            dest_term_ids,
            msg_content,
            reserve,
            optional_params,
        })
    }

    fn command_id() -> CommandId {
        CommandId::Submit
    }
}

/// SMGP Submit 响应
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitResp {
    pub msg_id: SmgpMsgId,
    pub status: u32,
}

impl SubmitResp {
    pub const BODY_SIZE: usize = 10 + 4;
}

impl Encodable for SubmitResp {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        buf.put_slice(&self.msg_id.bytes);
        buf.put_u32(self.status);
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
        let mut msg_id_bytes = [0u8; 10];
        buf.copy_to_slice(&mut msg_id_bytes);
        let status = buf.get_u32();
        Ok(SubmitResp {
            msg_id: SmgpMsgId::new(msg_id_bytes),
            status,
        })
    }

    fn command_id() -> CommandId {
        CommandId::SubmitResp
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
    fn submit_resp_roundtrip() {
        let resp = SubmitResp {
            msg_id: SmgpMsgId::from_u64(0x123456789ABCDEF0),
            status: 0,
        };
        let bytes = Pdu::from(resp.clone()).to_pdu_bytes(1);
        let decoded = decode_pdu::<SubmitResp>(&bytes.as_slice()).unwrap();
        assert_eq!(decoded.msg_id, resp.msg_id);
        assert_eq!(decoded.status, 0);
    }

    #[test]
    fn submit_roundtrip_single_dest() {
        let submit = Submit::new().with_message("1065900000", "13800138000", b"Hello");
        let bytes = Pdu::from(submit.clone()).to_pdu_bytes(10);
        let decoded = decode_pdu::<Submit>(&bytes.as_slice()).unwrap();
        assert_eq!(decoded.dest_term_id_count, 1);
        assert_eq!(decoded.src_term_id, "1065900000");
        assert_eq!(decoded.dest_term_ids, vec!["13800138000"]);
        assert_eq!(decoded.msg_content, b"Hello");
    }

    #[test]
    fn submit_string_fields_decode_correctly() {
        let mut submit = Submit::new();
        submit.service_id = "SMS".to_string();
        submit.fee_type = "02".to_string();
        submit.fee_code = "000000".to_string();
        submit.src_term_id = "1065900000".to_string();
        submit.dest_term_ids.push("13800138000".to_string());
        submit.dest_term_id_count = 1;
        submit.msg_content = b"Hello".to_vec();

        let bytes = Pdu::from(submit).to_pdu_bytes(1);
        let decoded = decode_pdu::<Submit>(&bytes.as_slice()).unwrap();
        assert_eq!(decoded.service_id, "SMS");
        assert_eq!(decoded.fee_type, "02");
        assert_eq!(decoded.fee_code, "000000");
        assert_eq!(decoded.src_term_id, "1065900000");
        assert_eq!(decoded.dest_term_ids, vec!["13800138000"]);
    }
}
