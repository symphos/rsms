//! CMPP 2.0/3.0 消息解析与编码（统一使用 datatypes 中的定义）。

use crate::codec::{Encodable, PduHeader};
use crate::datatypes::{
    ActiveTest, ActiveTestResp, Cancel, CancelResp, CmppVersion, Connect, ConnectResp,
    ConnectRespV20, Deliver, DeliverResp, DeliverRespV20, DeliverV20, Query, QueryResp, Submit,
    SubmitResp, SubmitRespV20, SubmitV20, Terminate, TerminateResp,
};
use crate::PduRegistry;
use bytes::{BufMut, BytesMut};
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

/// 解码一条 CMPP PDU 字节序列为 `CmppMessage`，版本默认为 V3.0。
///
/// 等价于 `decode_message_with_version(pdu, None)`。
pub fn decode_message(pdu: &[u8]) -> Result<CmppMessage, RsmsError> {
    decode_message_with_version(pdu, None)
}

/// 解码一条 CMPP PDU 字节序列为 `CmppMessage`，并指定协议版本。
///
/// `version` 为协议版本字节（如 `0x20` = V2.0，`0x30` = V3.0）；
/// 传入 `None` 或无法识别的值时默认按 V3.0 解码。
/// Submit 和 Deliver 的 V2.0/V3.0 结构不同，版本错误会导致解码失败或字段偏移错误。
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

