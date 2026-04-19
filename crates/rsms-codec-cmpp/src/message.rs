//! CMPP 2.0/3.0 消息解析与编码（统一使用 datatypes 中的定义）。

use crate::codec::PduHeader;
use crate::datatypes::{
    ActiveTest, ActiveTestResp, Cancel, CancelResp, CmppVersion, Connect, ConnectResp, Deliver,
    DeliverResp, DeliverV20, Query, QueryResp, Submit, SubmitResp, SubmitV20, Terminate,
    TerminateResp,
};
use crate::PduRegistry;
use rsms_core::RsmsError;
use std::io::Cursor;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CmppMessage {
    Connect {
        version: CmppVersion,
        sequence_id: u32,
        connect: Connect,
    },
    ConnectResp {
        version: CmppVersion,
        sequence_id: u32,
        resp: ConnectResp,
    },
    SubmitV20 {
        sequence_id: u32,
        submit: SubmitV20,
    },
    SubmitV30 {
        sequence_id: u32,
        submit: Submit,
    },
    DeliverV20 {
        sequence_id: u32,
        deliver: DeliverV20,
    },
    DeliverV30 {
        sequence_id: u32,
        deliver: Deliver,
    },
    SubmitResp {
        version: CmppVersion,
        sequence_id: u32,
        resp: SubmitResp,
    },
    DeliverResp {
        version: CmppVersion,
        sequence_id: u32,
        resp: DeliverResp,
    },
    Query {
        version: CmppVersion,
        sequence_id: u32,
        query: Query,
    },
    QueryResp {
        version: CmppVersion,
        sequence_id: u32,
        resp: QueryResp,
    },
    Cancel {
        version: CmppVersion,
        sequence_id: u32,
        cancel: Cancel,
    },
    CancelResp {
        version: CmppVersion,
        sequence_id: u32,
        resp: CancelResp,
    },
    ActiveTest {
        version: CmppVersion,
        sequence_id: u32,
    },
    ActiveTestResp {
        version: CmppVersion,
        sequence_id: u32,
    },
    Terminate {
        version: CmppVersion,
        sequence_id: u32,
    },
    TerminateResp {
        version: CmppVersion,
        sequence_id: u32,
    },
    Unknown {
        version: CmppVersion,
        command_id: u32,
        sequence_id: u32,
        body: Vec<u8>,
    },
}

impl CmppMessage {
    pub fn version(&self) -> CmppVersion {
        match self {
            CmppMessage::Connect { version, .. } => *version,
            CmppMessage::ConnectResp { version, .. } => *version,
            CmppMessage::SubmitV20 { .. } => CmppVersion::V20,
            CmppMessage::SubmitV30 { .. } => CmppVersion::V30,
            CmppMessage::DeliverV20 { .. } => CmppVersion::V20,
            CmppMessage::DeliverV30 { .. } => CmppVersion::V30,
            CmppMessage::SubmitResp { version, .. } => *version,
            CmppMessage::DeliverResp { version, .. } => *version,
            CmppMessage::Query { version, .. } => *version,
            CmppMessage::QueryResp { version, .. } => *version,
            CmppMessage::Cancel { version, .. } => *version,
            CmppMessage::CancelResp { version, .. } => *version,
            CmppMessage::ActiveTest { version, .. } => *version,
            CmppMessage::ActiveTestResp { version, .. } => *version,
            CmppMessage::Terminate { version, .. } => *version,
            CmppMessage::TerminateResp { version, .. } => *version,
            CmppMessage::Unknown { version, .. } => *version,
        }
    }

