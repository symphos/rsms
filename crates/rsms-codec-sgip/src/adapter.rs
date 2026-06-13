//! SgipAdapter：复用 decode_message + Pdu::to_pdu_bytes，做 SgipMessage ↔ UnifiedMessage 翻译。
//! 验证窄腰对「独立 Report 命令」的吸收能力。
//!
//! 约定：encode 方向 header 的 SgipSequence 固定 node_id=0, timestamp=0, number=sequence_id
//! （decode_message 丢弃 header SgipSequence，统一模型不携带它；真实连接序列生成在编排层别处）。

use crate::codec::Encodable;
use crate::datatypes::{CommandId, DeliverResp, Report, SgipSequence, Submit, SubmitResp, Unbind, UnbindResp};
use crate::message::{decode_message, SgipMessage};
use rsms_core::{Frame, Protocol, Result, RsmsError};
use rsms_model::{
    Address, DeliveryStatus, Encoding, MessageId, ProtocolAdapter, ProtocolExtra, SgipExtra,
    UnifiedDeliver, UnifiedMessage, UnifiedReport, UnifiedSubmit, UnifiedSubmitResp,
};

/// SGIP 协议适配器。
pub struct SgipAdapter;

// ── 编码翻译表（SGIP msg_fmt ↔ 统一 Encoding）——取值不同于 SMGP/SMPP ──
fn encoding_from_fmt(fmt: u8) -> Encoding {
    match fmt {
        0 => Encoding::Ascii,
        4 => Encoding::Ucs2,
        8 => Encoding::Binary,
        other => Encoding::Other(other),
    }
}
fn fmt_from_encoding(enc: Encoding) -> u8 {
    match enc {
        Encoding::Ascii | Encoding::Gsm7 => 0,
        Encoding::Ucs2 => 4,
        Encoding::Binary => 8,
        // SGIP 无 GBK 专属 msg_fmt，落 0（有损：回译为 Ascii）
        Encoding::Gbk => 0,
        Encoding::Other(v) => v,
    }
}

/// SGIP 状态码 → 统一 DeliveryStatus（state: 0=成功投递；其余取值——等待/投递中/已删除等——
/// 完整映射待后续，本轮非 0 一律归 Unknown）。
fn status_from_state(state: u8) -> DeliveryStatus {
    match state {
        0 => DeliveryStatus::Delivered,
        _ => DeliveryStatus::Unknown,
    }
}

/// 把 SgipSequence 编为 12 字节大端，装进 MessageId::Binary。
/// 仅作统一模型内部不透明标识（调用方按 MessageId 比对），不在适配器层反解回 SgipSequence。
fn seq_to_msg_id(seq: SgipSequence) -> MessageId {
    let mut b = Vec::with_capacity(12);
    b.extend_from_slice(&seq.node_id.to_be_bytes());
    b.extend_from_slice(&seq.timestamp.to_be_bytes());
    b.extend_from_slice(&seq.number.to_be_bytes());
    MessageId::Binary(b)
}

fn submit_to_unified(s: Submit) -> UnifiedSubmit {
    UnifiedSubmit {
        src: Address::plain(s.sp_number),
        dests: s.user_numbers.into_iter().map(Address::plain).collect(),
        content: s.message_content,
        encoding: encoding_from_fmt(s.msg_fmt),
        want_report: s.report_flag != 0,
        concat: None,
        extra: ProtocolExtra::Sgip(SgipExtra {
            charge_number: s.charge_number,
            corp_id: s.corp_id,
            service_type: s.service_type,
            fee_type: s.fee_type,
            fee_value: s.fee_value,
            given_value: s.given_value,
            agent_flag: s.agent_flag,
            morelate_to_mt_flag: s.morelate_to_mt_flag,
            priority: s.priority,
            expire_time: s.expire_time,
            schedule_time: s.schedule_time,
            tppid: s.tppid,
            tpudhi: s.tpudhi,
            message_type: s.message_type,
        }),
        tlvs: vec![],
    }
}

