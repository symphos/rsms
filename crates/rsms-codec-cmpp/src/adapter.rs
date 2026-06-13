//! CmppAdapter：复用 decode_message/encode_message，做 CmppMessage ↔ UnifiedMessage 翻译。
//! 本轮仅 V3.0（无状态 decode 默认 V3.0；V2.0 需握手版本上下文，见计划范围边界）。
//!
//! 已知限制（本轮）：encode 不支持 Bind/Deliver/Report（仅 Submit/SubmitResp/心跳/解绑，
//! 与其它 adapter 一致）；Deliver 报告路径 status 暂为 Unknown（精确状态解析待后续）。

use crate::datatypes::{CmppVersion, CommandId, Connect, Deliver, Submit, SubmitResp};
use crate::message::{decode_message, encode_message, CmppMessage};
use rsms_core::{Frame, Protocol, Result, RsmsError};
use rsms_model::{
    Address, CmppExtra, DeliveryStatus, Encoding, MessageId, ProtocolAdapter, ProtocolExtra,
    UnifiedBind, UnifiedDeliver, UnifiedMessage, UnifiedReport, UnifiedSubmit, UnifiedSubmitResp,
};

/// CMPP 协议适配器（V3.0）。
pub struct CmppAdapter;

// ── 编码翻译表（CMPP msg_fmt ↔ 统一 Encoding）──
fn encoding_from_fmt(fmt: u8) -> Encoding {
    match fmt {
        0 => Encoding::Ascii,
        4 => Encoding::Binary,
        8 => Encoding::Ucs2,
        15 => Encoding::Gbk,
        other => Encoding::Other(other),
    }
}
fn fmt_from_encoding(enc: Encoding) -> u8 {
    match enc {
        Encoding::Ascii | Encoding::Gsm7 => 0,
        Encoding::Binary => 4,
        Encoding::Ucs2 => 8,
        Encoding::Gbk => 15,
        Encoding::Other(v) => v,
    }
}

fn submit_v30_to_unified(s: Submit) -> UnifiedSubmit {
    UnifiedSubmit {
        src: Address::plain(s.src_id),
        dests: s.dest_terminal_ids.into_iter().map(Address::plain).collect(),
        content: s.msg_content,
        encoding: encoding_from_fmt(s.msg_fmt),
        want_report: s.registered_delivery != 0,
        concat: None,
        extra: ProtocolExtra::Cmpp(CmppExtra {
            msg_id: s.msg_id,
            pk_total: s.pk_total,
            pk_number: s.pk_number,
            msg_level: s.msg_level,
            service_id: s.service_id,
            fee_user_type: s.fee_user_type,
            fee_terminal_id: s.fee_terminal_id,
            fee_terminal_type: s.fee_terminal_type,
            tppid: s.tppid,
            tpudhi: s.tpudhi,
            msg_src: s.msg_src,
            fee_type: s.fee_type,
            fee_code: s.fee_code,
            valid_time: s.valid_time,
            at_time: s.at_time,
            dest_terminal_type: s.dest_terminal_type,
            link_id: s.link_id,
        }),
        tlvs: vec![],
    }
}

fn deliver_v30_to_unified(d: Deliver) -> UnifiedMessage {
    if d.registered_delivery == 1 {
        UnifiedMessage::Report(UnifiedReport {
            msg_id: MessageId::Binary(d.msg_id.to_vec()),
            status: DeliveryStatus::Unknown,
            dest: Address::plain(d.dest_id),
            raw: d.msg_content,
        })
    } else {
        UnifiedMessage::Deliver(UnifiedDeliver {
            src: Address::plain(d.src_terminal_id),
            dest: Address::plain(d.dest_id),
            content: d.msg_content,
            encoding: encoding_from_fmt(d.msg_fmt),
            concat: None,
            extra: ProtocolExtra::None,
            tlvs: vec![],
        })
    }
}

fn connect_to_unified(c: Connect) -> UnifiedBind {
    UnifiedBind {
        client_id: c.source_addr,
        authenticator: c.authenticator_source.to_vec(),
        timestamp: c.timestamp,
        version: c.version,
    }
}

