use bytes::{Buf, BufMut, BytesMut};
use std::io::Cursor;

use crate::codec::CodecError;

/// SMGP TLV 标签常量
pub mod tlv_tags {
    pub const TP_PID: u16 = 0x0001;
    pub const TP_UDHI: u16 = 0x0002;
    pub const LINK_ID: u16 = 0x0003;
    pub const CHARGE_USER_TYPE: u16 = 0x0004;
    pub const CHARGE_TERM_TYPE: u16 = 0x0005;
    pub const CHARGE_TERM_PSEUDO: u16 = 0x0006;
    pub const DEST_TERM_TYPE: u16 = 0x0007;
    pub const DEST_TERM_PSEUDO: u16 = 0x0008;
    pub const PK_TOTAL: u16 = 0x0009;
    pub const PK_NUMBER: u16 = 0x000A;
    pub const SUBMIT_MSG_TYPE: u16 = 0x000B;
    pub const SP_DEAL_RESULT: u16 = 0x000C;
    pub const SRC_TERM_TYPE: u16 = 0x000D;
    pub const SRC_TERM_PSEUDO: u16 = 0x000E;
    pub const NODES_COUNT: u16 = 0x000F;
    pub const MSG_SRC: u16 = 0x0010;
    pub const SRC_TYPE: u16 = 0x0011;
    pub const M_SERVICE_ID: u16 = 0x0012;
}

/// SMGP TLV 参数类型
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tlv {
    Byte { tag: u16, value: u8 },
    Short { tag: u16, value: u16 },
    Int { tag: u16, value: u32 },
    String { tag: u16, value: String },
    Octets { tag: u16, value: Vec<u8> },
    Empty { tag: u16 },
}

impl Tlv {
    pub fn tag(&self) -> u16 {
        match self {
            Tlv::Byte { tag, .. } => *tag,
            Tlv::Short { tag, .. } => *tag,
            Tlv::Int { tag, .. } => *tag,
            Tlv::String { tag, .. } => *tag,
            Tlv::Octets { tag, .. } => *tag,
            Tlv::Empty { tag } => *tag,
        }
    }

    pub fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        buf.put_u16(self.tag());
        match self {
            Tlv::Byte { value, .. } => {
                buf.put_u16(1);
                buf.put_u8(*value);
            }
            Tlv::Short { value, .. } => {
                buf.put_u16(2);
                buf.put_u16(*value);
            }
            Tlv::Int { value, .. } => {
                buf.put_u16(4);
                buf.put_u32(*value);
            }
            Tlv::String { value, .. } => {
                let bytes = value.as_bytes();
                buf.put_u16(bytes.len() as u16);
                buf.put_slice(bytes);
            }
            Tlv::Octets { value, .. } => {
                buf.put_u16(value.len() as u16);
                buf.put_slice(value);
            }
            Tlv::Empty { .. } => {
                buf.put_u16(0);
            }
        }
        Ok(())
    }

    pub fn encoded_size(&self) -> usize {
        4 + match self {
            Tlv::Byte { .. } => 1,
            Tlv::Short { .. } => 2,
            Tlv::Int { .. } => 4,
            Tlv::String { value, .. } => value.len(),
            Tlv::Octets { value, .. } => value.len(),
            Tlv::Empty { .. } => 0,
        }
    }

    pub fn decode(buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if buf.remaining() < 4 {
            return Err(CodecError::Incomplete);
        }
        let tag = buf.get_u16();
        let length = buf.get_u16() as usize;

        if buf.remaining() < length {
            return Err(CodecError::Incomplete);
        }

        let tlv = match length {
            0 => Tlv::Empty { tag },
            1 => Tlv::Byte {
                tag,
                value: buf.get_u8(),
            },
            2 => {
                let value = buf.get_u16();
                Tlv::Short { tag, value }
            }
            4 => {
                let value = buf.get_u32();
                Tlv::Int { tag, value }
            }
            _ => {
                let mut value = vec![0u8; length];
                buf.copy_to_slice(&mut value);
                Tlv::Octets { tag, value }
            }
        };

        Ok(tlv)
    }
}

/// SMGP 可选参数容器
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OptionalParameters {
    pub tlvs: Vec<Tlv>,
}

impl OptionalParameters {
    pub fn new() -> Self {
        Self { tlvs: Vec::new() }
    }

    pub fn add(&mut self, tlv: Tlv) {
        self.tlvs.push(tlv);
    }

    pub fn get_byte(&self, tag: u16) -> Option<u8> {
        self.tlvs.iter().find_map(|t| match t {
            Tlv::Byte { tag: t, value } if *t == tag => Some(*value),
            Tlv::Octets { tag: t, value } if *t == tag && value.len() == 1 => Some(value[0]),
            _ => None,
        })
    }

