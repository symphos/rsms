use bytes::{Buf, BufMut, BytesMut};
use rsms_core::{decode_pstring, encode_pstring};
use std::io::Cursor;

use crate::codec::{CodecError, Decodable, Encodable, PduHeader};
use crate::datatypes::CommandId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub time: String,
    pub query_type: u8,
    pub query_code: String,
    pub reserve: String,
    pub m_type: u8,
}

impl Query {
    pub const BODY_SIZE: usize = 8 + 1 + 10 + 8 + 1;

    pub fn new() -> Self {
        Self {
            time: String::new(),
            query_type: 0,
            query_code: String::new(),
            reserve: String::new(),
            m_type: 0,
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
        encode_pstring(buf, &self.time, 8, "time").map_err(|e| CodecError::FieldValidation {
            field: "time",
            reason: e,
        })?;
        buf.put_u8(self.query_type);
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
        buf.put_u8(self.m_type);
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

        let time = decode_pstring(buf, 8).map_err(|_| CodecError::Incomplete)?;
        let query_type = buf.get_u8();
        let query_code = decode_pstring(buf, 10).map_err(|_| CodecError::Incomplete)?;
        let reserve = decode_pstring(buf, 8).map_err(|_| CodecError::Incomplete)?;
        let m_type = buf.get_u8();

        Ok(Query {
            time,
            query_type,
            query_code,
            reserve,
            m_type,
        })
    }

    fn command_id() -> CommandId {
        CommandId::Query
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryResp {
    pub time: String,
    pub query_type: u8,
    pub query_code: String,
    pub mt_ttl_msg: u32,
    pub mt_suc: u32,
    pub mt_fail: u32,
    pub mo_rcv: u32,
    pub mo_suc: u32,
    pub mo_fail: u32,
    pub mo_scs: u32,
    pub mo_err: u32,
}

impl QueryResp {
    pub const FIXED_SIZE: usize = 8 + 1 + 10 + 4 + 4 + 4 + 4 + 4 + 4 + 4 + 4;
}

impl Encodable for QueryResp {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        encode_pstring(buf, &self.time, 8, "time").map_err(|e| CodecError::FieldValidation {
            field: "time",
            reason: e,
        })?;
        buf.put_u8(self.query_type);
        encode_pstring(buf, &self.query_code, 10, "query_code").map_err(|e| {
            CodecError::FieldValidation {
                field: "query_code",
                reason: e,
            }
        })?;
        buf.put_u32(self.mt_ttl_msg);
        buf.put_u32(self.mt_suc);
        buf.put_u32(self.mt_fail);
        buf.put_u32(self.mo_rcv);
        buf.put_u32(self.mo_suc);
        buf.put_u32(self.mo_fail);
        buf.put_u32(self.mo_scs);
        buf.put_u32(self.mo_err);
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        Self::FIXED_SIZE
    }
}

impl Decodable for QueryResp {
    fn decode(_header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if buf.remaining() < Self::FIXED_SIZE {
            return Err(CodecError::Incomplete);
        }

        let time = decode_pstring(buf, 8).map_err(|_| CodecError::Incomplete)?;
        let query_type = buf.get_u8();
        let query_code = decode_pstring(buf, 10).map_err(|_| CodecError::Incomplete)?;
        let mt_ttl_msg = buf.get_u32();
        let mt_suc = buf.get_u32();
        let mt_fail = buf.get_u32();
        let mo_rcv = buf.get_u32();
        let mo_suc = buf.get_u32();
        let mo_fail = buf.get_u32();
        let mo_scs = buf.get_u32();
        let mo_err = buf.get_u32();

        Ok(QueryResp {
            time,
            query_type,
            query_code,
            mt_ttl_msg,
            mt_suc,
            mt_fail,
            mo_rcv,
            mo_suc,
            mo_fail,
            mo_scs,
            mo_err,
        })
    }

    fn command_id() -> CommandId {
        CommandId::QueryResp
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cancel {
    pub msg_id: [u8; 8],
}

impl Cancel {
    pub const BODY_SIZE: usize = 8;
}

impl Encodable for Cancel {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        buf.put_slice(&self.msg_id);
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        Self::BODY_SIZE
    }
}

impl Decodable for Cancel {
    fn decode(_header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if buf.remaining() < Self::BODY_SIZE {
            return Err(CodecError::Incomplete);
        }
        let mut msg_id = [0u8; 8];
        buf.copy_to_slice(&mut msg_id);
        Ok(Cancel { msg_id })
    }

    fn command_id() -> CommandId {
        CommandId::Cancel
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelResp {
    pub success_id: u32,
}

impl CancelResp {
    pub const BODY_SIZE: usize = 4;
}

impl Encodable for CancelResp {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        buf.put_u32(self.success_id);
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        Self::BODY_SIZE
    }
}

impl Decodable for CancelResp {
    fn decode(_header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if buf.remaining() < Self::BODY_SIZE {
            return Err(CodecError::Incomplete);
        }
        let success_id = buf.get_u32();
        Ok(CancelResp { success_id })
    }

    fn command_id() -> CommandId {
        CommandId::CancelResp
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
    fn query_roundtrip() {
        let query = Query {
            time: "04051200".to_string(),
            query_type: 0,
            query_code: "01".to_string(),
            reserve: String::new(),
            m_type: 0,
        };
        let bytes = Pdu::from(query).to_pdu_bytes(1);
        let decoded = decode_pdu::<Query>(&bytes.as_slice()).unwrap();
        assert_eq!(decoded.time, "04051200");
        assert_eq!(decoded.query_type, 0);
        assert_eq!(decoded.query_code, "01");
    }

    #[test]
    fn query_resp_roundtrip() {
        let resp = QueryResp {
            time: "04051200".to_string(),
            query_type: 0,
            query_code: "01".to_string(),
            mt_ttl_msg: 100,
            mt_suc: 90,
            mt_fail: 10,
            mo_rcv: 50,
            mo_suc: 45,
            mo_fail: 5,
            mo_scs: 0,
            mo_err: 0,
        };
        let bytes = Pdu::from(resp).to_pdu_bytes(2);
        let decoded = decode_pdu::<QueryResp>(&bytes.as_slice()).unwrap();
        assert_eq!(decoded.mt_ttl_msg, 100);
        assert_eq!(decoded.mt_suc, 90);
        assert_eq!(decoded.query_code, "01");
    }

    #[test]
    fn cancel_roundtrip() {
        let cancel = Cancel {
            msg_id: [0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0],
        };
        let bytes = Pdu::from(cancel.clone()).to_pdu_bytes(3);
        let decoded = decode_pdu::<Cancel>(&bytes.as_slice()).unwrap();
        assert_eq!(decoded.msg_id, cancel.msg_id);
    }

    #[test]
    fn cancel_resp_roundtrip() {
        let resp = CancelResp { success_id: 1 };
        let bytes = Pdu::from(resp).to_pdu_bytes(4);
        let decoded = decode_pdu::<CancelResp>(&bytes.as_slice()).unwrap();
        assert_eq!(decoded.success_id, 1);
    }
}