fn report_to_unified(r: Report) -> UnifiedReport {
    // raw 仅含 report_type/state/error_code 摘要；reserve(8B) 不入 raw。
    let raw = vec![r.report_type, r.state, r.error_code];
    UnifiedReport {
        msg_id: seq_to_msg_id(r.submit_sequence),
        status: status_from_state(r.state),
        dest: Address::plain(r.user_number),
        raw,
    }
}

fn sgip_to_unified(msg: SgipMessage) -> UnifiedMessage {
    match msg {
        SgipMessage::Submit(s) => UnifiedMessage::Submit(submit_to_unified(s)),
        // SGIP SubmitResp 仅含 result，无 msg_id 字段；统一模型 msg_id 置空。
        SgipMessage::SubmitResp(r) => UnifiedMessage::SubmitResp(UnifiedSubmitResp {
            msg_id: MessageId::Text(String::new()),
            status: r.result,
        }),
        SgipMessage::Deliver(d) => UnifiedMessage::Deliver(UnifiedDeliver {
            src: Address::plain(d.sp_number),
            dest: Address::plain(d.user_number),
            content: d.message_content,
            encoding: encoding_from_fmt(d.msg_fmt),
            concat: None,
            extra: ProtocolExtra::None,
            tlvs: vec![],
        }),
        SgipMessage::DeliverResp(_) => UnifiedMessage::DeliverResp,
        SgipMessage::Report(r) => UnifiedMessage::Report(report_to_unified(r)),
        // ReportResp 是独立 Report 命令的响应；统一模型暂无对应变体，退化为 Unknown 并保留真实
        // command_id，不可伪装成 DeliverResp（那是 MO-Deliver 的响应，语义不同、会在路由层误判）。
        SgipMessage::ReportResp(_) => UnifiedMessage::Unknown {
            command_id: CommandId::ReportResp as u32,
            raw: vec![],
        },
        SgipMessage::Bind(b) => UnifiedMessage::Bind(rsms_model::UnifiedBind {
            client_id: b.login_name,
            authenticator: b.login_password.into_bytes(),
            timestamp: 0,
            version: b.login_type,
        }),
        SgipMessage::BindResp(r) => {
            UnifiedMessage::BindResp(rsms_model::UnifiedBindResp { status: r.result as u32 })
        }
        SgipMessage::Unbind(_) => UnifiedMessage::Unbind,
        SgipMessage::UnbindResp(_) => UnifiedMessage::UnbindResp,
        // Trace/TraceResp 本轮退化为 Unknown（仅 shadow 日志可见），各自保留真实 command_id 便于诊断。
        SgipMessage::Trace(t) => UnifiedMessage::Unknown {
            command_id: CommandId::Trace as u32,
            raw: t.trace_value.into_bytes(),
        },
        SgipMessage::TraceResp(_) => UnifiedMessage::Unknown {
            command_id: CommandId::TraceResp as u32,
            raw: vec![],
        },
        SgipMessage::Unknown { command_id, body } => UnifiedMessage::Unknown { command_id, raw: body },
    }
}

