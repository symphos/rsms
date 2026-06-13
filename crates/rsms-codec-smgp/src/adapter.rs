//! SmgpAdapter：复用现有 decode_message/encode_message，做 SmgpMessage ↔ UnifiedMessage 翻译。

use crate::datatypes::{DeliverResp, SmgpMsgId, Submit, SubmitResp};
use crate::message::{decode_message, encode_message, SmgpMessage};
use rsms_core::{Frame, Protocol, Result, RsmsError};
use rsms_model::{
    Address, DeliveryStatus, Encoding, MessageId, ProtocolAdapter, ProtocolExtra, SmgpExtra,
    UnifiedBind, UnifiedBindResp, UnifiedDeliver, UnifiedMessage, UnifiedReport, UnifiedSubmit,
    UnifiedSubmitResp,
};

/// SMGP 协议适配器：实现 `ProtocolAdapter`，把 SMGP PDU 字节与统一消息模型互译。
pub struct SmgpAdapter;

// ──────────────────────────────────────────────
// 编码翻译表（SMGP ↔ 统一 Encoding）
// ──────────────────────────────────────────────

/// SMGP msg_fmt → 统一 Encoding。
fn encoding_from_fmt(fmt: u8) -> Encoding {
    match fmt {
        0 => Encoding::Ascii,
        4 => Encoding::Binary,
        8 => Encoding::Ucs2,
        15 => Encoding::Gbk,
        other => Encoding::Other(other),
    }
}

/// 统一 Encoding → SMGP msg_fmt。
fn fmt_from_encoding(enc: Encoding) -> u8 {
    match enc {
        Encoding::Ascii | Encoding::Gsm7 => 0,
        Encoding::Binary => 4,
        Encoding::Ucs2 => 8,
        Encoding::Gbk => 15,
        Encoding::Other(v) => v,
    }
}

// ──────────────────────────────────────────────
// Decode 方向：SmgpMessage → UnifiedMessage
// ──────────────────────────────────────────────

fn submit_to_unified(s: Submit) -> UnifiedSubmit {
    UnifiedSubmit {
        src: Address::plain(s.src_term_id),
        dests: s.dest_term_ids.into_iter().map(Address::plain).collect(),
        content: s.msg_content,
        encoding: encoding_from_fmt(s.msg_fmt),
        want_report: s.need_report != 0,
        // 长短信分片由 rsms-longmsg 在更上层处理，试点期 adapter 不拆 UDH
        concat: None,
        extra: ProtocolExtra::Smgp(SmgpExtra {
            msg_type: s.msg_type,
            priority: s.priority,
            service_id: s.service_id,
            fee_type: s.fee_type,
            fee_code: s.fee_code,
            fixed_fee: s.fixed_fee,
            charge_term_id: s.charge_term_id,
            valid_time: s.valid_time,
            at_time: s.at_time,
        }),
        tlvs: vec![], // optional_params → TLV 翻译留待后续步骤
    }
}

fn deliver_to_unified(d: crate::datatypes::Deliver) -> UnifiedMessage {
    if d.is_report != 0 {
        UnifiedMessage::Report(UnifiedReport {
            msg_id: MessageId::Binary(d.msg_id.bytes.to_vec()),
            status: DeliveryStatus::Unknown, // 精确状态解析留待后续
            dest: Address::plain(d.dest_term_id),
            raw: d.msg_content,
        })
    } else {
        UnifiedMessage::Deliver(UnifiedDeliver {
            src: Address::plain(d.src_term_id),
            dest: Address::plain(d.dest_term_id),
            content: d.msg_content,
            encoding: encoding_from_fmt(d.msg_fmt),
            concat: None,
            extra: ProtocolExtra::None,
            tlvs: vec![],
        })
    }
}

fn login_to_unified(l: crate::datatypes::Login) -> UnifiedBind {
    UnifiedBind {
        client_id: l.client_id,
        authenticator: l.authenticator.to_vec(),
        timestamp: l.timestamp,
        version: l.version,
    }
}

fn smgp_to_unified(msg: SmgpMessage) -> UnifiedMessage {
    match msg {
        SmgpMessage::Submit { submit, .. } => UnifiedMessage::Submit(submit_to_unified(submit)),
        SmgpMessage::SubmitResp { resp, .. } => UnifiedMessage::SubmitResp(UnifiedSubmitResp {
            msg_id: MessageId::Binary(resp.msg_id.bytes.to_vec()),
            status: resp.status,
        }),
        SmgpMessage::Deliver { deliver, .. } => deliver_to_unified(deliver),
        SmgpMessage::DeliverResp { .. } => UnifiedMessage::DeliverResp,
        SmgpMessage::Login { login, .. } => UnifiedMessage::Bind(login_to_unified(login)),
        SmgpMessage::LoginResp { resp, .. } => {
            UnifiedMessage::BindResp(UnifiedBindResp { status: resp.status })
        }
        SmgpMessage::ActiveTest { .. } => UnifiedMessage::Ping,
        SmgpMessage::ActiveTestResp { .. } => UnifiedMessage::PingResp,
        SmgpMessage::Exit { .. } => UnifiedMessage::Unbind,
        SmgpMessage::ExitResp { .. } => UnifiedMessage::UnbindResp,
        SmgpMessage::Unknown {
            command_id, body, ..
        } => UnifiedMessage::Unknown {
            command_id,
            raw: body,
        },
    }
}

// ──────────────────────────────────────────────
// Encode 方向：UnifiedMessage → SmgpMessage → bytes
// ──────────────────────────────────────────────

