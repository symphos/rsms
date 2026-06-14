//! SgipAdapter：复用 decode_message + Pdu::to_pdu_bytes，做 SgipMessage ↔ UnifiedMessage 翻译。
//! 验证窄腰对「独立 Report 命令」的吸收能力。
//!
//! 约定：encode 方向 header 的 SgipSequence 固定 node_id=0, timestamp=0, number=sequence_id
//! （decode_message 丢弃 header SgipSequence，统一模型不携带它；真实连接序列生成在编排层别处）。

use crate::codec::Encodable;
use crate::datatypes::{
    Bind, BindResp, CommandId, Deliver, DeliverResp, Report, ReportResp, SgipSequence, Submit,
    SubmitResp, Unbind, UnbindResp,
};
use crate::message::{decode_message, SgipMessage};
use rsms_core::{Frame, Protocol, Result, RsmsError};
use rsms_model::{
    Address, DeliveryStatus, Encoding, MessageId, ProtocolAdapter, ProtocolExtra, Sequence,
    SgipExtra, UnifiedDeliver, UnifiedMessage, UnifiedReport, UnifiedSubmit, UnifiedSubmitResp,
};

/// SGIP 协议适配器。
pub struct SgipAdapter;

// ── 编码翻译表（SGIP msg_fmt ↔ 统一 Encoding）——取值不同于 SMGP/SMPP ──
fn encoding_from_fmt(fmt: u8) -> Encoding {
    // SGIP 1.2 MessageCoding：0=ASCII、4=二进制、8=UCS2、15=GBK（旧版把 4/8 弄反了——
    // 联调实测 cmos 发中文用 8(UCS2)，被当 Binary 解→乱码）。
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
        // SGIP Report 无源地址字段，src 置空。
        src: Address::plain(String::new()),
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
            // SGIP 的 login_type 仍走 version 字段（保持现状）。
            version: b.login_type,
            // SGIP 用不上以下三字段。
            system_type: None,
            mode: rsms_model::BindMode::default(),
            login_mode: None,
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

// ── Encode 方向 ──
// SGIP 是 `Sequence` 枚举的关键用户：复合序列须原样写入头部 SgipSequence(node_id,timestamp,number)，
// 响应帧也靠它回显请求序列。从 `Sequence` 解出三段：
//   - Sequence::Sgip{..} → 直接取三分量；
//   - Sequence::Plain(n)  → 退化为 (0, 0, n)，与原 adapter 约定一致。
fn seq_parts(seq: Sequence) -> (u32, u32, u32) {
    match seq {
        Sequence::Sgip { node_id, timestamp, number } => (node_id, timestamp, number),
        Sequence::Plain(n) => (0, 0, n),
    }
}

/// SGIP state(投递状态) 反映射：Delivered→0；其余统一归 1（投递失败/未知，取合理默认非 0）。
/// 与正向 `status_from_state`（仅区分 0/非 0）对称，有损但语义稳定。
fn state_from_status(status: &DeliveryStatus) -> u8 {
    match status {
        DeliveryStatus::Delivered => 0,
        _ => 1,
    }
}

// 注：SGIP 的 to_pdu_bytes 是 Encodable trait 方法，定义在各具体 PDU 类型上（非 Pdu 枚举），
// 故按消息类型分别在具体结构体上调用 to_pdu_bytes，不经 Pdu 中转。
fn unified_to_sgip_bytes(msg: &UnifiedMessage, seq: Sequence) -> Result<Vec<u8>> {
    let (node, ts, num) = seq_parts(seq);
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
            sub.to_pdu_bytes(node, ts, num)
        }
        UnifiedMessage::SubmitResp(r) => SubmitResp { result: r.status }.to_pdu_bytes(node, ts, num),
        UnifiedMessage::DeliverResp => DeliverResp { result: 0 }.to_pdu_bytes(node, ts, num),
        UnifiedMessage::Unbind => Unbind.to_pdu_bytes(node, ts, num),
        UnifiedMessage::UnbindResp => UnbindResp.to_pdu_bytes(node, ts, num),
        // Bind（明文认证，无 MD5）：login_type 复用统一模型 version 字段，
        // login_password 从 authenticator 字节按明文还原（与 decode 的 into_bytes 对称）。
        UnifiedMessage::Bind(b) => {
            let bind = Bind {
                login_type: b.version,
                login_name: b.client_id.clone(),
                login_password: String::from_utf8_lossy(&b.authenticator).into_owned(),
                reserve: [0u8; 8],
            };
            bind.to_pdu_bytes(node, ts, num)
        }
        UnifiedMessage::BindResp(r) => {
            BindResp { result: r.status as u8, reserve: [0u8; 8] }.to_pdu_bytes(node, ts, num)
        }
        // MO 上行 Deliver：与 decode 对称（decode 时 src=sp_number, dest=user_number）。
        UnifiedMessage::Deliver(d) => {
            // tpudhi：优先取 SgipExtra.tpudhi；无该方言时按 concat 是否存在推导（长短信置 1）。
            let tpudhi = match &d.extra {
                ProtocolExtra::Sgip(e) => e.tpudhi,
                _ => {
                    if d.concat.is_some() {
                        1
                    } else {
                        0
                    }
                }
            };
            let mut del = Deliver::new();
            del.sp_number = d.src.number.clone();
            del.user_number = d.dest.number.clone();
            del.tpudhi = tpudhi;
            del.msg_fmt = fmt_from_encoding(d.encoding);
            del.message_content = d.content.clone();
            // tppid、reserve 保持 Deliver::new() 默认
            del.to_pdu_bytes(node, ts, num)
        }
        // 投递报告（独立 Report 命令）：submit_sequence 取自 r.msg_id（12 字节大端 → 三段）。
        UnifiedMessage::Report(r) => {
            let submit_sequence = match &r.msg_id {
                MessageId::Binary(b) if b.len() == 12 => {
                    let node_id = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
                    let timestamp = u32::from_be_bytes([b[4], b[5], b[6], b[7]]);
                    let number = u32::from_be_bytes([b[8], b[9], b[10], b[11]]);
                    SgipSequence::new(node_id, timestamp, number)
                }
                // 非 12 字节 Binary 或 Text：无法还原复合序列，置零（有损）。
                _ => SgipSequence::new(0, 0, 0),
            };
            // raw 对称：decode 时 raw=[report_type, state, error_code]。
            let report_type = r.raw.first().copied().unwrap_or(0);
            let error_code = r.raw.get(2).copied().unwrap_or(0);
            let report = Report {
                submit_sequence,
                report_type,
                user_number: r.dest.number.clone(),
                state: state_from_status(&r.status),
                error_code,
                reserve: [0u8; 8],
            };
            report.to_pdu_bytes(node, ts, num)
        }
        // SGIP 收到独立 Report 必须回 ReportResp；decode 把 ReportResp 退化为 Unknown，
        // 故这里据 command_id 还原回 ReportResp（example client 回执依赖此臂）。
        UnifiedMessage::Unknown { command_id, .. }
            if *command_id == CommandId::ReportResp as u32 =>
        {
            ReportResp { result: 0 }.to_pdu_bytes(node, ts, num)
        }
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
    fn encode(&self, msg: &UnifiedMessage, seq: Sequence) -> Result<Vec<u8>> {
        unified_to_sgip_bytes(msg, seq)
    }
    /// SGIP 头序列是 12 字节复合（node_id+timestamp+number），从 data[8..20] 解出，
    /// 供业务层回复时回显请求序列，避免在 example 里手剥字节偏移。
    fn sequence_of(&self, frame: &Frame) -> Sequence {
        let d = frame.data_as_slice();
        if d.len() >= 20 {
            Sequence::Sgip {
                node_id: u32::from_be_bytes([d[8], d[9], d[10], d[11]]),
                timestamp: u32::from_be_bytes([d[12], d[13], d[14], d[15]]),
                number: u32::from_be_bytes([d[16], d[17], d[18], d[19]]),
            }
        } else {
            Sequence::Plain(frame.sequence_id)
        }
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
        let reencoded = SgipAdapter.encode(&unified, Sequence::Plain(42)).unwrap();
        assert_eq!(reencoded, original, "SGIP Submit 经统一模型往返后字节应无损一致");
    }

    #[test]
    fn bind_roundtrip_via_unified() {
        // 明文认证：login_password 直接进 authenticator 字节。
        let bind = Bind {
            login_type: 1,
            login_name: "SP001".to_string(),
            login_password: "secret123".to_string(),
            reserve: [0u8; 8],
        };
        let original = bind.to_pdu_bytes(0, 0, 11).to_vec();
        let unified = SgipAdapter.decode(&frame_of(original.clone())).unwrap();
        let reencoded = SgipAdapter.encode(&unified, Sequence::Plain(11)).unwrap();
        assert_eq!(reencoded, original, "SGIP Bind 经统一模型往返后字节应无损一致");
    }

    #[test]
    fn deliver_roundtrip_via_unified() {
        // MO 上行：decode 时 src=sp_number, dest=user_number；encode 须对称还原。
        let mut deliver = Deliver::new();
        deliver.sp_number = "10655000000".to_string();
        deliver.user_number = "13800138000".to_string();
        deliver.message_content = b"MO reply".to_vec();
        let original = deliver.to_pdu_bytes(0, 0, 5).to_vec();
        let unified = SgipAdapter.decode(&frame_of(original.clone())).unwrap();
        let reencoded = SgipAdapter.encode(&unified, Sequence::Plain(5)).unwrap();
        assert_eq!(reencoded, original, "SGIP Deliver(MO) 经统一模型往返后字节应无损一致");
    }

    #[test]
    fn report_roundtrip_via_unified_composite_seq() {
        // Report 用 Sequence::Sgip{..} 编码，验证复合序列（node/timestamp/number）写入头部正确。
        let report = Report {
            submit_sequence: SgipSequence::new(7, 0x04051200, 99),
            report_type: 0,
            user_number: "13800138000".to_string(),
            state: 0,
            error_code: 0,
            reserve: [0u8; 8],
        };
        // 头部序列与 Report body 内的 submit_sequence 是两个独立字段；
        // 这里用 Sgip{1,2,3} 作头部序列，验证它被原样写入 header。
        let original = report.to_pdu_bytes(1, 2, 3).to_vec();
        let unified = SgipAdapter.decode(&frame_of(original.clone())).unwrap();
        let reencoded = SgipAdapter
            .encode(&unified, Sequence::Sgip { node_id: 1, timestamp: 2, number: 3 })
            .unwrap();
        assert_eq!(
            reencoded, original,
            "SGIP Report 用复合 Sequence 往返后字节应无损一致（含 body submit_sequence 与 header 序列）"
        );
        // 进一步显式断言 decode 回来的 header 序列匹配复合序列。
        let mut cursor = std::io::Cursor::new(reencoded.as_slice());
        let header = crate::codec::PduHeader::decode(&mut cursor).unwrap();
        assert_eq!(header.sequence, SgipSequence::new(1, 2, 3), "header 复合序列应原样回显");
    }

    #[test]
    fn report_resp_roundtrip_via_unified() {
        // decode 把 ReportResp 退化为 Unknown{command_id=ReportResp}；
        // encode 据 command_id 还原回 ReportResp（example client 回执依赖此臂）。
        let resp = ReportResp { result: 0 };
        let original = resp.to_pdu_bytes(0, 0, 8).to_vec();
        let unified = SgipAdapter.decode(&frame_of(original.clone())).unwrap();
        match &unified {
            UnifiedMessage::Unknown { command_id, .. } => {
                assert_eq!(*command_id, CommandId::ReportResp as u32);
            }
            _ => panic!("SGIP ReportResp 应退化为 Unknown 并保留 command_id"),
        }
        let reencoded = SgipAdapter.encode(&unified, Sequence::Plain(8)).unwrap();
        assert_eq!(reencoded, original, "SGIP ReportResp 经统一模型往返后字节应无损一致");
    }
}