    pub fn sequence_id(&self) -> u32 {
        match self {
            CmppMessage::Connect { sequence_id, .. } => *sequence_id,
            CmppMessage::ConnectResp { sequence_id, .. } => *sequence_id,
            CmppMessage::SubmitV20 { sequence_id, .. } => *sequence_id,
            CmppMessage::SubmitV30 { sequence_id, .. } => *sequence_id,
            CmppMessage::DeliverV20 { sequence_id, .. } => *sequence_id,
            CmppMessage::DeliverV30 { sequence_id, .. } => *sequence_id,
            CmppMessage::SubmitResp { sequence_id, .. } => *sequence_id,
            CmppMessage::DeliverResp { sequence_id, .. } => *sequence_id,
            CmppMessage::Query { sequence_id, .. } => *sequence_id,
            CmppMessage::QueryResp { sequence_id, .. } => *sequence_id,
            CmppMessage::Cancel { sequence_id, .. } => *sequence_id,
            CmppMessage::CancelResp { sequence_id, .. } => *sequence_id,
            CmppMessage::ActiveTest { sequence_id, .. } => *sequence_id,
            CmppMessage::ActiveTestResp { sequence_id, .. } => *sequence_id,
            CmppMessage::Terminate { sequence_id, .. } => *sequence_id,
            CmppMessage::TerminateResp { sequence_id, .. } => *sequence_id,
            CmppMessage::Unknown { sequence_id, .. } => *sequence_id,
        }
    }
}

pub fn decode_message(pdu: &[u8]) -> Result<CmppMessage, RsmsError> {
    decode_message_with_version(pdu, None)
}

pub fn decode_message_with_version(
    pdu: &[u8],
    version: Option<u8>,
) -> Result<CmppMessage, RsmsError> {
    let version = version
        .and_then(|v| CmppVersion::from_wire(v).ok())
        .unwrap_or(CmppVersion::V30);

    let registry = PduRegistry::for_version(version);

    let mut cursor = Cursor::new(pdu);
    let header = PduHeader::decode(&mut cursor).map_err(|e| RsmsError::Codec(e.to_string()))?;
    let seq = header.sequence_id;
    let body = &pdu[PduHeader::SIZE..];

    let pdu = registry
        .dispatch(header, body)
        .map_err(|e| RsmsError::Codec(e.to_string()))?;

    let msg = match pdu {
        crate::codec::Pdu::Connect(c) => CmppMessage::Connect {
            version,
            sequence_id: seq,
            connect: c,
        },
        crate::codec::Pdu::ConnectResp(c) => CmppMessage::ConnectResp {
            version,
            sequence_id: seq,
            resp: c,
        },
        crate::codec::Pdu::Submit(s) => CmppMessage::SubmitV30 {
            sequence_id: seq,
            submit: s,
        },
        crate::codec::Pdu::Deliver(d) => CmppMessage::DeliverV30 {
            sequence_id: seq,
            deliver: d,
        },
        crate::codec::Pdu::SubmitResp(s) => CmppMessage::SubmitResp {
            version,
            sequence_id: seq,
            resp: s,
        },
        crate::codec::Pdu::DeliverResp(d) => CmppMessage::DeliverResp {
            version,
            sequence_id: seq,
            resp: d,
        },
        crate::codec::Pdu::SubmitV20(s) => CmppMessage::SubmitV20 {
            sequence_id: seq,
            submit: s,
        },
        crate::codec::Pdu::DeliverV20(d) => CmppMessage::DeliverV20 {
            sequence_id: seq,
            deliver: d,
        },
        crate::codec::Pdu::Query(q) => CmppMessage::Query {
            version,
            sequence_id: seq,
            query: q,
        },
        crate::codec::Pdu::QueryResp(r) => CmppMessage::QueryResp {
            version,
            sequence_id: seq,
            resp: r,
        },
        crate::codec::Pdu::Cancel(c) => CmppMessage::Cancel {
            version,
            sequence_id: seq,
            cancel: c,
        },
        crate::codec::Pdu::CancelResp(r) => CmppMessage::CancelResp {
            version,
            sequence_id: seq,
            resp: r,
        },
        crate::codec::Pdu::ActiveTest(_) => CmppMessage::ActiveTest {
            version,
            sequence_id: seq,
        },
        crate::codec::Pdu::ActiveTestResp(_) => CmppMessage::ActiveTestResp {
            version,
            sequence_id: seq,
        },
        crate::codec::Pdu::Terminate(_) => CmppMessage::Terminate {
            version,
            sequence_id: seq,
        },
        crate::codec::Pdu::TerminateResp(_) => CmppMessage::TerminateResp {
            version,
            sequence_id: seq,
        },
    };

    Ok(msg)
}