fn unified_to_smgp(msg: &UnifiedMessage, seq: u32) -> Result<SmgpMessage> {
    let m = match msg {
        UnifiedMessage::Submit(s) => {
            let extra = match &s.extra {
                ProtocolExtra::Smgp(e) => e.clone(),
                _ => SmgpExtra::default(),
            };
            let mut sub = Submit::new();
            sub.src_term_id = s.src.number.clone();
            sub.dest_term_ids = s.dests.iter().map(|a| a.number.clone()).collect();
            sub.dest_term_id_count = sub.dest_term_ids.len() as u8;
            sub.msg_content = s.content.clone();
            sub.msg_fmt = fmt_from_encoding(s.encoding);
            sub.need_report = if s.want_report { 1 } else { 0 };
            sub.msg_type = extra.msg_type;
            sub.priority = extra.priority;
            sub.service_id = extra.service_id;
            sub.fee_type = extra.fee_type;
            sub.fee_code = extra.fee_code;
            sub.fixed_fee = extra.fixed_fee;
            sub.charge_term_id = extra.charge_term_id;
            sub.valid_time = extra.valid_time;
            sub.at_time = extra.at_time;
            // reserve 保持默认 [0u8; 8]，optional_params 保持空——与 Submit::new() 默认一致
            SmgpMessage::Submit {
                sequence_id: seq,
                submit: sub,
            }
        }
        UnifiedMessage::SubmitResp(r) => {
            let bytes10 = match &r.msg_id {
                MessageId::Binary(b) => {
                    let mut arr = [0u8; 10];
                    let n = b.len().min(10);
                    arr[..n].copy_from_slice(&b[..n]);
                    arr
                }
                MessageId::Text(t) => {
                    let mut arr = [0u8; 10];
                    let tb = t.as_bytes();
                    let n = tb.len().min(10);
                    arr[..n].copy_from_slice(&tb[..n]);
                    arr
                }
            };
            SmgpMessage::SubmitResp {
                sequence_id: seq,
                resp: SubmitResp {
                    msg_id: SmgpMsgId::new(bytes10),
                    status: r.status,
                },
            }
        }
        UnifiedMessage::DeliverResp => SmgpMessage::DeliverResp {
            sequence_id: seq,
            resp: DeliverResp { status: 0 },
        },
        UnifiedMessage::Ping => SmgpMessage::ActiveTest { sequence_id: seq },
        UnifiedMessage::PingResp => SmgpMessage::ActiveTestResp { sequence_id: seq },
        UnifiedMessage::Unbind => SmgpMessage::Exit { sequence_id: seq },
        UnifiedMessage::UnbindResp => SmgpMessage::ExitResp { sequence_id: seq },
        other => {
            return Err(RsmsError::Other(format!(
                "SMGP encode 暂不支持该消息类型: {other:?}"
            )))
        }
    };
    Ok(m)
}

// ──────────────────────────────────────────────
// ProtocolAdapter 实现
// ──────────────────────────────────────────────

impl ProtocolAdapter for SmgpAdapter {
    fn protocol(&self) -> Protocol {
        Protocol::Smgp
    }

    /// 把完整 SMGP PDU 字节（含协议头）解码为统一消息。
    fn decode(&self, frame: &Frame) -> Result<UnifiedMessage> {
        let msg = decode_message(frame.data_as_slice())?;
        Ok(smgp_to_unified(msg))
    }

    /// 把统一消息编码为完整 SMGP PDU 字节（含协议头，写入 sequence_id）。
    fn encode(&self, msg: &UnifiedMessage, sequence_id: u32) -> Result<Vec<u8>> {
        let smgp = unified_to_smgp(msg, sequence_id)?;
        encode_message(&smgp)
    }
}

// ──────────────────────────────────────────────
// 测试
// ──────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::Pdu;
    use crate::datatypes::Submit;
    use rsms_core::{RawPdu};

    /// 用完整 PDU 字节构造 Frame（command_id/sequence_id 由 Frame::from 自动提取）。
    fn frame_of(bytes: Vec<u8>) -> Frame {
        Frame::from(RawPdu::from_vec(bytes))
    }

    #[test]
    fn decode_submit_to_unified() {
        let submit = Submit::new().with_message("1065900000", "13800138000", b"Hello");
        let bytes = Pdu::from(submit).to_pdu_bytes(7).to_vec();
        let unified = SmgpAdapter.decode(&frame_of(bytes)).unwrap();
        match unified {
            UnifiedMessage::Submit(s) => {
                assert_eq!(s.src.number, "1065900000");
                assert_eq!(s.dests[0].number, "13800138000");
                assert_eq!(s.content, b"Hello");
            }
            _ => panic!("expected Submit"),
        }
    }

    #[test]
    fn decode_deliver_report_vs_mo() {
        let mut d = crate::datatypes::Deliver::new();
        d.is_report = 1;
        d.dest_term_id = "1065900000".to_string();
        let bytes = Pdu::from(d).to_pdu_bytes(8).to_vec();
        let unified = SmgpAdapter.decode(&frame_of(bytes)).unwrap();
        assert!(
            matches!(unified, UnifiedMessage::Report(_)),
            "is_report=1 应翻译为 Report"
        );
    }

    #[test]
    fn submit_byte_roundtrip_via_unified() {
        // SMGP Submit → bytes → decode → UnifiedMessage → encode → bytes，字节应一致
        let submit = Submit::new().with_message("1065900000", "13800138000", b"Hello");
        let original = Pdu::from(submit).to_pdu_bytes(42).to_vec();
        let unified = SmgpAdapter.decode(&frame_of(original.clone())).unwrap();
        let reencoded = SmgpAdapter.encode(&unified, 42).unwrap();
        assert_eq!(reencoded, original, "经统一模型往返后字节应无损一致");
    }
}
