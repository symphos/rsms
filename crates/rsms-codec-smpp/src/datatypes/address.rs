use bytes::{Buf, BufMut};
use std::io::Cursor;

use crate::codec::{decode_cstring, encode_cstring, CodecError};

pub const MAX_ADDRESS_LENGTH: usize = 20;
pub const MAX_ADDRESS_BYTES: usize = MAX_ADDRESS_LENGTH + 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Address {
    pub ton: u8,
    pub npi: u8,
    pub address: String,
}

impl Address {
    pub fn new(ton: u8, npi: u8, address: impl Into<String>) -> Self {
        Self {
            ton,
            npi,
            address: address.into(),
        }
    }

    pub fn decode(buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        let ton = buf.get_u8();
        let npi = buf.get_u8();
        let address = decode_cstring(buf, MAX_ADDRESS_BYTES, "address")?;
        Ok(Self { ton, npi, address })
    }

    pub fn encode(&self, buf: &mut bytes::BytesMut) -> Result<(), CodecError> {
        buf.put_u8(self.ton);
        buf.put_u8(self.npi);
        encode_cstring(buf, &self.address, MAX_ADDRESS_BYTES)
    }

    pub fn encoded_size(&self) -> usize {
        2 + self.address.len() + 1
    }
}

pub mod ton {
    #[allow(dead_code)]
    pub const UNKNOWN: u8 = 0x00;
    #[allow(dead_code)]
    pub const INTERNATIONAL: u8 = 0x01;
    #[allow(dead_code)]
    pub const NATIONAL: u8 = 0x02;
    #[allow(dead_code)]
    pub const NETWORK_SPECIFIC: u8 = 0x03;
    #[allow(dead_code)]
    pub const SUBSCRIBER_NUMBER: u8 = 0x04;
    #[allow(dead_code)]
    pub const ALPHANUMERIC: u8 = 0x05;
    #[allow(dead_code)]
    pub const ABBREVIATED: u8 = 0x06;
}

pub mod npi {
    #[allow(dead_code)]
    pub const UNKNOWN: u8 = 0x00;
    #[allow(dead_code)]
    pub const ISDN: u8 = 0x01;
    #[allow(dead_code)]
    pub const DATA: u8 = 0x03;
    #[allow(dead_code)]
    pub const TELEX: u8 = 0x04;
    #[allow(dead_code)]
    pub const SCANNING: u8 = 0x05;
    #[allow(dead_code)]
    pub const RESERVED_1: u8 = 0x06;
    #[allow(dead_code)]
    pub const RESERVED_2: u8 = 0x07;
    #[allow(dead_code)]
    pub const NATIONAL: u8 = 0x08;
    #[allow(dead_code)]
    pub const PRIVATE: u8 = 0x09;
    #[allow(dead_code)]
    pub const ERMES: u8 = 0x0A;
    #[allow(dead_code)]
    pub const INTERNET: u8 = 0x0E;
    #[allow(dead_code)]
    pub const WAP_CLIENT: u8 = 0x0F;
}