pub fn encode_message(msg: &CmppMessage) -> Result<Vec<u8>, RsmsError> {
    let seq = msg.sequence_id();
    let pdu = match msg {
        CmppMessage::Connect { connect, .. } => crate::codec::Pdu::Connect(connect.clone()),
        CmppMessage::ConnectResp { resp, .. } => crate::codec::Pdu::ConnectResp(resp.clone()),
        CmppMessage::SubmitV20 { submit, .. } => crate::codec::Pdu::SubmitV20(submit.clone()),
        CmppMessage::SubmitV30 { submit, .. } => crate::codec::Pdu::Submit(submit.clone()),
        CmppMessage::DeliverV20 { deliver, .. } => crate::codec::Pdu::DeliverV20(deliver.clone()),
        CmppMessage::DeliverV30 { deliver, .. } => crate::codec::Pdu::Deliver(deliver.clone()),
        CmppMessage::SubmitResp { resp, .. } => crate::codec::Pdu::SubmitResp(resp.clone()),
        CmppMessage::DeliverResp { resp, .. } => crate::codec::Pdu::DeliverResp(resp.clone()),
        CmppMessage::Query { query, .. } => crate::codec::Pdu::Query(query.clone()),
        CmppMessage::QueryResp { resp, .. } => crate::codec::Pdu::QueryResp(resp.clone()),
        CmppMessage::Cancel { cancel, .. } => crate::codec::Pdu::Cancel(cancel.clone()),
        CmppMessage::CancelResp { resp, .. } => crate::codec::Pdu::CancelResp(resp.clone()),
        CmppMessage::ActiveTest { .. } => crate::codec::Pdu::ActiveTest(ActiveTest),
        CmppMessage::ActiveTestResp { .. } => {
            crate::codec::Pdu::ActiveTestResp(ActiveTestResp { reserved: 0 })
        }
        CmppMessage::Terminate { .. } => crate::codec::Pdu::Terminate(Terminate),
        CmppMessage::TerminateResp { .. } => crate::codec::Pdu::TerminateResp(TerminateResp),
        CmppMessage::Unknown {
            command_id, body, ..
        } => {
            let body_len = body.len();
            let total = (12 + body_len) as u32;
            let mut v = Vec::with_capacity(total as usize);
            v.extend_from_slice(&total.to_be_bytes());
            v.extend_from_slice(&command_id.to_be_bytes());
            v.extend_from_slice(&seq.to_be_bytes());
            v.extend_from_slice(body);
            return Ok(v);
        }
    };
    Ok(pdu.to_pdu_bytes(seq).to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::Pdu;
    use crate::datatypes::encoded_size_v20;
    use bytes::BytesMut;
    use rsms_core::encode_pstring;

    fn encode_pstring_fixed(value: &str, max_len: usize) -> Vec<u8> {
        let mut buf = BytesMut::new();
        encode_pstring(&mut buf, value, max_len, "test").unwrap();
        buf.to_vec()
    }

    fn build_submit_v20_pdu(seq_id: u32, submit: &crate::SubmitV20) -> Vec<u8> {
        let body_size = encoded_size_v20(submit) - 8;
        let total_len = PduHeader::SIZE + body_size;
        let mut pdu = Vec::with_capacity(total_len);
        pdu.extend_from_slice(&(total_len as u32).to_be_bytes());
        pdu.extend_from_slice(&0x00000004u32.to_be_bytes());
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

    #[test]
    fn roundtrip_active_test_hex() {
        let hex = "0000000c0000000800000001";
        let raw: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        let m = decode_message(&raw).unwrap();
        match m {
            CmppMessage::ActiveTest {
                version: _,
                sequence_id,
            } => assert_eq!(sequence_id, 1),
            _ => panic!("expected active test"),
        }
        let out = encode_message(&m).unwrap();
        assert_eq!(out, raw);
    }

    #[test]
    fn pipeline_order_documented() {
        let pdu: Vec<u8> = (0.."0000000c0000000800000001".len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&"0000000c0000000800000001"[i..i + 2], 16).unwrap())
            .collect();
        let _ = crate::frame::decode_frames(&mut bytes::BytesMut::from(&pdu[..])).unwrap();
        let _ = decode_message(&pdu).unwrap();
    }

    #[test]
    fn test_decode_message_with_version_v20_submit() {
        let mut submit = crate::SubmitV20::new();
        submit.msg_id = [0x01; 8];
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
        submit.msg_src = "src".to_string();
        submit.fee_type = "01".to_string();
        submit.fee_code = "000000".to_string();
        submit.src_id = "10655000000".to_string();
        submit.dest_usr_tl = 1;
        submit.dest_terminal_ids = vec!["13800138000".to_string()];
        submit.msg_content = b"Test V2.0".to_vec();
        submit.reserve = [0u8; 8];

        let pdu_bytes = build_submit_v20_pdu(12345, &submit);

        let msg = decode_message_with_version(&pdu_bytes, Some(0x20)).unwrap();
        match msg {
            CmppMessage::SubmitV20 {
                sequence_id,
                submit: s,
            } => {
                assert_eq!(sequence_id, 12345);
                assert_eq!(s.msg_fmt, 15);
                assert_eq!(s.msg_content, b"Test V2.0");
                assert_eq!(s.src_id, "10655000000");
            }
            _ => panic!("expected SubmitV20 message"),
        }
    }

    #[test]
    fn test_decode_message_with_version_v30_submit() {
        let submit = Submit::new();
        let pdu: Pdu = submit.into();
        let pdu_bytes: Vec<u8> = pdu.to_pdu_bytes(54321).to_vec();

        let msg = decode_message_with_version(&pdu_bytes, Some(0x30)).unwrap();
        match msg {
            CmppMessage::SubmitV30 { sequence_id, .. } => {
                assert_eq!(sequence_id, 54321);
            }
            _ => panic!("expected SubmitV30 message"),
        }
    }

    #[test]
    fn test_decode_message_with_version_none_uses_v30() {
        let submit = Submit::new();
        let pdu: Pdu = submit.into();
        let pdu_bytes: Vec<u8> = pdu.to_pdu_bytes(99999).to_vec();

        let msg = decode_message_with_version(&pdu_bytes, None).unwrap();
        match msg {
            CmppMessage::SubmitV30 { sequence_id, .. } => {
                assert_eq!(sequence_id, 99999);
            }
            _ => panic!("expected SubmitV30 message"),
        }
    }

    #[test]
    fn test_decode_message_version_00_and_01_treated_as_v2() {
        for version in &[0x00u8, 0x01] {
            let mut submit = crate::SubmitV20::new();
            submit.dest_usr_tl = 1;
            submit.dest_terminal_ids = vec!["13800138000".to_string()];
            submit.msg_content = b"V2".to_vec();
            submit.reserve = [0u8; 8];

            let pdu_bytes = build_submit_v20_pdu(1, &submit);

            let msg = decode_message_with_version(&pdu_bytes, Some(*version)).unwrap();
            match msg {
                CmppMessage::SubmitV20 { submit: s, .. } => {
                    assert_eq!(s.dest_usr_tl, 1);
                }
                _ => panic!("expected SubmitV20 message for version {:02x}", version),
            }
        }
    }
}
