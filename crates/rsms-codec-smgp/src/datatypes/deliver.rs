use bytes::{Buf, BufMut, BytesMut};
use rsms_core::{decode_pstring, encode_pstring};
use std::io::Cursor;

use crate::codec::{CodecError, Decodable, Encodable, PduHeader};
use crate::datatypes::tlv::OptionalParameters;
use crate::datatypes::CommandId;
use crate::datatypes::SmgpMsgId;

/// SMGP Deliver 请求（String 字段版）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deliver {
    pub msg_id: SmgpMsgId,
    pub is_report: u8,
    pub msg_fmt: u8,
    pub recv_time: String,
    pub src_term_id: String,
    pub dest_term_id: String,
    pub msg_content: Vec<u8>,
    pub reserve: [u8; 8],
    pub optional_params: OptionalParameters,
}

impl Deliver {
    pub fn new() -> Self {
        Self {
            msg_id: SmgpMsgId::default(),
            is_report: 0,
            msg_fmt: 0,
            recv_time: String::new(),
            src_term_id: String::new(),
            dest_term_id: String::new(),
            msg_content: Vec::new(),
            reserve: [0u8; 8],
            optional_params: OptionalParameters::new(),
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
        buf.put_slice(&self.msg_id.bytes);
        buf.put_u8(self.is_report);
        buf.put_u8(self.msg_fmt);
        encode_pstring(buf, &self.recv_time, 14, "recv_time").map_err(|e| {
            CodecError::FieldValidation {
                field: "recv_time",
                reason: e,
            }
        })?;
        encode_pstring(buf, &self.src_term_id, 21, "src_term_id").map_err(|e| {
            CodecError::FieldValidation {
                field: "src_term_id",
                reason: e,
            }
        })?;
        encode_pstring(buf, &self.dest_term_id, 21, "dest_term_id").map_err(|e| {
            CodecError::FieldValidation {
                field: "dest_term_id",
                reason: e,
            }
        })?;
        buf.put_u8(self.msg_content.len() as u8);
        buf.put_slice(&self.msg_content);
        buf.put_slice(&self.reserve);
        self.optional_params.encode(buf)?;
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        10 + 1
            + 1
            + 14
            + 21
            + 21
            + 1
            + self.msg_content.len()
            + 8
            + self.optional_params.encoded_size()
    }
}

impl Decodable for Deliver {
    fn decode(header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        let body_len = (header.total_length - PduHeader::SIZE as u32) as usize;
        if buf.remaining() < body_len {
            return Err(CodecError::Incomplete);
        }

        let mut msg_id_bytes = [0u8; 10];
        buf.copy_to_slice(&mut msg_id_bytes);
        let is_report = buf.get_u8();
        let msg_fmt = buf.get_u8();
        let recv_time = decode_pstring(buf, 14).map_err(|_| CodecError::Incomplete)?;
        let src_term_id = decode_pstring(buf, 21).map_err(|_| CodecError::Incomplete)?;
        let dest_term_id = decode_pstring(buf, 21).map_err(|_| CodecError::Incomplete)?;
        let msg_length = buf.get_u8() as usize;
        let mut msg_content = vec![0u8; msg_length];
        buf.copy_to_slice(&mut msg_content);
        let mut reserve = [0u8; 8];
        buf.copy_to_slice(&mut reserve);

        let optional_params = OptionalParameters::decode_remaining(buf)?;

        Ok(Deliver {
            msg_id: SmgpMsgId::new(msg_id_bytes),
            is_report,
            msg_fmt,
            recv_time,
            src_term_id,
            dest_term_id,
            msg_content,
            reserve,
            optional_params,
        })
    }