// ── Encode 方向（约定 node_id=0, timestamp=0, number=sequence_id）──
// 注：SGIP 的 to_pdu_bytes 是 Encodable trait 方法，定义在各具体 PDU 类型上（非 Pdu 枚举），
// 故按消息类型分别在具体结构体上调用 to_pdu_bytes，不经 Pdu 中转。
fn unified_to_sgip_bytes(msg: &UnifiedMessage, seq: u32) -> Result<Vec<u8>> {
    let bytes = match msg {
        UnifiedMessage::Submit(s) => {
            let extra = match &s.extra {
                ProtocolExtra::Sgip(e) => e.clone(),
                _ => SgipExtra::default(),
            };
            let mut sub = Submit::new();
            sub.sp_number = s.src.number.clone();
            sub.charge_number = extra.charge_number;
            sub.user_count = s.dests.len() as u8;
            sub.user_numbers = s.dests.iter().map(|a| a.number.clone()).collect();
            sub.corp_id = extra.corp_id;
            sub.service_type = extra.service_type;
            sub.fee_type = extra.fee_type;
            sub.fee_value = extra.fee_value;
            sub.given_value = extra.given_value;
            sub.agent_flag = extra.agent_flag;
            sub.morelate_to_mt_flag = extra.morelate_to_mt_flag;
            sub.priority = extra.priority;
            sub.expire_time = extra.expire_time;
            sub.schedule_time = extra.schedule_time;
            sub.report_flag = if s.want_report { 1 } else { 0 };
            sub.tppid = extra.tppid;
            sub.tpudhi = extra.tpudhi;
            sub.msg_fmt = fmt_from_encoding(s.encoding);
            sub.message_type = extra.message_type;
            sub.message_content = s.content.clone();
            // reserve 保持 Submit::new() 默认 [0u8;8]
            sub.to_pdu_bytes(0, 0, seq)
        }
        UnifiedMessage::SubmitResp(r) => SubmitResp { result: r.status }.to_pdu_bytes(0, 0, seq),
        UnifiedMessage::DeliverResp => DeliverResp { result: 0 }.to_pdu_bytes(0, 0, seq),
        UnifiedMessage::Unbind => Unbind.to_pdu_bytes(0, 0, seq),
        UnifiedMessage::UnbindResp => UnbindResp.to_pdu_bytes(0, 0, seq),
        other => {
            return Err(RsmsError::Other(format!(
                "SGIP encode 暂不支持该消息类型（含 Ping，SGIP 无心跳）: {other:?}"
            )))
        }
    };
    Ok(bytes.to_vec())
}

impl ProtocolAdapter for SgipAdapter {
    fn protocol(&self) -> Protocol {
        Protocol::Sgip
    }
    fn decode(&self, frame: &Frame) -> Result<UnifiedMessage> {
        let msg = decode_message(frame.data_as_slice())?;
        Ok(sgip_to_unified(msg))
    }
    fn encode(&self, msg: &UnifiedMessage, sequence_id: u32) -> Result<Vec<u8>> {
        unified_to_sgip_bytes(msg, sequence_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::Encodable;
    use crate::datatypes::{Report, SgipSequence, Submit};
    use rsms_core::RawPdu;

    fn frame_of(bytes: Vec<u8>) -> Frame {
        Frame::from(RawPdu::from_vec(bytes))
    }

    #[test]
    fn decode_submit_to_unified() {
        let s = Submit::new().with_message("10655000000", "13800138000", b"Test");
        let bytes = s.to_pdu_bytes(0, 0, 10).to_vec();
        match SgipAdapter.decode(&frame_of(bytes)).unwrap() {
            UnifiedMessage::Submit(u) => {
                assert_eq!(u.src.number, "10655000000");
                assert_eq!(u.dests[0].number, "13800138000");
                assert_eq!(u.content, b"Test");
            }
            _ => panic!("expected Submit"),
        }
    }

    #[test]
    fn decode_standalone_report() {
        let report = Report {
            submit_sequence: SgipSequence::new(1, 0x04051200, 42),
            report_type: 0,
            user_number: "13800138000".to_string(),
            state: 0,
            error_code: 0,
            reserve: [0u8; 8],
        };
        let bytes = report.to_pdu_bytes(0, 0, 7).to_vec();
        match SgipAdapter.decode(&frame_of(bytes)).unwrap() {
            UnifiedMessage::Report(u) => {
                assert_eq!(u.dest.number, "13800138000");
                assert!(matches!(u.status, DeliveryStatus::Delivered));
                assert!(matches!(&u.msg_id, MessageId::Binary(b) if b.len() == 12));
            }
            _ => panic!("SGIP 独立 Report 命令应翻译为 UnifiedMessage::Report"),
        }
    }

    #[test]
    fn submit_byte_roundtrip_via_unified() {
        let s = Submit::new().with_message("10655000000", "13800138000", b"Test");
        let original = s.to_pdu_bytes(0, 0, 42).to_vec();
        let unified = SgipAdapter.decode(&frame_of(original.clone())).unwrap();
        let reencoded = SgipAdapter.encode(&unified, 42).unwrap();
        assert_eq!(reencoded, original, "SGIP Submit 经统一模型往返后字节应无损一致");
    }
}
