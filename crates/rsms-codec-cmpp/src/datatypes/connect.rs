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
        buf.try_copy_to_slice(&mut authenticator_source)
            .map_err(|_| CodecError::Incomplete)?;
        let version = buf.try_get_u8().map_err(|_| CodecError::Incomplete)?;
        let timestamp = buf.try_get_u32().map_err(|_| CodecError::Incomplete)?;
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
        let status = buf.try_get_u32().map_err(|_| CodecError::Incomplete)?;
        let mut authenticator_ismg = [0u8; 16];
        buf.try_copy_to_slice(&mut authenticator_ismg)
            .map_err(|_| CodecError::Incomplete)?;
        let version = buf.try_get_u8().map_err(|_| CodecError::Incomplete)?;
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

/// CMPP 2.0 Connect 响应（Status 为 1 字节）。
///
/// 与 V3.0 `ConnectResp`（Status u32，body=21B）的唯一区别：Status 占 1 字节，
/// body = Status(1) + Authenticator_ISMG(16) + Version(1) = 18B，总长 30B。解码后
/// 提升为公共 `ConnectResp`（status 用 `as u32`），供注册表覆盖 V3.0 解码器、
/// 统一走 `Pdu::ConnectResp`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectRespV20 {
    pub status: u8,
    pub authenticator_ismg: [u8; 16],
    pub version: u8,
}

impl ConnectRespV20 {
    pub const BODY_SIZE: usize = 1 + 16 + 1;
}

impl Encodable for ConnectRespV20 {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        buf.put_u8(self.status);
        buf.put_slice(&self.authenticator_ismg);
        buf.put_u8(self.version);
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        Self::BODY_SIZE
    }
}

impl Decodable for ConnectRespV20 {
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
        let status = buf.try_get_u8().map_err(|_| CodecError::Incomplete)?;
        let mut authenticator_ismg = [0u8; 16];
        buf.try_copy_to_slice(&mut authenticator_ismg)
            .map_err(|_| CodecError::Incomplete)?;
        let version = buf.try_get_u8().map_err(|_| CodecError::Incomplete)?;
        Ok(ConnectRespV20 {
            status,
            authenticator_ismg,
            version,
        })
    }

    fn command_id() -> CommandId {
        CommandId::ConnectResp
    }
}

impl From<ConnectRespV20> for crate::codec::Pdu {
    fn from(v20: ConnectRespV20) -> Self {
        crate::codec::Pdu::ConnectResp(ConnectResp {
            status: v20.status as u32,
            authenticator_ismg: v20.authenticator_ismg,
            version: v20.version,
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
    fn connect_resp_v20_decode_18b_body() {
        // V2.0 ConnectResp：body = Status(1) + Authenticator_ISMG(16) + Version(1) = 18B，总长 30B。
        let ismg = [0xABu8; 16];
        let total_len = (PduHeader::SIZE + 18) as u32;
        let mut pdu = Vec::with_capacity(total_len as usize);
        pdu.extend_from_slice(&total_len.to_be_bytes());
        pdu.extend_from_slice(&(CommandId::ConnectResp as u32).to_be_bytes());
        pdu.extend_from_slice(&5u32.to_be_bytes());
        pdu.push(0); // status
        pdu.extend_from_slice(&ismg);
        pdu.push(0x20); // version
        let decoded = decode_pdu::<ConnectRespV20>(&pdu).unwrap();
        assert_eq!(decoded.status, 0);
        assert_eq!(decoded.authenticator_ismg, ismg);
        assert_eq!(decoded.version, 0x20);
    }

    #[test]
    fn connect_resp_v20_into_pdu_promotes_status() {
        let v20 = ConnectRespV20 {
            status: 1,
            authenticator_ismg: [0xCD; 16],
            version: 0x20,
        };
        match Pdu::from(v20) {
            Pdu::ConnectResp(r) => {
                assert_eq!(r.status, 1u32, "status 应由 u8 提升为 u32");
                assert_eq!(r.authenticator_ismg, [0xCD; 16]);
                assert_eq!(r.version, 0x20);
            }
            other => panic!("expected Pdu::ConnectResp, got {other:?}"),
        }
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