    fn command_id() -> CommandId {
        CommandId::Deliver
    }
}

/// SMGP 状态报告（从 Deliver 的 msg_content 解析，is_report=1）
/// 格式: id:XXXXXXXXXX sub:000 dlvrd:000 submit date:YYYYMMDDHH done date:YYYYMMDDHH stat:XXXXXXX err:000 text:XXXXXXXXXXXXXXXXXXXX
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmgpReport {
    pub msg_id: String,      // 10 bytes
    pub sub: String,         // 3 bytes, 子短信数
    pub dlvrd: String,       // 3 bytes, 成功数
    pub submit_time: String, // 10 bytes, YYYYMMDDHH
    pub done_time: String,   // 10 bytes, YYYYMMDDHH
    pub stat: String,        // 7 bytes, 状态
    pub err: String,         // 3 bytes, 错误码
    pub txt: String,         // 20 bytes, 附加文本
}

impl SmgpReport {
    /// SMGP 状态报告总长度 122 字节
    pub const LENGTH: usize = 122;

    /// 解析 SMGP 状态报告
    /// 格式: id:XXXXXXXXXX sub:000 dlvrd:000 submit date:YYYYMMDDHH done date:YYYYMMDDHH stat:XXXXXXX err:000 text:XXXXXXXXXXXXXXXXXXXX
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < Self::LENGTH {
            return None;
        }

        let s = std::str::from_utf8(&data[..Self::LENGTH]).ok()?;

        let msg_id = Self::extract_field(s, "id:")?;
        let rest = s.split_once(&format!("id:{} ", msg_id))?.1;

        let sub = Self::extract_field(rest, "sub:")?;
        let rest = rest.split_once(&format!("sub:{} ", sub))?.1;

        let dlvrd = Self::extract_field(rest, "dlvrd:")?;
        let rest = rest.split_once(&format!("dlvrd:{} ", dlvrd))?.1;

        let submit_time = Self::extract_field(rest, "submit date:")?;
        let rest = rest.split_once(&format!("submit date:{} ", submit_time))?.1;

        let done_time = Self::extract_field(rest, "done date:")?;
        let rest = rest.split_once(&format!("done date:{} ", done_time))?.1;

        let stat = Self::extract_field(rest, "stat:")?;
        let rest = rest.split_once(&format!("stat:{} ", stat))?.1;

        let err = Self::extract_field(rest, "err:")?;
        let txt = rest
            .split_once(&format!("err:{} text:", err))
            .map(|(_, t)| t.trim_end().to_string())
            .unwrap_or_default();

        Some(SmgpReport {
            msg_id,
            sub,
            dlvrd,
            submit_time,
            done_time,
            stat,
            err,
            txt,
        })
    }

    fn extract_field(s: &str, key: &str) -> Option<String> {
        let start = s.find(key)?;
        let value_start = start + key.len();
        let value_end = s[value_start..]
            .find(' ')
            .map(|p| value_start + p)
            .unwrap_or(s.len());
        Some(s[value_start..value_end].to_string())
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(Self::LENGTH);

        // id:
        bytes.extend_from_slice(b"id:");
        bytes.extend_from_slice(self.msg_id.as_bytes());
        bytes.push(b' ');

        //  sub:
        bytes.extend_from_slice(b"sub:");
        bytes.extend_from_slice(self.sub.as_bytes());
        bytes.push(b' ');

        //  dlvrd:
        bytes.extend_from_slice(b"dlvrd:");
        bytes.extend_from_slice(self.dlvrd.as_bytes());
        bytes.push(b' ');

        //  submit date:
        bytes.extend_from_slice(b"submit date:");
        bytes.extend_from_slice(self.submit_time.as_bytes());
        bytes.push(b' ');

        //  done date:
        bytes.extend_from_slice(b"done date:");
        bytes.extend_from_slice(self.done_time.as_bytes());
        bytes.push(b' ');

        //  stat:
        bytes.extend_from_slice(b"stat:");
        bytes.extend_from_slice(self.stat.as_bytes());
        bytes.push(b' ');

        //  err:
        bytes.extend_from_slice(b"err:");
        bytes.extend_from_slice(self.err.as_bytes());
        bytes.push(b' ');

        //  text:
        bytes.extend_from_slice(b"text:");
        bytes.extend_from_slice(self.txt.as_bytes());

        // Pad to LENGTH
        bytes.resize(Self::LENGTH, b' ');

        bytes
    }
}

/// SMGP Deliver 响应
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliverResp {
    pub status: u32,
}

