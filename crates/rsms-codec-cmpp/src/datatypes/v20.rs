use bytes::{Buf, BufMut, BytesMut};
use rsms_core::{decode_pstring, encode_pstring};
use std::io::Cursor;

use crate::codec::{CodecError, Decodable, PduHeader};
use crate::datatypes::CommandId;

#[allow(dead_code)]
pub const SUBMIT_V20_BODY_SIZE: usize =
    8 + 1 + 1 + 1 + 1 + 10 + 1 + 21 + 1 + 1 + 1 + 6 + 2 + 6 + 17 + 17 + 21 + 1 + 21 + 1 + 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitV20 {
    pub msg_id: [u8; 8],
    pub pk_total: u8,
    pub pk_number: u8,
    pub registered_delivery: u8,
    pub msg_level: u8,
    pub service_id: String,
    pub fee_user_type: u8,
    pub fee_terminal_id: String,
    pub tppid: u8,
    pub tpudhi: u8,
    pub msg_fmt: u8,
    pub msg_src: String,
    pub fee_type: String,
    pub fee_code: String,
    pub valid_time: String,
    pub at_time: String,
    pub src_id: String,
    pub dest_usr_tl: u8,
    pub dest_terminal_ids: Vec<String>,
    pub msg_content: Vec<u8>,
    pub reserve: [u8; 8],
}

impl SubmitV20 {
    pub fn new() -> Self {
        Self {
            msg_id: [0u8; 8],
            pk_total: 1,
            pk_number: 1,
            registered_delivery: 0,
            msg_level: 1,
            service_id: String::new(),
            fee_user_type: 0,
            fee_terminal_id: String::new(),
            tppid: 0,
            tpudhi: 0,
            msg_fmt: 0,
            msg_src: String::new(),
            fee_type: String::new(),
            fee_code: String::new(),
            valid_time: String::new(),
            at_time: String::new(),
            src_id: String::new(),
            dest_usr_tl: 0,
            dest_terminal_ids: Vec::new(),
            msg_content: Vec::new(),
            reserve: [0u8; 8],
        }
    }
}

impl Default for SubmitV20 {
    fn default() -> Self {
        Self::new()
    }
}

pub fn encoded_size_v20(submit: &SubmitV20) -> usize {
    8 + 1
        + 1
        + 1
        + 1
        + 10
        + 1
        + 21
        + 1
        + 1
        + 1
        + 6
        + 2
        + 6
        + 17
        + 17
        + 21
        + 1
        + (submit.dest_terminal_ids.len() * 21)
        + 1
        + submit.msg_content.len()
        + 8
}

fn encode_pstring_fixed(value: &str, max_len: usize) -> Vec<u8> {
    let mut buf = BytesMut::new();
    encode_pstring(&mut buf, value, max_len, "test").unwrap();
    buf.to_vec()
}

pub fn encode_pdu_submit_v20(buf: &mut BytesMut, submit: &SubmitV20) -> Result<(), CodecError> {
    buf.put_slice(&submit.msg_id);
    buf.put_u8(submit.pk_total);
    buf.put_u8(submit.pk_number);
    buf.put_u8(submit.registered_delivery);
    buf.put_u8(submit.msg_level);
    encode_pstring(buf, &submit.service_id, 10, "service_id")
        .map_err(|_| CodecError::Incomplete)?;
    buf.put_u8(submit.fee_user_type);
    encode_pstring(buf, &submit.fee_terminal_id, 21, "fee_terminal_id")
        .map_err(|_| CodecError::Incomplete)?;
    buf.put_u8(submit.tppid);
    buf.put_u8(submit.tpudhi);
    buf.put_u8(submit.msg_fmt);
    encode_pstring(buf, &submit.msg_src, 6, "msg_src").map_err(|_| CodecError::Incomplete)?;
    encode_pstring(buf, &submit.fee_type, 2, "fee_type").map_err(|_| CodecError::Incomplete)?;
    encode_pstring(buf, &submit.fee_code, 6, "fee_code").map_err(|_| CodecError::Incomplete)?;
    encode_pstring(buf, &submit.valid_time, 17, "valid_time")
        .map_err(|_| CodecError::Incomplete)?;
    encode_pstring(buf, &submit.at_time, 17, "at_time").map_err(|_| CodecError::Incomplete)?;
    encode_pstring(buf, &submit.src_id, 21, "src_id").map_err(|_| CodecError::Incomplete)?;
    buf.put_u8(submit.dest_usr_tl);
    for dest in &submit.dest_terminal_ids {
        encode_pstring(buf, dest, 21, "dest_terminal_id").map_err(|_| CodecError::Incomplete)?;
    }
    buf.put_u8(submit.msg_content.len() as u8);
    buf.put_slice(&submit.msg_content);
    buf.put_slice(&submit.reserve);
    Ok(())
}

