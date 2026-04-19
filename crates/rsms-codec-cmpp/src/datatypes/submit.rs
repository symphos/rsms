use bytes::{Buf, BufMut, BytesMut};
use rsms_core::{decode_pstring, encode_pstring};
use std::io::Cursor;

use crate::codec::{CodecError, Decodable, Encodable, PduHeader};
use crate::datatypes::CommandId;

/// CMPP Submit 请求（String 字段版）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submit {
    pub msg_id: [u8; 8],
    pub pk_total: u8,
    pub pk_number: u8,
    pub registered_delivery: u8,
    pub msg_level: u8,
    pub service_id: String,
    pub fee_user_type: u8,
    pub fee_terminal_id: String,
    pub fee_terminal_type: u8,
    pub tppid: u8,
    pub tpudhi: u8,
    pub msg_fmt: u8,
    pub msg_src: String,
    pub fee_type: String,
    pub fee_code: String,
    pub valid_time: String,
    pub at_time: String,
    pub src_id: String,
    pub dest_usr_tl: u8,
    pub dest_terminal_ids: Vec<String>,
    pub dest_terminal_type: u8,
    pub msg_content: Vec<u8>,
    pub link_id: String,
}

impl Submit {
    pub fn new() -> Self {
        Self {
            msg_id: [0u8; 8],
            pk_total: 1,
            pk_number: 1,
            registered_delivery: 0,
            msg_level: 1,
            service_id: String::new(),
            fee_user_type: 0,
            fee_terminal_id: String::new(),
            fee_terminal_type: 0,
            tppid: 0,
            tpudhi: 0,
            msg_fmt: 0,
            msg_src: String::new(),
            fee_type: String::new(),
            fee_code: String::new(),
            valid_time: String::new(),
            at_time: String::new(),
            src_id: String::new(),
            dest_usr_tl: 0,
            dest_terminal_ids: Vec::new(),
            dest_terminal_type: 0,
            msg_content: Vec::new(),
            link_id: String::new(),
        }
    }