/// 将 `CmppMessage` 编码为完整的 PDU 字节序列（含协议头）。
///
/// `Unknown` 变体会将原始 body 原样写回，不做任何校验。
pub fn encode_message(msg: &CmppMessage) -> Result<Vec<u8>, RsmsError> {
    use crate::datatypes::CommandId;

    let seq = msg.sequence_id();

    // ── 版本感知早 return：CMPP 2.0 应答类 PDU 的 Result/Status 仅 1 字节，字段宽度与
    // V3.0 不同（公共 Pdu::*.to_pdu_bytes 写 u32）。改由 *RespV20 类型的 Encodable 写 V2.0
    // 窄 body（与其 Decodable 对称，字节宽度知识住在数据类型层）。version 非 V20 落常规路径。──
    fn encode_v20_pdu<T: Encodable>(command_id: CommandId, seq: u32, pdu: &T) -> Vec<u8> {
        let body_size = pdu.encoded_size();
        let mut buf = BytesMut::with_capacity(PduHeader::SIZE + body_size);
        buf.put_u32((PduHeader::SIZE + body_size) as u32);
        buf.put_u32(command_id as u32);
        buf.put_u32(seq);
        pdu.encode(&mut buf).expect("V2.0 应答 body 编码不应失败");
        buf.to_vec()
    }
    match msg {
        CmppMessage::SubmitResp {
            version: CmppVersion::V20,
            resp,
            ..
        } => {
            let v20 = SubmitRespV20 {
                msg_id: resp.msg_id,
                result: resp.result as u8,
            };
            return Ok(encode_v20_pdu(CommandId::SubmitResp, seq, &v20));
        }
        CmppMessage::DeliverResp {
            version: CmppVersion::V20,
            resp,
            ..
        } => {
            let v20 = DeliverRespV20 {
                msg_id: resp.msg_id,
                result: resp.result as u8,
            };
            return Ok(encode_v20_pdu(CommandId::DeliverResp, seq, &v20));
        }
        CmppMessage::ConnectResp {
            version: CmppVersion::V20,
            resp,
            ..
        } => {
            let v20 = ConnectRespV20 {
                status: resp.status as u8,
                authenticator_ismg: resp.authenticator_ismg,
                version: resp.version,
            };
            return Ok(encode_v20_pdu(CommandId::ConnectResp, seq, &v20));
        }
        _ => {}
    }

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
    fn encode_submit_resp_v20_is_21b_total() {
        // V2.0 SubmitResp：body = Msg_Id(8) + Result(1) = 9B，总长 12+9 = 21B。
        let msg = CmppMessage::SubmitResp {
            version: CmppVersion::V20,
            sequence_id: 7,
            resp: SubmitResp {
                msg_id: [0x11u8, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88],
                result: 9, // 由 u32 截到 u8 写线路
            },
        };
        let bytes = encode_message(&msg).unwrap();
        assert_eq!(bytes.len(), 21, "V2.0 SubmitResp 总长应为 21B（9B body）");
        // total_length 头字段。
        assert_eq!(&bytes[0..4], &21u32.to_be_bytes());
        assert_eq!(&bytes[4..8], &(crate::datatypes::CommandId::SubmitResp as u32).to_be_bytes());
        assert_eq!(&bytes[8..12], &7u32.to_be_bytes());
        // body：msg_id(8) + result(1)。
        assert_eq!(&bytes[12..20], &[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]);
        assert_eq!(bytes[20], 9);
        // 版本感知 decode 回来应是 SubmitResp，result 提升回 u32。
        let back = decode_message_with_version(&bytes, Some(0x20)).unwrap();
        match back {
            CmppMessage::SubmitResp { resp, .. } => assert_eq!(resp.result, 9u32),
            other => panic!("expected SubmitResp, got {other:?}"),
        }
    }

    #[test]
    fn encode_deliver_resp_v20_is_21b_total() {
        // V2.0 DeliverResp：body = Msg_Id(8) + Result(1) = 9B，总长 21B（与 SubmitResp 同形，仅 command_id 不同）。
        let msg = CmppMessage::DeliverResp {
            version: CmppVersion::V20,
            sequence_id: 3,
            resp: DeliverResp {
                msg_id: [0xA1u8, 0xB2, 0xC3, 0xD4, 0xE5, 0xF6, 0x07, 0x18],
                result: 5,
            },
        };
        let bytes = encode_message(&msg).unwrap();
        assert_eq!(bytes.len(), 21, "V2.0 DeliverResp 总长应为 21B（9B body）");
        assert_eq!(&bytes[0..4], &21u32.to_be_bytes());
        assert_eq!(&bytes[4..8], &(crate::datatypes::CommandId::DeliverResp as u32).to_be_bytes());
        assert_eq!(&bytes[8..12], &3u32.to_be_bytes());
        assert_eq!(&bytes[12..20], &[0xA1, 0xB2, 0xC3, 0xD4, 0xE5, 0xF6, 0x07, 0x18]);
        assert_eq!(bytes[20], 5);
        let back = decode_message_with_version(&bytes, Some(0x20)).unwrap();
        match back {
            CmppMessage::DeliverResp { resp, .. } => assert_eq!(resp.result, 5u32),
            other => panic!("expected DeliverResp, got {other:?}"),
        }
    }

    #[test]
    fn encode_connect_resp_v20_is_30b_total() {
        // V2.0 ConnectResp：body = Status(1) + ISMG(16) + Version(1) = 18B，总长 30B。
        let msg = CmppMessage::ConnectResp {
            version: CmppVersion::V20,
            sequence_id: 1,
            resp: ConnectResp {
                status: 0,
                authenticator_ismg: [0u8; 16],
                version: 0x20,
            },
        };
        let bytes = encode_message(&msg).unwrap();
        assert_eq!(bytes.len(), 30, "V2.0 ConnectResp 总长应为 30B（18B body）");
        assert_eq!(bytes[12], 0); // status u8
        assert_eq!(bytes[29], 0x20); // version
    }

    #[test]
    fn encode_submit_resp_v30_stays_24b() {
        // 回归：V3.0 SubmitResp 不受影响，仍 12+12 = 24B（Result u32）。
        let msg = CmppMessage::SubmitResp {
            version: CmppVersion::V30,
            sequence_id: 7,
            resp: SubmitResp {
                msg_id: [0u8; 8],
                result: 0,
            },
        };
        let bytes = encode_message(&msg).unwrap();
        assert_eq!(bytes.len(), 24, "V3.0 SubmitResp 总长应仍为 24B（12B body）");
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

    /// Fuzz regression: a remote peer must not be able to crash the decode path
    /// by sending a truncated/short PDU body. For every body-carrying command id
    /// and every short body length, `decode_message_with_version` must return
    /// (Ok or Err) without ever panicking.
    #[test]
    fn decode_message_short_body_never_panics() {
        // Command ids that carry a body (requests + responses with payload).
        let command_ids: [u32; 12] = [
            0x00000001, // Connect
            0x80000001, // ConnectResp
            0x00000004, // Submit
            0x80000004, // SubmitResp
            0x00000005, // Deliver
            0x80000005, // DeliverResp
            0x00000006, // Query
            0x80000006, // QueryResp
            0x00000007, // Cancel
            0x80000007, // CancelResp
            // Submit/SubmitV20 share command id 0x04; exercised above. Include
            // ActiveTestResp which also carries a 1-byte body.
            0x80000008, // ActiveTestResp
            0x00000002, // Terminate (no body, included for completeness)
        ];

        for &command_id in &command_ids {
            for n in 0u32..48 {
                let total_length = PduHeader::SIZE as u32 + n;
                let mut pdu = vec![0u8; PduHeader::SIZE + n as usize];
                pdu[0..4].copy_from_slice(&total_length.to_be_bytes());
                pdu[4..8].copy_from_slice(&command_id.to_be_bytes());
                // sequence_id (bytes 8..12) left as zero.

                // Must not panic for either CMPP version.
                let _ = decode_message_with_version(&pdu, Some(0x30));
                let _ = decode_message_with_version(&pdu, Some(0x20));
            }
        }
    }
}
