use bytes::{Buf, BufMut, BytesMut};
use std::io::Cursor;

use crate::codec::{CodecError, Decodable, Encodable, PduHeader};
use crate::datatypes::CommandId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Login {
    pub client_id: String,
    pub authenticator: [u8; 16],
    pub login_mode: u8,
    pub timestamp: u32,
    pub version: u8,
}

impl Login {
    pub const BODY_SIZE: usize = 8 + 16 + 1 + 4 + 1;
}

impl Encodable for Login {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        if self.client_id.len() > 8 {
            return Err(CodecError::FieldValidation {
                field: "client_id",
                reason: "exceeds 8 bytes".to_string(),
            });
        }
        let mut id_buf = [b' '; 8];
        let bytes = self.client_id.as_bytes();
        id_buf[..bytes.len()].copy_from_slice(bytes);
        buf.put_slice(&id_buf);
        buf.put_slice(&self.authenticator);
        buf.put_u8(self.login_mode);
        buf.put_u32(self.timestamp);
        buf.put_u8(self.version);
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        Self::BODY_SIZE
    }
}

impl Decodable for Login {
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
        let mut client_id_buf = [0u8; 8];
        buf.copy_to_slice(&mut client_id_buf);
        let client_id = String::from_utf8_lossy(&client_id_buf).trim().to_string();
        let mut authenticator = [0u8; 16];
        buf.copy_to_slice(&mut authenticator);
        let login_mode = buf.get_u8();
        let timestamp = buf.get_u32();
        let version = buf.get_u8();
        Ok(Login {
            client_id,
            authenticator,
            login_mode,
            timestamp,
            version,
        })
    }

    fn command_id() -> CommandId {
        CommandId::Login
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginResp {
    pub status: u32,
    pub authenticator: [u8; 16],
    pub version: u8,
}

impl LoginResp {
    pub const BODY_SIZE: usize = 4 + 16 + 1;
}

impl Encodable for LoginResp {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        buf.put_u32(self.status);
        buf.put_slice(&self.authenticator);
        buf.put_u8(self.version);
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        Self::BODY_SIZE
    }
}

impl Decodable for LoginResp {
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
        let mut authenticator = [0u8; 16];
        buf.copy_to_slice(&mut authenticator);
        let version = buf.get_u8();
        Ok(LoginResp {
            status,
            authenticator,
            version,
        })
    }

    fn command_id() -> CommandId {
        CommandId::LoginResp
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
    fn login_roundtrip() {
        let login = Login {
            client_id: "client01".to_string(),
            authenticator: [0x12; 16],
            login_mode: 0,
            timestamp: 0x01020304,
            version: 0x30,
        };
        let bytes = Pdu::from(login.clone()).to_pdu_bytes(1);
        let decoded = decode_pdu::<Login>(&bytes.as_slice()).unwrap();
        assert_eq!(decoded.client_id, login.client_id);
        assert_eq!(decoded.authenticator, login.authenticator);
        assert_eq!(decoded.login_mode, login.login_mode);
        assert_eq!(decoded.timestamp, login.timestamp);
        assert_eq!(decoded.version, login.version);
    }

    #[test]
    fn login_resp_roundtrip() {
        let resp = LoginResp {
            status: 0,
            authenticator: [0xcd; 16],
            version: 0x30,
        };
        let bytes = Pdu::from(resp.clone()).to_pdu_bytes(1);
        let decoded = decode_pdu::<LoginResp>(&bytes.as_slice()).unwrap();
        assert_eq!(decoded.status, resp.status);
        assert_eq!(decoded.authenticator, resp.authenticator);
        assert_eq!(decoded.version, resp.version);
    }
}