    pub fn with_message(mut self, src: &str, dest: &str, content: &[u8]) -> Self {
        self.msg_src = src.to_string();
        if !dest.is_empty() {
            self.dest_terminal_ids.push(dest.to_string());
            self.dest_usr_tl = 1;
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
        buf.put_slice(&self.msg_id);
        buf.put_u8(self.pk_total);
        buf.put_u8(self.pk_number);
        buf.put_u8(self.registered_delivery);
        buf.put_u8(self.msg_level);
        encode_pstring(buf, &self.service_id, 10, "service_id").map_err(|e| {
            CodecError::FieldValidation {
                field: "service_id",
                reason: e,
            }
        })?;
        buf.put_u8(self.fee_user_type);
        encode_pstring(buf, &self.fee_terminal_id, 32, "fee_terminal_id").map_err(|e| {
            CodecError::FieldValidation {
                field: "fee_terminal_id",
                reason: e,
            }
        })?;
        buf.put_u8(self.fee_terminal_type);
        buf.put_u8(self.tppid);
        buf.put_u8(self.tpudhi);
        buf.put_u8(self.msg_fmt);
        encode_pstring(buf, &self.msg_src, 6, "msg_src").map_err(|e| {
            CodecError::FieldValidation {
                field: "msg_src",
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
        encode_pstring(buf, &self.src_id, 21, "src_id").map_err(|e| {
            CodecError::FieldValidation {
                field: "src_id",
                reason: e,
            }
        })?;
        buf.put_u8(self.dest_usr_tl);
        for dest_id in &self.dest_terminal_ids {
            encode_pstring(buf, dest_id, 32, "dest_terminal_id").map_err(|e| {
                CodecError::FieldValidation {
                    field: "dest_terminal_id",
                    reason: e,
                }
            })?;
        }
        buf.put_u8(self.dest_terminal_type);
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
        8 + 1
            + 1
            + 1
            + 1
            + 10
            + 1
            + 32
            + 1
            + 1
            + 1
            + 1
            + 6
            + 2
            + 6
            + 17
            + 17
            + 21
            + 1
            + (self.dest_terminal_ids.len() * 32)
            + 1
            + 1
            + self.msg_content.len()
            + 20
    }
}

impl Decodable for Submit {
    fn decode(header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        let body_len = (header.total_length - PduHeader::SIZE as u32) as usize;
        if buf.remaining() < body_len {
            return Err(CodecError::Incomplete);
        }

        let mut msg_id = [0u8; 8];
        buf.copy_to_slice(&mut msg_id);
        let pk_total = buf.get_u8();
        let pk_number = buf.get_u8();
        let registered_delivery = buf.get_u8();
        let msg_level = buf.get_u8();
        let service_id = decode_pstring(buf, 10).map_err(|_| CodecError::Incomplete)?;
        let fee_user_type = buf.get_u8();
        let fee_terminal_id = decode_pstring(buf, 32).map_err(|_| CodecError::Incomplete)?;
        let fee_terminal_type = buf.get_u8();
        let tppid = buf.get_u8();
        let tpudhi = buf.get_u8();
        let msg_fmt = buf.get_u8();
        let msg_src = decode_pstring(buf, 6).map_err(|_| CodecError::Incomplete)?;
        let fee_type = decode_pstring(buf, 2).map_err(|_| CodecError::Incomplete)?;
        let fee_code = decode_pstring(buf, 6).map_err(|_| CodecError::Incomplete)?;
        let valid_time = decode_pstring(buf, 17).map_err(|_| CodecError::Incomplete)?;
        let at_time = decode_pstring(buf, 17).map_err(|_| CodecError::Incomplete)?;
        let src_id = decode_pstring(buf, 21).map_err(|_| CodecError::Incomplete)?;
        let dest_usr_tl = buf.get_u8();

        let mut dest_terminal_ids = Vec::with_capacity(dest_usr_tl as usize);
        for _ in 0..dest_usr_tl {
            let dest_id = decode_pstring(buf, 32).map_err(|_| CodecError::Incomplete)?;
            dest_terminal_ids.push(dest_id);
        }

        let dest_terminal_type = buf.get_u8();
        let msg_length = buf.get_u8() as usize;
        let mut msg_content = vec![0u8; msg_length];
        buf.copy_to_slice(&mut msg_content);
        let link_id = decode_pstring(buf, 20).map_err(|_| CodecError::Incomplete)?;

        Ok(Submit {
            msg_id,
            pk_total,
            pk_number,
            registered_delivery,
            msg_level,
            service_id,
            fee_user_type,
            fee_terminal_id,
            fee_terminal_type,
            tppid,
            tpudhi,
            msg_fmt,
            msg_src,
            fee_type,
            fee_code,
            valid_time,
            at_time,
            src_id,
            dest_usr_tl,
            dest_terminal_ids,
            dest_terminal_type,
            msg_content,
            link_id,
        })
    }

    fn command_id() -> CommandId {
        CommandId::Submit
    }
}

/// CMPP Submit 响应
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitResp {
    pub msg_id: [u8; 8],
    pub result: u32,
}

impl SubmitResp {
    pub const BODY_SIZE: usize = 8 + 4;
}

impl Encodable for SubmitResp {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        buf.put_slice(&self.msg_id);
        buf.put_u32(self.result);
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        Self::BODY_SIZE
    }
}

impl Decodable for SubmitResp {
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
        Ok(SubmitResp { msg_id, result })
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
            msg_id: [0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0],
            result: 0,
        };
        let bytes = Pdu::from(resp.clone()).to_pdu_bytes(1);
        let decoded = decode_pdu::<SubmitResp>(&bytes.as_slice()).unwrap();
        assert_eq!(decoded.msg_id, resp.msg_id);
        assert_eq!(decoded.result, resp.result);
    }

    #[test]
    fn submit_roundtrip_single_dest() {
        let submit = Submit::new().with_message("9000", "13800138000", b"Hello");
        let bytes = Pdu::from(submit).to_pdu_bytes(10);
        let decoded = decode_pdu::<Submit>(&bytes.as_slice()).unwrap();
        assert_eq!(decoded.msg_src, "9000");
        assert_eq!(decoded.dest_usr_tl, 1);
        assert_eq!(decoded.dest_terminal_ids, vec!["13800138000"]);
        assert_eq!(decoded.msg_content, b"Hello");
    }

    #[test]
    fn submit_roundtrip_multi_dest() {
        let mut submit = Submit::new();
        submit.msg_src = "9000".to_string();
        submit.dest_terminal_ids.push("13800138000".to_string());
        submit.dest_terminal_ids.push("13900139000".to_string());
        submit.dest_usr_tl = 2;
        submit.msg_content = b"Test".to_vec();

        let bytes = Pdu::from(submit).to_pdu_bytes(11);
        let decoded = decode_pdu::<Submit>(&bytes.as_slice()).unwrap();
        assert_eq!(decoded.dest_usr_tl, 2);
        assert_eq!(decoded.dest_terminal_ids.len(), 2);
        assert_eq!(decoded.msg_content, b"Test");
    }

    #[test]
    fn submit_string_fields_decode_correctly() {
        let mut submit = Submit::new();
        submit.service_id = "SMS".to_string();
        submit.msg_src = "106900".to_string();
        submit.fee_type = "02".to_string();
        submit.fee_code = "000000".to_string();
        submit.src_id = "106900".to_string();
        submit.link_id = "ABC123".to_string();
        submit.msg_content = b"Hello".to_vec();

        let bytes = Pdu::from(submit).to_pdu_bytes(1);
        let decoded = decode_pdu::<Submit>(&bytes.as_slice()).unwrap();
        assert_eq!(decoded.service_id, "SMS");
        assert_eq!(decoded.msg_src, "106900");
        assert_eq!(decoded.fee_type, "02");
        assert_eq!(decoded.fee_code, "000000");
        assert_eq!(decoded.src_id, "106900");
        assert_eq!(decoded.link_id, "ABC123");
    }
}