impl DeliverResp {
    pub const BODY_SIZE: usize = 4;
}

impl Encodable for DeliverResp {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        buf.put_u32(self.status);
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        Self::BODY_SIZE
    }
}

impl Decodable for DeliverResp {
    fn decode(_header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if buf.remaining() < Self::BODY_SIZE {
            return Err(CodecError::Incomplete);
        }
        let status = buf.get_u32();
        Ok(DeliverResp { status })
    }

    fn command_id() -> CommandId {
        CommandId::DeliverResp
    }
}

/// SMGP Query 请求（String 字段版）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub query_type: u8,
    pub query_time: String,
    pub query_code: String,
    pub reserve: String,
}

impl Query {
    pub const BODY_SIZE: usize = 1 + 8 + 10 + 8;

    pub fn new() -> Self {
        Self {
            query_type: 0,
            query_time: String::new(),
            query_code: String::new(),
            reserve: String::new(),
        }
    }
}

impl Default for Query {
    fn default() -> Self {
        Self::new()
    }
}

impl Encodable for Query {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        buf.put_u8(self.query_type);
        encode_pstring(buf, &self.query_time, 8, "query_time").map_err(|e| {
            CodecError::FieldValidation {
                field: "query_time",
                reason: e,
            }
        })?;
        encode_pstring(buf, &self.query_code, 10, "query_code").map_err(|e| {
            CodecError::FieldValidation {
                field: "query_code",
                reason: e,
            }
        })?;
        encode_pstring(buf, &self.reserve, 8, "reserve").map_err(|e| {
            CodecError::FieldValidation {
                field: "reserve",
                reason: e,
            }
        })?;
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        Self::BODY_SIZE
    }
}

impl Decodable for Query {
    fn decode(_header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if buf.remaining() < Self::BODY_SIZE {
            return Err(CodecError::Incomplete);
        }

        let query_type = buf.get_u8();
        let query_time = decode_pstring(buf, 8).map_err(|_| CodecError::Incomplete)?;
        let query_code = decode_pstring(buf, 10).map_err(|_| CodecError::Incomplete)?;
        let reserve = decode_pstring(buf, 8).map_err(|_| CodecError::Incomplete)?;

        Ok(Query {
            query_type,
            query_time,
            query_code,
            reserve,
        })
    }

    fn command_id() -> CommandId {
        CommandId::Query
    }
}

/// SMGP Query 响应（String 字段版）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryResp {
    pub query_result: String,
    pub reserve: String,
}

impl QueryResp {
    pub const BODY_SIZE: usize = 10 + 8;

    pub fn new() -> Self {
        Self {
            query_result: String::new(),
            reserve: String::new(),
        }
    }
}

impl Default for QueryResp {
    fn default() -> Self {
        Self::new()
    }
}

impl Encodable for QueryResp {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        encode_pstring(buf, &self.query_result, 10, "query_result").map_err(|e| {
            CodecError::FieldValidation {
                field: "query_result",
                reason: e,
            }
        })?;
        encode_pstring(buf, &self.reserve, 8, "reserve").map_err(|e| {
            CodecError::FieldValidation {
                field: "reserve",
                reason: e,
            }
        })?;
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        Self::BODY_SIZE
    }
}

impl Decodable for QueryResp {
    fn decode(_header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if buf.remaining() < Self::BODY_SIZE {
            return Err(CodecError::Incomplete);
        }

        let query_result = decode_pstring(buf, 10).map_err(|_| CodecError::Incomplete)?;
        let reserve = decode_pstring(buf, 8).map_err(|_| CodecError::Incomplete)?;

        Ok(QueryResp {
            query_result,
            reserve,
        })
    }

    fn command_id() -> CommandId {
        CommandId::QueryResp
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
        let resp = DeliverResp { status: 0 };
        let bytes = Pdu::from(resp.clone()).to_pdu_bytes(1);
        let decoded = decode_pdu::<DeliverResp>(&bytes.as_slice()).unwrap();
        assert_eq!(decoded.status, 0);
    }