    pub fn get_short(&self, tag: u16) -> Option<u16> {
        self.tlvs.iter().find_map(|t| match t {
            Tlv::Short { tag: t, value } if *t == tag => Some(*value),
            _ => None,
        })
    }

    pub fn get_int(&self, tag: u16) -> Option<u32> {
        self.tlvs.iter().find_map(|t| match t {
            Tlv::Int { tag: t, value } if *t == tag => Some(*value),
            _ => None,
        })
    }

    pub fn get_string(&self, tag: u16) -> Option<String> {
        self.tlvs.iter().find_map(|t| match t {
            Tlv::String { tag: t, value } if *t == tag => Some(value.clone()),
            Tlv::Octets { tag: t, value } if *t == tag => {
                Some(String::from_utf8_lossy(value).into_owned())
            }
            _ => None,
        })
    }

    pub fn encoded_size(&self) -> usize {
        self.tlvs.iter().map(|t| t.encoded_size()).sum()
    }

    pub fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        for tlv in &self.tlvs {
            tlv.encode(buf)?;
        }
        Ok(())
    }

    pub fn decode_remaining(buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        let mut params = Self::new();
        while buf.remaining() >= 4 {
            match Tlv::decode(buf) {
                Ok(tlv) => params.add(tlv),
                Err(CodecError::Incomplete) => break,
                Err(e) => return Err(e),
            }
        }
        Ok(params)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tlv_byte_roundtrip() {
        let tlv = Tlv::Byte {
            tag: tlv_tags::TP_UDHI,
            value: 1,
        };
        let mut buf = BytesMut::new();
        tlv.encode(&mut buf).unwrap();
        let mut cursor = Cursor::new(&buf[..]);
        let decoded = Tlv::decode(&mut cursor).unwrap();
        assert_eq!(decoded, tlv);
    }

    #[test]
    fn tlv_int_roundtrip() {
        let tlv = Tlv::Int {
            tag: tlv_tags::PK_TOTAL,
            value: 3,
        };
        let mut buf = BytesMut::new();
        tlv.encode(&mut buf).unwrap();
        let mut cursor = Cursor::new(&buf[..]);
        let decoded = Tlv::decode(&mut cursor).unwrap();
        assert_eq!(decoded, tlv);
    }

    #[test]
    fn tlv_string_roundtrip() {
        let tlv = Tlv::String {
            tag: tlv_tags::LINK_ID,
            value: "ABC123".to_string(),
        };
        let mut buf = BytesMut::new();
        tlv.encode(&mut buf).unwrap();
        let mut cursor = Cursor::new(&buf[..]);
        let decoded = Tlv::decode(&mut cursor).unwrap();
        // 长度>4 的字符串被解码为 Octets，这是预期的行为
        match decoded {
            Tlv::String { tag: t, value } => {
                assert_eq!(t, tlv_tags::LINK_ID);
                assert_eq!(value, "ABC123");
            }
            Tlv::Octets { tag: t, value } => {
                assert_eq!(t, tlv_tags::LINK_ID);
                assert_eq!(String::from_utf8_lossy(&value), "ABC123");
            }
            _ => panic!("expected String or Octets"),
        }
    }

    #[test]
    fn optional_params_mixed() {
        let mut params = OptionalParameters::new();
        params.add(Tlv::Byte {
            tag: tlv_tags::TP_UDHI,
            value: 1,
        });
        params.add(Tlv::String {
            tag: tlv_tags::LINK_ID,
            value: "ABC123".to_string(),
        });

        let mut buf = BytesMut::new();
        params.encode(&mut buf).unwrap();
        let mut cursor = Cursor::new(&buf[..]);
        let decoded = OptionalParameters::decode_remaining(&mut cursor).unwrap();
        assert_eq!(decoded.tlvs.len(), 2);
        assert_eq!(decoded.get_byte(tlv_tags::TP_UDHI), Some(1));
        assert_eq!(
            decoded.get_string(tlv_tags::LINK_ID),
            Some("ABC123".to_string())
        );
    }

    #[test]
    fn unknown_tlv_as_octets() {
        let tlv = Tlv::Octets {
            tag: 0x00FF,
            value: vec![0x01, 0x02, 0x03],
        };
        let mut buf = BytesMut::new();
        tlv.encode(&mut buf).unwrap();
        let mut cursor = Cursor::new(&buf[..]);
        let decoded = Tlv::decode(&mut cursor).unwrap();
        assert_eq!(decoded, tlv);
    }
}