fn cmpp_to_unified(msg: CmppMessage) -> UnifiedMessage {
    match msg {
        CmppMessage::SubmitV30 { submit, .. } => UnifiedMessage::Submit(submit_v30_to_unified(submit)),
        CmppMessage::SubmitResp { resp, .. } => UnifiedMessage::SubmitResp(UnifiedSubmitResp {
            msg_id: MessageId::Binary(resp.msg_id.to_vec()),
            status: resp.result,
        }),
        CmppMessage::DeliverV30 { deliver, .. } => deliver_v30_to_unified(deliver),
        CmppMessage::DeliverResp { .. } => UnifiedMessage::DeliverResp,
        CmppMessage::Connect { connect, .. } => UnifiedMessage::Bind(connect_to_unified(connect)),
        CmppMessage::ConnectResp { resp, .. } => {
            UnifiedMessage::BindResp(rsms_model::UnifiedBindResp { status: resp.status })
        }
        CmppMessage::ActiveTest { .. } => UnifiedMessage::Ping,
        CmppMessage::ActiveTestResp { .. } => UnifiedMessage::PingResp,
        CmppMessage::Terminate { .. } => UnifiedMessage::Unbind,
        CmppMessage::TerminateResp { .. } => UnifiedMessage::UnbindResp,
        CmppMessage::Unknown { command_id, body, .. } => {
            UnifiedMessage::Unknown { command_id, raw: body }
        }
        // V2.0 Submit/Deliver、Query/Cancel 等本轮退化为 Unknown（仅 shadow 日志可见），保留真实
        // command_id 便于诊断；match 改为穷尽，未来新增变体触发编译错误而非静默归零。
        CmppMessage::SubmitV20 { .. } => {
            UnifiedMessage::Unknown { command_id: CommandId::Submit as u32, raw: vec![] }
        }
        CmppMessage::DeliverV20 { .. } => {
            UnifiedMessage::Unknown { command_id: CommandId::Deliver as u32, raw: vec![] }
        }
        CmppMessage::Query { .. } => {
            UnifiedMessage::Unknown { command_id: CommandId::Query as u32, raw: vec![] }
        }
        CmppMessage::QueryResp { .. } => {
            UnifiedMessage::Unknown { command_id: CommandId::QueryResp as u32, raw: vec![] }
        }
        CmppMessage::Cancel { .. } => {
            UnifiedMessage::Unknown { command_id: CommandId::Cancel as u32, raw: vec![] }
        }
        CmppMessage::CancelResp { .. } => {
            UnifiedMessage::Unknown { command_id: CommandId::CancelResp as u32, raw: vec![] }
        }
    }
}

// ── Encode 方向：UnifiedMessage → CmppMessage(V3.0) → encode_message ──
fn unified_to_cmpp(msg: &UnifiedMessage, seq: u32) -> Result<CmppMessage> {
    let m = match msg {
        UnifiedMessage::Submit(s) => {
            let extra = match &s.extra {
                ProtocolExtra::Cmpp(e) => e.clone(),
                _ => CmppExtra::default(),
            };
            let mut sub = Submit::new();
            sub.msg_id = extra.msg_id;
            sub.pk_total = extra.pk_total;
            sub.pk_number = extra.pk_number;
            sub.registered_delivery = if s.want_report { 1 } else { 0 };
            sub.msg_level = extra.msg_level;
            sub.service_id = extra.service_id;
            sub.fee_user_type = extra.fee_user_type;
            sub.fee_terminal_id = extra.fee_terminal_id;
            sub.fee_terminal_type = extra.fee_terminal_type;
            sub.tppid = extra.tppid;
            sub.tpudhi = extra.tpudhi;
            sub.msg_fmt = fmt_from_encoding(s.encoding);
            sub.msg_src = extra.msg_src;
            sub.fee_type = extra.fee_type;
            sub.fee_code = extra.fee_code;
            sub.valid_time = extra.valid_time;
            sub.at_time = extra.at_time;
            sub.src_id = s.src.number.clone();
            sub.dest_usr_tl = s.dests.len() as u8;
            sub.dest_terminal_ids = s.dests.iter().map(|a| a.number.clone()).collect();
            sub.dest_terminal_type = extra.dest_terminal_type;
            sub.msg_content = s.content.clone();
            sub.link_id = extra.link_id;
            CmppMessage::SubmitV30 { sequence_id: seq, submit: sub }
        }
        UnifiedMessage::SubmitResp(r) => {
            let mut msg_id = [0u8; 8];
            if let MessageId::Binary(b) = &r.msg_id {
                let n = b.len().min(8);
                msg_id[..n].copy_from_slice(&b[..n]);
            }
            CmppMessage::SubmitResp {
                version: CmppVersion::V30,
                sequence_id: seq,
                resp: SubmitResp { msg_id, result: r.status },
            }
        }
        UnifiedMessage::Ping => CmppMessage::ActiveTest { version: CmppVersion::V30, sequence_id: seq },
        UnifiedMessage::PingResp => CmppMessage::ActiveTestResp { version: CmppVersion::V30, sequence_id: seq },
        UnifiedMessage::Unbind => CmppMessage::Terminate { version: CmppVersion::V30, sequence_id: seq },
        UnifiedMessage::UnbindResp => CmppMessage::TerminateResp { version: CmppVersion::V30, sequence_id: seq },
        other => {
            return Err(RsmsError::Other(format!(
                "CMPP encode 暂不支持该消息类型: {other:?}"
            )))
        }
    };
    Ok(m)
}