pub fn build_submit_v20_pdu(seq_id: u32, submit: &SubmitV20) -> Vec<u8> {
    let total_len = PduHeader::SIZE + encoded_size_v20(submit);
    let mut pdu = Vec::with_capacity(total_len);
    pdu.extend_from_slice(&(total_len as u32).to_be_bytes());
    pdu.extend_from_slice(&(CommandId::Submit as u32).to_be_bytes());
    pdu.extend_from_slice(&seq_id.to_be_bytes());
    pdu.extend_from_slice(&submit.msg_id);
    pdu.push(submit.pk_total);
    pdu.push(submit.pk_number);
    pdu.push(submit.registered_delivery);
    pdu.push(submit.msg_level);
    pdu.extend_from_slice(&encode_pstring_fixed(&submit.service_id, 10));
    pdu.push(submit.fee_user_type);
    pdu.extend_from_slice(&encode_pstring_fixed(&submit.fee_terminal_id, 21));
    pdu.push(submit.tppid);
    pdu.push(submit.tpudhi);
    pdu.push(submit.msg_fmt);
    pdu.extend_from_slice(&encode_pstring_fixed(&submit.msg_src, 6));
    pdu.extend_from_slice(&encode_pstring_fixed(&submit.fee_type, 2));
    pdu.extend_from_slice(&encode_pstring_fixed(&submit.fee_code, 6));
    pdu.extend_from_slice(&encode_pstring_fixed(&submit.valid_time, 17));
    pdu.extend_from_slice(&encode_pstring_fixed(&submit.at_time, 17));
    pdu.extend_from_slice(&encode_pstring_fixed(&submit.src_id, 21));
    pdu.push(submit.dest_usr_tl);
    for dest in &submit.dest_terminal_ids {
        pdu.extend_from_slice(&encode_pstring_fixed(dest, 21));
    }
    pdu.push(submit.msg_content.len() as u8);
    pdu.extend_from_slice(&submit.msg_content);
    pdu.extend_from_slice(&submit.reserve);
    pdu
}

pub fn decode_submit_v20(
    header: PduHeader,
    buf: &mut Cursor<&[u8]>,
) -> Result<SubmitV20, CodecError> {
    let body_len = (header.total_length - PduHeader::SIZE as u32) as usize;
    if buf.remaining() < body_len {
        return Err(CodecError::Incomplete);
    }

    let mut msg_id = [0u8; 8];
    buf.copy_to_slice(&mut msg_id);
    let pk_total = buf.get_u8();
    let pk_number = buf.get_u8();
    let registered_delivery = buf.get_u8();
    let msg_level = buf.get_u8();
    let service_id = decode_pstring(buf, 10).map_err(|_| CodecError::Incomplete)?;
    let fee_user_type = buf.get_u8();
    let fee_terminal_id = decode_pstring(buf, 21).map_err(|_| CodecError::Incomplete)?;
    let tppid = buf.get_u8();
    let tpudhi = buf.get_u8();
    let msg_fmt = buf.get_u8();
    let msg_src = decode_pstring(buf, 6).map_err(|_| CodecError::Incomplete)?;
    let fee_type = decode_pstring(buf, 2).map_err(|_| CodecError::Incomplete)?;
    let fee_code = decode_pstring(buf, 6).map_err(|_| CodecError::Incomplete)?;
    let valid_time = decode_pstring(buf, 17).map_err(|_| CodecError::Incomplete)?;
    let at_time = decode_pstring(buf, 17).map_err(|_| CodecError::Incomplete)?;
    let src_id = decode_pstring(buf, 21).map_err(|_| CodecError::Incomplete)?;
    let dest_usr_tl = buf.get_u8();

    let mut dest_terminal_ids = Vec::with_capacity(dest_usr_tl as usize);
    for _ in 0..dest_usr_tl {
        let dest_id = decode_pstring(buf, 21).map_err(|_| CodecError::Incomplete)?;
        dest_terminal_ids.push(dest_id);
    }

    let msg_length = buf.get_u8() as usize;
    let mut msg_content = vec![0u8; msg_length];
    buf.copy_to_slice(&mut msg_content);
    let mut reserve = [0u8; 8];
    buf.copy_to_slice(&mut reserve);

    Ok(SubmitV20 {
        msg_id,
        pk_total,
        pk_number,
        registered_delivery,
        msg_level,
        service_id,
        fee_user_type,
        fee_terminal_id,
        tppid,
        tpudhi,
        msg_fmt,
        msg_src,
        fee_type,
        fee_code,
        valid_time,
        at_time,
        src_id,
        dest_usr_tl,
        dest_terminal_ids,
        msg_content,
        reserve,
    })
}

