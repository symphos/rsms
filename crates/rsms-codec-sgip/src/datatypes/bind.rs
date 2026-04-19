use bytes::{Buf, BufMut, BytesMut};
use std::io::Cursor;

use crate::codec::{CodecError, Decodable, Encodable, PduHeader};
use crate::datatypes::CommandId;

/// SGIP Bind 请求
///
/// SGIP 使用明文认证（无 MD5），与 CMPP/SMGP 的 MD5 挑战-响应不同。
///
/// 协议字段（41 字节）：
/// - LoginType:     1 byte  (1=SP→SMG, 2=SMG→SP, 3=SMG↔SMG, 4=SMG→GNS, 5=GNS→SMG, 6=GNS↔GNS, 11=test)
/// - LoginName:     16 bytes (用户名，null-padded)
/// - LoginPassword: 16 bytes (密码，明文，null-padded)
/// - Reserve:       8 bytes  (保留)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bind {
    pub login_type: u8,
    pub login_name: String,
    pub login_password: String,
    pub reserve: [u8; 8],
}

impl Bind {
    pub const BODY_SIZE: usize = 1 + 16 + 16 + 8;

    pub fn new() -> Self {
        Self {
            login_type: 1,
            login_name: String::new(),
            login_password: String::new(),
            reserve: [0u8; 8],
        }
    }
}

impl Default for Bind {
    fn default() -> Self {
        Self::new()
    }
}

impl Encodable for Bind {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        let name_bytes = self.login_name.as_bytes();
        if name_bytes.len() >= 16 {
            return Err(CodecError::FieldValidation {
                field: "login_name",
                reason: format!("length {} exceeds max 15 bytes", name_bytes.len()),
            });
        }

        let pwd_bytes = self.login_password.as_bytes();
        if pwd_bytes.len() >= 16 {
            return Err(CodecError::FieldValidation {
                field: "login_password",
                reason: format!("length {} exceeds max 15 bytes", pwd_bytes.len()),
            });
        }

        buf.put_u8(self.login_type);

        // LoginName: 16 bytes, null-padded
        buf.put_slice(name_bytes);
        buf.put_bytes(0, 16 - name_bytes.len());

        // LoginPassword: 16 bytes, null-padded
        buf.put_slice(pwd_bytes);
        buf.put_bytes(0, 16 - pwd_bytes.len());

        // Reserve: 8 bytes
        buf.put_slice(&self.reserve);

        Ok(())
    }

    fn encoded_size(&self) -> usize {
        Self::BODY_SIZE
    }
}

impl Decodable for Bind {
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

        let login_type = buf.get_u8();

        let mut login_name_buf = [0u8; 16];
        buf.copy_to_slice(&mut login_name_buf);
        let login_name = String::from_utf8_lossy(&login_name_buf)
            .trim_end_matches('\0')
            .to_string();

        let mut login_password_buf = [0u8; 16];
        buf.copy_to_slice(&mut login_password_buf);
        let login_password = String::from_utf8_lossy(&login_password_buf)
            .trim_end_matches('\0')
            .to_string();

        let mut reserve = [0u8; 8];
        buf.copy_to_slice(&mut reserve);

        Ok(Bind {
            login_type,
            login_name,
            login_password,
            reserve,
        })
    }

    fn command_id() -> CommandId {
        CommandId::Bind
    }
}

/// SGIP Bind 响应
///
/// 协议字段（9 字节）：
/// - Result:  1 byte (0=成功)
/// - Reserve: 8 bytes
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindResp {
    pub result: u8,
    pub reserve: [u8; 8],
}

impl BindResp {
    pub const BODY_SIZE: usize = 1 + 8;

    pub fn new() -> Self {
        Self {
            result: 0,
            reserve: [0u8; 8],
        }
    }
}

impl Default for BindResp {
    fn default() -> Self {
        Self::new()
    }
}

impl Encodable for BindResp {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        buf.put_u8(self.result);
        buf.put_slice(&self.reserve);
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        Self::BODY_SIZE
    }
}

impl Decodable for BindResp {
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

        let result = buf.get_u8();
        let mut reserve = [0u8; 8];
        buf.copy_to_slice(&mut reserve);

        Ok(BindResp { result, reserve })
    }

    fn command_id() -> CommandId {
        CommandId::BindResp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_pdu<T: Decodable>(bytes: &[u8]) -> Result<T, CodecError> {
        let mut cursor = Cursor::new(bytes);
        let header = PduHeader::decode(&mut cursor)?;
        T::decode(header, &mut cursor)
    }

    #[test]
    fn bind_roundtrip() {
        let bind = Bind {
            login_type: 1,
            login_name: "SP001".to_string(),
            login_password: "secret123".to_string(),
            reserve: [0u8; 8],
        };
        let bytes = bind.to_pdu_bytes(1, 2, 3);
        // header(20) + body(41) = 61
        assert_eq!(bytes.len(), 20 + 41);

        let decoded = decode_pdu::<Bind>(&bytes).unwrap();
        assert_eq!(decoded.login_type, 1);
        assert_eq!(decoded.login_name, "SP001");
        assert_eq!(decoded.login_password, "secret123");
        assert_eq!(decoded.reserve, [0u8; 8]);
    }

    #[test]
    fn bind_resp_roundtrip() {
        let resp = BindResp {
            result: 0,
            reserve: [0u8; 8],
        };
        let bytes = resp.to_pdu_bytes(1, 2, 3);
        // header(20) + body(9) = 29
        assert_eq!(bytes.len(), 20 + 9);

        let decoded = decode_pdu::<BindResp>(&bytes).unwrap();
        assert_eq!(decoded.result, 0);
        assert_eq!(decoded.reserve, [0u8; 8]);
    }

    #[test]
    fn bind_name_too_long() {
        let bind = Bind {
            login_type: 1,
            login_name: "this_name_is_way_too_long_for_sgip".to_string(),
            login_password: "secret".to_string(),
            reserve: [0u8; 8],
        };
        let mut buf = BytesMut::new();
        let result = bind.encode(&mut buf);
        assert!(result.is_err());
    }

    #[test]
    fn bind_password_too_long() {
        let bind = Bind {
            login_type: 1,
            login_name: "SP001".to_string(),
            login_password: "this_password_is_way_too_long_for_sgip".to_string(),
            reserve: [0u8; 8],
        };
        let mut buf = BytesMut::new();
        let result = bind.encode(&mut buf);
        assert!(result.is_err());
    }

    #[test]
    fn bind_exact_byte_boundaries() {
        // 15-char name (max allowed, since 16th byte is null terminator)
        let bind = Bind {
            login_type: 2,
            login_name: "abcdefghijklmno".to_string(),
            login_password: "123456789012345".to_string(),
            reserve: [0xFF; 8],
        };
        let bytes = bind.to_pdu_bytes(1, 2, 3);
        let decoded = decode_pdu::<Bind>(&bytes).unwrap();
        assert_eq!(decoded.login_name, "abcdefghijklmno");
        assert_eq!(decoded.login_password, "123456789012345");
        assert_eq!(decoded.login_type, 2);
        assert_eq!(decoded.reserve, [0xFF; 8]);
    }
}
