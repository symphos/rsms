use bytes::{Buf, BufMut, BytesMut};
use rsms_core::{decode_pstring, encode_pstring};
use std::io::Cursor;

use crate::codec::{CodecError, Decodable, Encodable, PduHeader};
use crate::datatypes::CommandId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deliver {
    pub user_number: String,
    pub sp_number: String,
    pub tppid: u8,
    pub tpudhi: u8,
    pub msg_fmt: u8,
    pub message_content: Vec<u8>,
    pub reserve: [u8; 8],
}

impl Deliver {
    pub fn new() -> Self {
        Self {
            user_number: String::new(),
            sp_number: String::new(),
            tppid: 0,
            tpudhi: 0,
            msg_fmt: 0,
            message_content: Vec::new(),
            reserve: [0u8; 8],
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
        encode_pstring(buf, &self.user_number, 21, "user_number").map_err(|e| {
            CodecError::FieldValidation {
                field: "user_number",
                reason: e,
            }
        })?;
        encode_pstring(buf, &self.sp_number, 21, "sp_number").map_err(|e| {
            CodecError::FieldValidation {
                field: "sp_number",
                reason: e,
            }
        })?;
        buf.put_u8(self.tppid);
        buf.put_u8(self.tpudhi);
        buf.put_u8(self.msg_fmt);
        buf.put_u32(self.message_content.len() as u32);
        buf.put_slice(&self.message_content);
        buf.put_slice(&self.reserve);
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        21 + 21 + 1 + 1 + 1 + 4 + self.message_content.len() + 8
    }
}

impl Decodable for Deliver {
    fn decode(header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        let body_len = (header.total_length - PduHeader::SIZE as u32) as usize;
        if buf.remaining() < body_len {
            return Err(CodecError::Incomplete);
        }

        let user_number = decode_pstring(buf, 21).map_err(|_| CodecError::Incomplete)?;
        let sp_number = decode_pstring(buf, 21).map_err(|_| CodecError::Incomplete)?;
        let tppid = buf.get_u8();
        let tpudhi = buf.get_u8();
        let msg_fmt = buf.get_u8();
        let message_length = buf.get_u32() as usize;
        let mut message_content = vec![0u8; message_length];
        buf.copy_to_slice(&mut message_content);
        let mut reserve = [0u8; 8];
        buf.copy_to_slice(&mut reserve);

        Ok(Deliver {
            user_number,
            sp_number,
            tppid,
            tpudhi,
            msg_fmt,
            message_content,
            reserve,
        })
    }

    fn command_id() -> CommandId {
        CommandId::Deliver
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliverResp {
    pub result: u32,
}

impl DeliverResp {
    pub const BODY_SIZE: usize = 4;
}

impl Encodable for DeliverResp {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        buf.put_u32(self.result);
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
        let result = buf.get_u32();
        Ok(DeliverResp { result })
    }

    fn command_id() -> CommandId {
        CommandId::DeliverResp
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub submit_sequence: crate::datatypes::SgipSequence,
    pub report_type: u8,
    pub user_number: String,
    pub state: u8,
    pub error_code: u8,
    pub reserve: [u8; 8],
}

impl Report {
    pub const BODY_SIZE: usize = 12 + 1 + 21 + 1 + 1 + 8;
}

impl Encodable for Report {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        self.submit_sequence.encode(buf);
        buf.put_u8(self.report_type);
        encode_pstring(buf, &self.user_number, 21, "user_number").map_err(|e| {
            CodecError::FieldValidation {
                field: "user_number",
                reason: e,
            }
        })?;
        buf.put_u8(self.state);
        buf.put_u8(self.error_code);
        buf.put_slice(&self.reserve);
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        Self::BODY_SIZE
    }
}

impl Decodable for Report {
    fn decode(_header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if buf.remaining() < Self::BODY_SIZE {
            return Err(CodecError::Incomplete);
        }
        let submit_sequence =
            crate::datatypes::SgipSequence::decode(buf).map_err(|_| CodecError::Incomplete)?;
        let report_type = buf.get_u8();
        let user_number = decode_pstring(buf, 21).map_err(|_| CodecError::Incomplete)?;
        let state = buf.get_u8();
        let error_code = buf.get_u8();
        let mut reserve = [0u8; 8];
        buf.copy_to_slice(&mut reserve);
        Ok(Report {
            submit_sequence,
            report_type,
            user_number,
            state,
            error_code,
            reserve,
        })
    }