impl From<SubmitV20> for super::Submit {
    fn from(v20: SubmitV20) -> Self {
        super::Submit {
            msg_id: v20.msg_id,
            pk_total: v20.pk_total,
            pk_number: v20.pk_number,
            registered_delivery: v20.registered_delivery,
            msg_level: v20.msg_level,
            service_id: v20.service_id,
            fee_user_type: v20.fee_user_type,
            fee_terminal_id: v20.fee_terminal_id,
            fee_terminal_type: 0,
            tppid: v20.tppid,
            tpudhi: v20.tpudhi,
            msg_fmt: v20.msg_fmt,
            msg_src: v20.msg_src,
            fee_type: v20.fee_type,
            fee_code: v20.fee_code,
            valid_time: v20.valid_time,
            at_time: v20.at_time,
            src_id: v20.src_id,
            dest_usr_tl: v20.dest_usr_tl,
            dest_terminal_ids: v20.dest_terminal_ids,
            dest_terminal_type: 0,
            msg_content: v20.msg_content,
            link_id: String::new(),
        }
    }
}

impl Decodable for SubmitV20 {
    fn decode(header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        decode_submit_v20(header, buf)
    }

    fn command_id() -> CommandId {
        CommandId::Submit
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliverV20 {
    pub msg_id: [u8; 8],
    pub dest_id: String,
    pub service_id: String,
    pub tppid: u8,
    pub tpudhi: u8,
    pub msg_fmt: u8,
    pub src_terminal_id: String,
    pub registered_delivery: u8,
    pub msg_content: Vec<u8>,
}

impl DeliverV20 {
    pub fn new() -> Self {
        Self {
            msg_id: [0u8; 8],
            dest_id: String::new(),
            service_id: String::new(),
            tppid: 0,
            tpudhi: 0,
            msg_fmt: 0,
            src_terminal_id: String::new(),
            registered_delivery: 0,
            msg_content: Vec::new(),
        }
    }
}

impl Default for DeliverV20 {
    fn default() -> Self {
        Self::new()
    }
}

pub fn decode_deliver_v20(
    header: PduHeader,
    buf: &mut Cursor<&[u8]>,
) -> Result<DeliverV20, CodecError> {
    let body_len = (header.total_length - PduHeader::SIZE as u32) as usize;
    if buf.remaining() < body_len {
        return Err(CodecError::Incomplete);
    }

    let mut msg_id = [0u8; 8];
    buf.copy_to_slice(&mut msg_id);
    let dest_id = decode_pstring(buf, 21).map_err(|_| CodecError::Incomplete)?;
    let service_id = decode_pstring(buf, 10).map_err(|_| CodecError::Incomplete)?;
    let tppid = buf.get_u8();
    let tpudhi = buf.get_u8();
    let msg_fmt = buf.get_u8();
    let src_terminal_id = decode_pstring(buf, 21).map_err(|_| CodecError::Incomplete)?;
    let registered_delivery = buf.get_u8();
    let msg_length = buf.get_u8() as usize;
    let mut msg_content = vec![0u8; msg_length];
    buf.copy_to_slice(&mut msg_content);

    Ok(DeliverV20 {
        msg_id,
        dest_id,
        service_id,
        tppid,
        tpudhi,
        msg_fmt,
        src_terminal_id,
        registered_delivery,
        msg_content,
    })
}

impl From<DeliverV20> for super::Deliver {
    fn from(v20: DeliverV20) -> Self {
        super::Deliver {
            msg_id: v20.msg_id,
            dest_id: v20.dest_id,
            service_id: v20.service_id,
            tppid: v20.tppid,
            tpudhi: v20.tpudhi,
            msg_fmt: v20.msg_fmt,
            src_terminal_id: v20.src_terminal_id,
            src_terminal_type: 0,
            registered_delivery: v20.registered_delivery,
            msg_content: v20.msg_content,
            link_id: String::new(),
        }
    }
}

impl Decodable for DeliverV20 {
    fn decode(header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        decode_deliver_v20(header, buf)
    }

    fn command_id() -> CommandId {
        CommandId::Deliver
    }
}

pub fn encoded_size_deliver_v20(deliver: &DeliverV20) -> usize {
    8 + 21 + 10 + 1 + 1 + 1 + 21 + 1 + 1 + deliver.msg_content.len()
}

pub fn encode_pdu_deliver_v20(buf: &mut BytesMut, deliver: &DeliverV20) -> Result<(), CodecError> {
    buf.put_slice(&deliver.msg_id);
    encode_pstring(buf, &deliver.dest_id, 21, "dest_id").map_err(|_| CodecError::Incomplete)?;
    encode_pstring(buf, &deliver.service_id, 10, "service_id")
        .map_err(|_| CodecError::Incomplete)?;
    buf.put_u8(deliver.tppid);
    buf.put_u8(deliver.tpudhi);
    buf.put_u8(deliver.msg_fmt);
    encode_pstring(buf, &deliver.src_terminal_id, 21, "src_terminal_id")
        .map_err(|_| CodecError::Incomplete)?;
    buf.put_u8(deliver.registered_delivery);
    buf.put_u8(deliver.msg_content.len() as u8);
    buf.put_slice(&deliver.msg_content);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BytesMut;
    use rsms_core::encode_pstring;

    fn encode_pstring_fixed_for_test(value: &str, max_len: usize) -> Vec<u8> {
        let mut buf = BytesMut::new();
        encode_pstring(&mut buf, value, max_len, "test").unwrap();
        buf.to_vec()
    }

    fn build_deliver_v20_pdu(seq_id: u32, deliver: &DeliverV20) -> Vec<u8> {
        let body_size = 8 + 21 + 10 + 1 + 1 + 1 + 21 + 1 + 1 + deliver.msg_content.len();
        let total_len = PduHeader::SIZE + body_size;
        let mut pdu = Vec::with_capacity(total_len);
        pdu.extend_from_slice(&(total_len as u32).to_be_bytes());
        pdu.extend_from_slice(&0x00000005u32.to_be_bytes());
        pdu.extend_from_slice(&seq_id.to_be_bytes());
        pdu.extend_from_slice(&deliver.msg_id);
        pdu.extend_from_slice(&encode_pstring_fixed_for_test(&deliver.dest_id, 21));
        pdu.extend_from_slice(&encode_pstring_fixed_for_test(&deliver.service_id, 10));
        pdu.push(deliver.tppid);
        pdu.push(deliver.tpudhi);
        pdu.push(deliver.msg_fmt);
        pdu.extend_from_slice(&encode_pstring_fixed_for_test(&deliver.src_terminal_id, 21));
        pdu.push(deliver.registered_delivery);
        pdu.push(deliver.msg_content.len() as u8);
        pdu.extend_from_slice(&deliver.msg_content);
        pdu
    }

    #[test]
    fn test_decode_submit_v20_basic() {
        let mut submit = SubmitV20::new();
        submit.msg_id = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        submit.pk_total = 1;
        submit.pk_number = 1;
        submit.registered_delivery = 0;
        submit.msg_level = 1;
        submit.service_id = "SMS".to_string();
        submit.fee_user_type = 0;
        submit.fee_terminal_id = "13800138000".to_string();
        submit.tppid = 0;
        submit.tpudhi = 0;
        submit.msg_fmt = 15;
        submit.msg_src = "abc".to_string();
        submit.fee_type = "01".to_string();
        submit.fee_code = "000000".to_string();
        submit.valid_time = "".to_string();
        submit.at_time = "".to_string();
        submit.src_id = "10655000000".to_string();
        submit.dest_usr_tl = 1;
        submit.dest_terminal_ids = vec!["13800138000".to_string()];
        submit.msg_content = b"Hello CMPP2.0".to_vec();
        submit.reserve = [0u8; 8];

        let pdu_bytes = build_submit_v20_pdu(12345, &submit);
        let mut cursor = std::io::Cursor::new(pdu_bytes.as_slice());
        let header = PduHeader::decode(&mut cursor).unwrap();
        let decoded = decode_submit_v20(header, &mut cursor).unwrap();

        assert_eq!(decoded.pk_total, 1);
        assert_eq!(decoded.pk_number, 1);
        assert_eq!(decoded.registered_delivery, 0);
        assert_eq!(decoded.msg_level, 1);
        assert_eq!(decoded.service_id, "SMS");
        assert_eq!(decoded.fee_user_type, 0);
        assert_eq!(decoded.fee_terminal_id, "13800138000");
        assert_eq!(decoded.tppid, 0);
        assert_eq!(decoded.tpudhi, 0);
        assert_eq!(decoded.msg_fmt, 15);
        assert_eq!(decoded.msg_src, "abc");
        assert_eq!(decoded.fee_type, "01");
        assert_eq!(decoded.fee_code, "000000");
        assert_eq!(decoded.src_id, "10655000000");
        assert_eq!(decoded.dest_usr_tl, 1);
        assert_eq!(decoded.dest_terminal_ids, vec!["13800138000"]);
        assert_eq!(decoded.msg_content, b"Hello CMPP2.0");
    }

    #[test]
    fn test_decode_submit_v20_multiple_dest() {
        let mut submit = SubmitV20::new();
        submit.dest_usr_tl = 3;
        submit.dest_terminal_ids = vec![
            "13800138001".to_string(),
            "13800138002".to_string(),
            "13800138003".to_string(),
        ];
        submit.msg_content = b"Test".to_vec();
        submit.reserve = [0u8; 8];

        let pdu_bytes = build_submit_v20_pdu(1, &submit);
        let mut cursor = std::io::Cursor::new(pdu_bytes.as_slice());
        let header = PduHeader::decode(&mut cursor).unwrap();
        let decoded = decode_submit_v20(header, &mut cursor).unwrap();

        assert_eq!(decoded.dest_usr_tl, 3);
        assert_eq!(decoded.dest_terminal_ids.len(), 3);
    }

    #[test]
    fn test_submit_v20_to_submit_conversion() {
        let mut v20 = SubmitV20::new();
        v20.msg_id = [0x11; 8];
        v20.pk_total = 2;
        v20.pk_number = 1;
        v20.registered_delivery = 1;
        v20.msg_level = 5;
        v20.service_id = "SMS".to_string();
        v20.fee_user_type = 1;
        v20.fee_terminal_id = "13800138000".to_string();
        v20.tppid = 0;
        v20.tpudhi = 0;
        v20.msg_fmt = 8;
        v20.msg_src = "src".to_string();
        v20.fee_type = "02".to_string();
        v20.fee_code = "001234".to_string();
        v20.valid_time = "valid".to_string();
        v20.at_time = "at".to_string();
        v20.src_id = "10655000000".to_string();
        v20.dest_usr_tl = 2;
        v20.dest_terminal_ids = vec!["10086".to_string(), "10010".to_string()];
        v20.msg_content = b"Converted".to_vec();
        v20.reserve = [0u8; 8];

        let submit: crate::datatypes::Submit = v20.into();

        assert_eq!(submit.pk_total, 2);
        assert_eq!(submit.pk_number, 1);
        assert_eq!(submit.registered_delivery, 1);
        assert_eq!(submit.msg_level, 5);
        assert_eq!(submit.service_id, "SMS");
        assert_eq!(submit.fee_user_type, 1);
        assert_eq!(submit.fee_terminal_id, "13800138000");
        assert_eq!(submit.fee_terminal_type, 0);
        assert_eq!(submit.tppid, 0);
        assert_eq!(submit.tpudhi, 0);
        assert_eq!(submit.msg_fmt, 8);
        assert_eq!(submit.msg_src, "src");
        assert_eq!(submit.fee_type, "02");
        assert_eq!(submit.fee_code, "001234");
        assert_eq!(submit.valid_time, "valid");
        assert_eq!(submit.at_time, "at");
        assert_eq!(submit.src_id, "10655000000");
        assert_eq!(submit.dest_usr_tl, 2);
        assert_eq!(submit.dest_terminal_ids, vec!["10086", "10010"]);
        assert_eq!(submit.dest_terminal_type, 0);
        assert_eq!(submit.msg_content, b"Converted");
        assert_eq!(submit.link_id, "");
    }

    #[test]
    fn test_encoded_size_v20() {
        let mut submit = SubmitV20::new();
        submit.dest_usr_tl = 2;
        submit.dest_terminal_ids = vec!["13800138000".to_string(), "13800138001".to_string()];
        submit.msg_content = b"Hello".to_vec();
        submit.reserve = [0u8; 8];

        let size = encoded_size_v20(&submit);
        assert_eq!(
            size,
            8 + 1
                + 1
                + 1
                + 1
                + 10
                + 1
                + 21
                + 1
                + 1
                + 1
                + 6
                + 2
                + 6
                + 17
                + 17
                + 21
                + 1
                + (2 * 21)
                + 1
                + 5
                + 8
        );
    }

    #[test]
    fn test_decode_deliver_v20_basic() {
        let mut deliver = DeliverV20::new();
        deliver.msg_id = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        deliver.dest_id = "10655000000".to_string();
        deliver.service_id = "SMS".to_string();
        deliver.tppid = 0;
        deliver.tpudhi = 0;
        deliver.msg_fmt = 15;
        deliver.src_terminal_id = "13800138000".to_string();
        deliver.registered_delivery = 0;
        deliver.msg_content = b"Hello CMPP2.0 MO".to_vec();

        let pdu_bytes = build_deliver_v20_pdu(12345, &deliver);
        let mut cursor = std::io::Cursor::new(pdu_bytes.as_slice());
        let header = PduHeader::decode(&mut cursor).unwrap();
        let decoded = decode_deliver_v20(header, &mut cursor).unwrap();

        assert_eq!(
            decoded.msg_id,
            [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]
        );
        assert_eq!(decoded.dest_id, "10655000000");
        assert_eq!(decoded.service_id, "SMS");
        assert_eq!(decoded.tppid, 0);
        assert_eq!(decoded.tpudhi, 0);
        assert_eq!(decoded.msg_fmt, 15);
        assert_eq!(decoded.src_terminal_id, "13800138000");
        assert_eq!(decoded.registered_delivery, 0);
        assert_eq!(decoded.msg_content, b"Hello CMPP2.0 MO");
    }

    #[test]
    fn test_decode_deliver_v20_with_empty_content() {
        let mut deliver = DeliverV20::new();
        deliver.dest_id = "106900".to_string();
        deliver.service_id = "SMS".to_string();
        deliver.src_terminal_id = "13800138000".to_string();
        deliver.msg_content = vec![];

        let pdu_bytes = build_deliver_v20_pdu(1, &deliver);
        let mut cursor = std::io::Cursor::new(pdu_bytes.as_slice());
        let header = PduHeader::decode(&mut cursor).unwrap();
        let decoded = decode_deliver_v20(header, &mut cursor).unwrap();

        assert_eq!(decoded.dest_id, "106900");
        assert_eq!(decoded.src_terminal_id, "13800138000");
        assert!(decoded.msg_content.is_empty());
    }

    #[test]
    fn test_deliver_v20_to_deliver_conversion() {
        let mut v20 = DeliverV20::new();
        v20.msg_id = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x00, 0x11];
        v20.dest_id = "10655000000".to_string();
        v20.service_id = "MMS".to_string();
        v20.tppid = 1;
        v20.tpudhi = 1;
        v20.msg_fmt = 4;
        v20.src_terminal_id = "13900001111".to_string();
        v20.registered_delivery = 0;
        v20.msg_content = b"MO Reply".to_vec();

        let deliver: crate::datatypes::Deliver = v20.into();

        assert_eq!(
            deliver.msg_id,
            [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x00, 0x11]
        );
        assert_eq!(deliver.dest_id, "10655000000");
        assert_eq!(deliver.service_id, "MMS");
        assert_eq!(deliver.tppid, 1);
        assert_eq!(deliver.tpudhi, 1);
        assert_eq!(deliver.msg_fmt, 4);
        assert_eq!(deliver.src_terminal_id, "13900001111");
        assert_eq!(deliver.src_terminal_type, 0);
        assert_eq!(deliver.registered_delivery, 0);
        assert_eq!(deliver.msg_content, b"MO Reply");
        assert_eq!(deliver.link_id, "");
    }

    #[test]
    fn test_deliver_v20_field_sizes() {
        let mut deliver = DeliverV20::new();
        deliver.dest_id = "12345678901234567890X".to_string();
        deliver.src_terminal_id = "12345678901234567890X".to_string();

        let pdu_bytes = build_deliver_v20_pdu(1, &deliver);
        let mut cursor = std::io::Cursor::new(pdu_bytes.as_slice());
        let header = PduHeader::decode(&mut cursor).unwrap();

        assert_eq!(
            header.total_length as usize,
            PduHeader::SIZE + 8 + 21 + 10 + 1 + 1 + 1 + 21 + 1 + 1 + 0
        );
    }
}