    #[test]
    fn deliver_mo_roundtrip() {
        let mut deliver = Deliver::new();
        deliver.is_report = 0;
        deliver.msg_fmt = 0;
        deliver.msg_content = b"MO message".to_vec();
        deliver.src_term_id = "13800138000".to_string();

        let bytes = Pdu::from(deliver.clone()).to_pdu_bytes(5);
        let decoded = decode_pdu::<Deliver>(&bytes.as_slice()).unwrap();
        assert_eq!(decoded.is_report, 0);
        assert_eq!(decoded.msg_content, b"MO message");
        assert_eq!(decoded.src_term_id, "13800138000");
    }

    #[test]
    fn deliver_string_fields_decode_correctly() {
        let mut deliver = Deliver::new();
        deliver.recv_time = "20260405120000".to_string();
        deliver.src_term_id = "13800138000".to_string();
        deliver.dest_term_id = "1065900000".to_string();
        deliver.msg_content = b"Hello".to_vec();

        let bytes = Pdu::from(deliver).to_pdu_bytes(1);
        let decoded = decode_pdu::<Deliver>(&bytes.as_slice()).unwrap();
        assert_eq!(decoded.recv_time, "20260405120000");
        assert_eq!(decoded.src_term_id, "13800138000");
        assert_eq!(decoded.dest_term_id, "1065900000");
    }

    #[test]
    fn query_roundtrip() {
        let query = Query {
            query_type: 0,
            query_time: "04051200".to_string(),
            query_code: "01".to_string(),
            reserve: String::new(),
        };
        let bytes = Pdu::from(query.clone()).to_pdu_bytes(1);
        let decoded = decode_pdu::<Query>(&bytes.as_slice()).unwrap();
        assert_eq!(decoded.query_type, 0);
        assert_eq!(decoded.query_code, "01");
    }

    #[test]
    fn query_resp_roundtrip() {
        let resp = QueryResp {
            query_result: "OK".to_string(),
            reserve: String::new(),
        };
        let bytes = Pdu::from(resp.clone()).to_pdu_bytes(2);
        let decoded = decode_pdu::<QueryResp>(&bytes.as_slice()).unwrap();
        assert_eq!(decoded.query_result, "OK");
    }

    #[test]
    fn smgp_report_parse() {
        // Test with bytes produced by to_bytes()
        let report = SmgpReport {
            msg_id: "0123456789".to_string(),
            sub: "001".to_string(),
            dlvrd: "001".to_string(),
            submit_time: "2026040512".to_string(),
            done_time: "2026040515".to_string(),
            stat: "DELIVRD".to_string(),
            err: "000".to_string(),
            txt: "Hello".to_string(),
        };

        // Test round-trip
        let encoded = report.to_bytes();
        let parsed = SmgpReport::parse(&encoded).expect("parse should succeed");
        assert_eq!(parsed.msg_id, "0123456789");
        assert_eq!(parsed.sub, "001");
        assert_eq!(parsed.dlvrd, "001");
        assert_eq!(parsed.submit_time, "2026040512");
        assert_eq!(parsed.done_time, "2026040515");
        assert_eq!(parsed.stat, "DELIVRD");
        assert_eq!(parsed.err, "000");
        assert_eq!(parsed.txt, "Hello");
    }

    #[test]
    fn smgp_report_to_bytes() {
        let report = SmgpReport {
            msg_id: "0123456789".to_string(),
            sub: "001".to_string(),
            dlvrd: "001".to_string(),
            submit_time: "2026040512".to_string(),
            done_time: "2026040515".to_string(),
            stat: "DELIVRD".to_string(),
            err: "000".to_string(),
            txt: "Hello".to_string(),
        };
        let bytes = report.to_bytes();
        assert_eq!(bytes.len(), SmgpReport::LENGTH);

        let parsed = SmgpReport::parse(&bytes).unwrap();
        assert_eq!(parsed.msg_id, "0123456789");
        assert_eq!(parsed.stat, "DELIVRD");
    }

    #[test]
    fn smgp_report_invalid() {
        let data = b"invalid";
        assert!(SmgpReport::parse(data).is_none());
    }
}
