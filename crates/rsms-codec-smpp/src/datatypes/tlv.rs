use bytes::{Buf, BufMut, Bytes, BytesMut};
use std::io::Cursor;

use crate::codec::{CodecError, Encodable};

pub mod tags {
    pub const USER_MESSAGE_REFERENCE: u16 = 0x0204;
    pub const SOURCE_PORT: u16 = 0x020A;
    pub const DESTINATION_PORT: u16 = 0x020C;
    pub const SAR_MSG_REF_NUM: u16 = 0x020E;
    pub const SAR_TOTAL_SEGMENTS: u16 = 0x020F;
    pub const SAR_SEGMENT_SEQNUM: u16 = 0x0210;
    pub const MORE_MESSAGES_TO_SEND: u16 = 0x0426;
    pub const PAYLOAD_TYPE: u16 = 0x0019;
    pub const MESSAGE_PAYLOAD: u16 = 0x0424;
    pub const PRIVACY_INDICATOR: u16 = 0x0201;
    pub const CALLBACK_NUM: u16 = 0x0381;
    pub const SOURCE_SUBADDRESS: u16 = 0x0202;
    pub const DEST_SUBADDRESS: u16 = 0x0203;
    pub const DISPLAY_TIME: u16 = 0x1201;
    pub const SMS_SIGNAL: u16 = 0x1203;
    pub const MS_VALIDITY: u16 = 0x1204;
    pub const MS_MSG_WAIT_FACILITIES: u16 = 0x1205;
    pub const NUMBER_OF_MESSAGES: u16 = 0x0205;
    pub const ALERT_ON_MSG_DELIVERY: u16 = 0x130C;
    pub const LANGUAGE_INDICATOR: u16 = 0x000D;
    pub const ITS_REPLY_TYPE: u16 = 0x1380;
    pub const ITS_SESSION_INFO: u16 = 0x1383;
    pub const USER_DATA_HEADER: u16 = 0x0005;
    pub const NETWORK_ERROR_CODE: u16 = 0x0423;
    pub const DELIVERY_FAILURE_REASON: u16 = 0x0425;
    pub const ADDITIONAL_STATUS_INFO_TEXT: u16 = 0x001D;
    pub const RECEIPTED_MESSAGE_ID: u16 = 0x001E;
    pub const MESSAGE_STATE: u16 = 0x0427;
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tlv {
    pub tag: u16,
    pub length: u16,
    pub value: Bytes,
}

impl Tlv {
    pub fn new(tag: u16, value: Vec<u8>) -> Self {
        let length = value.len() as u16;
        Self {
            tag,
            length,
            value: Bytes::from(value),
        }
    }

    pub fn to_bytes(&self) -> Bytes {
        let mut buf = BytesMut::new();
        self.encode(&mut buf).expect("TLV encoding should not fail");
        buf.freeze()
    }

    pub fn decode(buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if buf.remaining() < 4 {
            return Err(CodecError::Incomplete);
        }
        let tag = buf.get_u16();
        let length = buf.get_u16();
        if buf.remaining() < length as usize {
            return Err(CodecError::Incomplete);
        }
        let mut value_bytes = vec![0u8; length as usize];
        buf.copy_to_slice(&mut value_bytes);
        let value = Bytes::from(value_bytes);
        Ok(Self { tag, length, value })
    }
}

impl Encodable for Tlv {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        buf.put_u16(self.tag);
        buf.put_u16(self.length);
        buf.extend_from_slice(&self.value);
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        4 + self.value.len()
    }
}