    fn command_id() -> CommandId {
        CommandId::Report
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportResp {
    pub result: u32,
}

impl ReportResp {
    pub const BODY_SIZE: usize = 4;
}

impl Encodable for ReportResp {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        buf.put_u32(self.result);
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        Self::BODY_SIZE
    }
}

impl Decodable for ReportResp {
    fn decode(_header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if buf.remaining() < Self::BODY_SIZE {
            return Err(CodecError::Incomplete);
        }
        let result = buf.get_u32();
        Ok(ReportResp { result })
    }

    fn command_id() -> CommandId {
        CommandId::ReportResp
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trace {
    pub trace_value: String,
    pub reserve: [u8; 8],
}

impl Trace {
    pub fn new() -> Self {
        Self {
            trace_value: String::new(),
            reserve: [0u8; 8],
        }
    }
}

impl Default for Trace {
    fn default() -> Self {
        Self::new()
    }
}

impl Encodable for Trace {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        encode_pstring(buf, &self.trace_value, 21, "trace_value").map_err(|e| {
            CodecError::FieldValidation {
                field: "trace_value",
                reason: e,
            }
        })?;
        buf.put_slice(&self.reserve);
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        21 + 8
    }
}

impl Decodable for Trace {
    fn decode(header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        let body_len = (header.total_length - PduHeader::SIZE as u32) as usize;
        if buf.remaining() < body_len {
            return Err(CodecError::Incomplete);
        }

        let trace_value = decode_pstring(buf, 21).map_err(|_| CodecError::Incomplete)?;
        let mut reserve = [0u8; 8];
        buf.copy_to_slice(&mut reserve);

        Ok(Trace {
            trace_value,
            reserve,
        })
    }

    fn command_id() -> CommandId {
        CommandId::Trace
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceResp {
    pub result: u32,
    pub trace_result: String,
    pub reserve: [u8; 8],
}

impl TraceResp {
    pub fn new() -> Self {
        Self {
            result: 0,
            trace_result: String::new(),
            reserve: [0u8; 8],
        }
    }
}

impl Default for TraceResp {
    fn default() -> Self {
        Self::new()
    }
}

impl Encodable for TraceResp {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        buf.put_u32(self.result);
        encode_pstring(buf, &self.trace_result, 21, "trace_result").map_err(|e| {
            CodecError::FieldValidation {
                field: "trace_result",
                reason: e,
            }
        })?;
        buf.put_slice(&self.reserve);
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        4 + 21 + 8
    }
}

impl Decodable for TraceResp {
    fn decode(header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        let body_len = (header.total_length - PduHeader::SIZE as u32) as usize;
        if buf.remaining() < body_len {
            return Err(CodecError::Incomplete);
        }

        let result = buf.get_u32();
        let trace_result = decode_pstring(buf, 21).map_err(|_| CodecError::Incomplete)?;
        let mut reserve = [0u8; 8];
        buf.copy_to_slice(&mut reserve);

        Ok(TraceResp {
            result,
            trace_result,
            reserve,
        })
    }

    fn command_id() -> CommandId {
        CommandId::TraceResp
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
    fn deliver_roundtrip() {
        let mut deliver = Deliver::new();
        deliver.user_number = "13800138000".to_string();
        deliver.message_content = b"MO reply".to_vec();

        let bytes = deliver.to_pdu_bytes(1, 0x04051200, 5);
        let decoded = decode_pdu::<Deliver>(&bytes).unwrap();
        assert_eq!(decoded.message_content, b"MO reply");
        assert_eq!(decoded.user_number, "13800138000");
    }

    #[test]
    fn deliver_resp_roundtrip() {
        let resp = DeliverResp { result: 0 };
        let bytes = resp.to_pdu_bytes(1, 0x04051200, 6);
        let decoded = decode_pdu::<DeliverResp>(&bytes).unwrap();
        assert_eq!(decoded.result, 0);
    }

    #[test]
    fn report_roundtrip() {
        let report = Report {
            submit_sequence: SgipSequence::new(1, 0x04051200, 42),
            report_type: 0,
            user_number: "13800138000".to_string(),
            state: 0,
            error_code: 0,
            reserve: [0u8; 8],
        };
        let bytes = report.to_pdu_bytes(1, 0x04051200, 7);
        let decoded = decode_pdu::<Report>(&bytes).unwrap();
        assert_eq!(decoded.state, 0);
        assert_eq!(decoded.error_code, 0);
        assert_eq!(decoded.user_number, "13800138000");
    }

    #[test]
    fn trace_roundtrip() {
        let trace = Trace {
            trace_value: "13800138000".to_string(),
            reserve: [0u8; 8],
        };
        let bytes = trace.to_pdu_bytes(1, 0x04051200, 8);
        let decoded = decode_pdu::<Trace>(&bytes).unwrap();
        assert_eq!(decoded.trace_value, "13800138000");
    }
}
