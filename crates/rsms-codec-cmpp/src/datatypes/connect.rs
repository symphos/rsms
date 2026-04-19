use bytes::{Buf, BufMut, BytesMut};
use rsms_core::{decode_pstring, encode_pstring};
use std::io::Cursor;

use crate::codec::{CodecError, Decodable, Encodable, PduHeader};
use crate::datatypes::CommandId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connect {
    pub source_addr: String,
    pub authenticator_source: [u8; 16],
    pub version: u8,
    pub timestamp: u32,
}

impl Connect {
    pub const BODY_SIZE: usize = 6 + 16 + 1 + 4;
}

impl Encodable for Connect {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        encode_pstring(buf, &self.source_addr, 6, "source_addr").map_err(|e| {
            CodecError::FieldValidation {
                field: "source_addr",
                reason: e,
            }
        })?;
        buf.put_slice(&self.authenticator_source);
        buf.put_u8(self.version);
        buf.put_u32(self.timestamp);
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        Self::BODY_SIZE
    }
}

impl Decodable for Connect {
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
        let source_addr = decode_pstring(buf, 6).map_err(|_| CodecError::Incomplete)?;
        let mut authenticator_source = [0u8; 16];
        buf.copy_to_slice(&mut authenticator_source);
        let version = buf.get_u8();
        let timestamp = buf.get_u32();
        Ok(Connect {
            source_addr,
            authenticator_source,
            version,
            timestamp,
        })
    }

    fn command_id() -> CommandId {
        CommandId::Connect
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectResp {
    pub status: u32,
    pub authenticator_ismg: [u8; 16],
    pub version: u8,
}

impl ConnectResp {
    pub const BODY_SIZE: usize = 4 + 16 + 1;
}

impl Encodable for ConnectResp {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        buf.put_u32(self.status);
        buf.put_slice(&self.authenticator_ismg);
        buf.put_u8(self.version);
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        Self::BODY_SIZE
    }
}

impl Decodable for ConnectResp {
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
        let status = buf.get_u32();
        let mut authenticator_ismg = [0u8; 16];
        buf.copy_to_slice(&mut authenticator_ismg);
        let version = buf.get_u8();
        Ok(ConnectResp {
            status,
            authenticator_ismg,
            version,
        })
    }

    fn command_id() -> CommandId {
        CommandId::ConnectResp
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
    fn connect_roundtrip() {
        let connect = Connect {
            source_addr: "106900".to_string(),
            authenticator_source: [0xab; 16],
            version: 0x30,
            timestamp: 0x01020304,
        };
        let bytes = Pdu::from(connect.clone()).to_pdu_bytes(1);
        let decoded = decode_pdu::<Connect>(&bytes.as_slice()).unwrap();
        assert_eq!(decoded.source_addr, connect.source_addr);
        assert_eq!(decoded.authenticator_source, connect.authenticator_source);
        assert_eq!(decoded.version, connect.version);
        assert_eq!(decoded.timestamp, connect.timestamp);
    }

    #[test]
    fn connect_resp_roundtrip() {
        let resp = ConnectResp {
            status: 0,
            authenticator_ismg: [0xcd; 16],
            version: 0x30,
        };
        let bytes = Pdu::from(resp.clone()).to_pdu_bytes(1);
        let decoded = decode_pdu::<ConnectResp>(&bytes.as_slice()).unwrap();
        assert_eq!(decoded.status, resp.status);
        assert_eq!(decoded.authenticator_ismg, resp.authenticator_ismg);
        assert_eq!(decoded.version, resp.version);
    }
}