impl ProtocolAdapter for CmppAdapter {
    fn protocol(&self) -> Protocol {
        Protocol::Cmpp
    }
    fn decode(&self, frame: &Frame) -> Result<UnifiedMessage> {
        let msg = decode_message(frame.data_as_slice())?;
        Ok(cmpp_to_unified(msg))
    }
    fn encode(&self, msg: &UnifiedMessage, sequence_id: u32) -> Result<Vec<u8>> {
        let cmpp = unified_to_cmpp(msg, sequence_id)?;
        encode_message(&cmpp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::Pdu;
    use crate::datatypes::{Deliver, Submit};
    use rsms_core::RawPdu;

    fn frame_of(bytes: Vec<u8>) -> Frame {
        Frame::from(RawPdu::from_vec(bytes))
    }

    #[test]
    fn decode_submit_v30_billing_fields() {
        let mut s = Submit::new();
        s.src_id = "1065900000".to_string();
        s.dest_terminal_ids = vec!["13800138000".to_string()];
        s.dest_usr_tl = 1;
        s.msg_content = b"Hello".to_vec();
        s.fee_type = "01".to_string();
        s.fee_code = "000100".to_string();
        let pdu: Pdu = s.into();
        let bytes = pdu.to_pdu_bytes(7).to_vec();
        match CmppAdapter.decode(&frame_of(bytes)).unwrap() {
            UnifiedMessage::Submit(u) => {
                assert_eq!(u.src.number, "1065900000");
                assert_eq!(u.dests[0].number, "13800138000");
                match &u.extra {
                    ProtocolExtra::Cmpp(e) => {
                        assert_eq!(e.fee_type, "01");
                        assert_eq!(e.fee_code, "000100");
                    }
                    _ => panic!("extra 应为 Cmpp"),
                }
            }
            _ => panic!("expected Submit"),
        }
    }

    #[test]
    fn decode_deliver_report_via_registered_delivery() {
        let mut d = Deliver::new();
        d.registered_delivery = 1;
        d.dest_id = "1065900000".to_string();
        let pdu: Pdu = d.into();
        let bytes = pdu.to_pdu_bytes(8).to_vec();
        assert!(matches!(
            CmppAdapter.decode(&frame_of(bytes)).unwrap(),
            UnifiedMessage::Report(_)
        ));
    }

    #[test]
    fn submit_v30_byte_roundtrip_via_unified() {
        let mut s = Submit::new();
        s.src_id = "1065900000".to_string();
        s.dest_terminal_ids = vec!["13800138000".to_string()];
        s.dest_usr_tl = 1;
        s.msg_fmt = 8;
        s.msg_content = b"\x4e\x2d\x65\x87".to_vec();
        s.registered_delivery = 1;
        s.service_id = "SVC".to_string();
        s.fee_type = "01".to_string();
        s.fee_code = "000100".to_string();
        s.msg_src = "900001".to_string();
        let pdu: Pdu = s.into();
        let original = pdu.to_pdu_bytes(42).to_vec();

        let unified = CmppAdapter.decode(&frame_of(original.clone())).unwrap();
        let reencoded = CmppAdapter.encode(&unified, 42).unwrap();
        assert_eq!(reencoded, original, "CMPP V3.0 Submit 经统一模型往返后字节应无损一致");
    }
}
