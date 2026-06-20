//! CmppAdapter：复用 decode_message/encode_message，做 CmppMessage ↔ UnifiedMessage 翻译。
//! 本轮仅 V3.0（无状态 decode 默认 V3.0；V2.0 需握手版本上下文，见计划范围边界）。
//!
//! 已知限制（本轮）：Deliver 报告路径 status 暂为 Unknown（精确状态解析待后续）。
//! 本轮 encode 已补 Bind/Deliver(MO)/Report/BindResp（详见各 match 臂注释）。

use crate::datatypes::{
    CmppVersion, CommandId, Connect, ConnectResp, Deliver, DeliverResp, DeliverV20, Submit,
    SubmitResp, SubmitV20,
};
use crate::message::{decode_message, decode_message_with_version, encode_message, CmppMessage};
use rsms_core::{Frame, Protocol, Result, RsmsError};
use rsms_model::{
    Address, CmppExtra, Concat, DeliveryStatus, Encoding, MessageId, ProtocolAdapter,
    ProtocolExtra, Sequence, UnifiedBind, UnifiedDeliver, UnifiedMessage, UnifiedReport,
    UnifiedSubmit, UnifiedSubmitResp,
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

/// 解码侧：CMPP 的 UDHI 标志在独立字段 tp_udhi（u8）。仅当 tp_udhi 置位且正文以级联 UDH 开头时，
/// 剥离 UDH → (Some(concat), payload)；否则 (None, 原正文)。gate on tp_udhi 避免误判普通正文。
fn split_udh(tp_udhi: u8, content: Vec<u8>) -> (Option<Concat>, Vec<u8>) {
    if tp_udhi != 0 {
        if let Some((c, off)) = Concat::from_udh(&content) {
            return (Some(c), content[off..].to_vec());
        }
    }
    (None, content)
}

/// 编码侧：有 concat 则前置级联 UDH 到正文并把 tp_udhi 置 1；否则原样、tp_udhi 保持入参。
fn join_udh(concat: &Option<Concat>, content: &[u8], tp_udhi: u8) -> (Vec<u8>, u8) {
    match concat {
        Some(c) => {
            let mut body = c.to_udh_prefix();
            body.extend_from_slice(content);
            (body, 1)
        }
        None => (content.to_vec(), tp_udhi),
    }
}

fn submit_v30_to_unified(s: Submit) -> UnifiedSubmit {
    // 级联长短信：CMPP 用独立 tp_udhi 字段标记正文以 UDH 开头。仅当 tp_udhi 置位且为有效级联 UDH
    // 时剥离 → concat 承载分段信息、content 为纯 payload；否则 concat=None、content 原样。
    // CmppExtra.tpudhi 仍透传原值（与 concat 一致），encode 据 concat 重建并置 tpudhi=1。
    let (concat, content) = split_udh(s.tpudhi, s.msg_content);
    UnifiedSubmit {
        src: Address::plain(s.src_id),
        dests: s.dest_terminal_ids.into_iter().map(Address::plain).collect(),
        content,
        encoding: encoding_from_fmt(s.msg_fmt),
        want_report: s.registered_delivery != 0,
        concat,
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
            version: 0x30,
        }),
        tlvs: vec![],
    }
}

/// V2.0 Submit → 统一模型。V2.0 比 V3.0 少 fee_terminal_type/dest_terminal_type/link_id；
/// 这些字段在 CmppExtra 取默认。version 标记为 0x20，供 encode 回产 SubmitV20。
fn submit_v20_to_unified(s: SubmitV20) -> UnifiedSubmit {
    let (concat, content) = split_udh(s.tpudhi, s.msg_content);
    UnifiedSubmit {
        src: Address::plain(s.src_id),
        dests: s.dest_terminal_ids.into_iter().map(Address::plain).collect(),
        content,
        encoding: encoding_from_fmt(s.msg_fmt),
        want_report: s.registered_delivery != 0,
        concat,
        extra: ProtocolExtra::Cmpp(CmppExtra {
            msg_id: s.msg_id,
            pk_total: s.pk_total,
            pk_number: s.pk_number,
            msg_level: s.msg_level,
            service_id: s.service_id,
            fee_user_type: s.fee_user_type,
            fee_terminal_id: s.fee_terminal_id,
            tppid: s.tppid,
            tpudhi: s.tpudhi,
            msg_src: s.msg_src,
            fee_type: s.fee_type,
            fee_code: s.fee_code,
            valid_time: s.valid_time,
            at_time: s.at_time,
            version: 0x20,
            ..Default::default()
        }),
        tlvs: vec![],
    }
}

fn deliver_v30_to_unified(d: Deliver) -> UnifiedMessage {
    if d.registered_delivery == 1 {
        // 报告投递状态：CMPP 报告正文是定长二进制（msg_id 在 [0..8]、stat 在 [8..15]），
        // 用 CmppReport::parse 取。解析失败（正文过短/非标准）回退 Unknown。
        let parsed = crate::datatypes::CmppReport::parse(&d.msg_content);
        let status = parsed
            .as_ref()
            .map(|r| DeliveryStatus::from_stat_code(&r.stat))
            .unwrap_or(DeliveryStatus::Unknown);
        // 关联用的 Msg_Id 取**报告正文体**里的原 submit Msg_Id（CmppReport.msg_id），
        // 而非 Deliver 信封 d.msg_id（信封是这条 Deliver 自身的 id，与原 submit 无关——
        // 真机 cmos 联调实测信封 = submit_id+1，用信封会导致 Resp↔Report 永不关联）。
        // 解析失败时回退信封 id（保持短正文等退化场景的字节往返）。
        let report_msg_id = parsed
            .as_ref()
            .map(|r| r.msg_id.to_vec())
            .unwrap_or_else(|| d.msg_id.to_vec());
        UnifiedMessage::Report(UnifiedReport {
            msg_id: MessageId::Binary(report_msg_id),
            status,
            // 新增 src 字段：报告源地址取 src_terminal_id（dest 仍取 dest_id）。
            src: Address::plain(d.src_terminal_id.clone()),
            dest: Address::plain(d.dest_id),
            raw: d.msg_content,
        })
    } else {
        // MO 长短信分段：同 Submit，按 tp_udhi 标志剥离级联 UDH → concat、content 为纯 payload。
        let (concat, content) = split_udh(d.tpudhi, d.msg_content);
        UnifiedMessage::Deliver(UnifiedDeliver {
            src: Address::plain(d.src_terminal_id),
            dest: Address::plain(d.dest_id),
            content,
            encoding: encoding_from_fmt(d.msg_fmt),
            concat,
            extra: ProtocolExtra::None,
            tlvs: vec![],
        })
    }
}

/// V2.0 Deliver → 统一模型。registered_delivery=1 为状态报告(→Report，含 stat 解析；
/// 注：UnifiedReport 无 version 字段，V2.0 报告无法回 encode 为 V2.0——属模型边界，decode 仍可读)；
/// 否则为 MO 上行(→Deliver，extra.version=0x20 供 encode 回产 DeliverV20)。
fn deliver_v20_to_unified(d: DeliverV20) -> UnifiedMessage {
    if d.registered_delivery == 1 {
        let parsed = crate::datatypes::CmppReport::parse(&d.msg_content);
        let status = parsed
            .as_ref()
            .map(|r| DeliveryStatus::from_stat_code(&r.stat))
            .unwrap_or(DeliveryStatus::Unknown);
        // 同 V3.0：关联 Msg_Id 取正文体（原 submit id），非 Deliver 信封 id。
        let report_msg_id = parsed
            .as_ref()
            .map(|r| r.msg_id.to_vec())
            .unwrap_or_else(|| d.msg_id.to_vec());
        UnifiedMessage::Report(UnifiedReport {
            msg_id: MessageId::Binary(report_msg_id),
            status,
            src: Address::plain(d.src_terminal_id),
            dest: Address::plain(d.dest_id),
            raw: d.msg_content,
        })
    } else {
        let (concat, content) = split_udh(d.tpudhi, d.msg_content);
        UnifiedMessage::Deliver(UnifiedDeliver {
            src: Address::plain(d.src_terminal_id),
            dest: Address::plain(d.dest_id),
            content,
            encoding: encoding_from_fmt(d.msg_fmt),
            concat,
            extra: ProtocolExtra::Cmpp(CmppExtra { version: 0x20, ..Default::default() }),
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
        // CMPP 无 system_type/login_mode，bind 模式单一，取默认；encode 时忽略这三项。
        system_type: None,
        mode: rsms_model::BindMode::default(),
        login_mode: None,
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
        // V2.0 Submit → 统一模型（仅当经版本感知路径 decode_with_version(V20) 解出时才会到达；
        // 默认 decode 走 V3.0、不产生 SubmitV20）。
        CmppMessage::SubmitV20 { submit, .. } => UnifiedMessage::Submit(submit_v20_to_unified(submit)),
        // Deliver V2.0、Query/Cancel 等本轮退化为 Unknown（仅 shadow 日志可见），保留真实
        // command_id 便于诊断；match 改为穷尽，未来新增变体触发编译错误而非静默归零。
        // V2.0 Deliver → 统一模型（仅经 decode_with_version(V20) 解出时到达）。
        CmppMessage::DeliverV20 { deliver, .. } => deliver_v20_to_unified(deliver),
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
fn unified_to_cmpp(msg: &UnifiedMessage, seq: Sequence) -> Result<CmppMessage> {
    // CMPP 头部序列为单字段 u32；从统一 Sequence 退化取主序列号。
    let seq = seq.as_u32();
    let m = match msg {
        UnifiedMessage::Submit(s) => {
            let extra = match &s.extra {
                ProtocolExtra::Cmpp(e) => e.clone(),
                _ => CmppExtra::default(),
            };
            // 有 concat 则前置级联 UDH 到正文并置 tp_udhi=1；否则正文原样、tp_udhi 沿用 extra。
            let (udh_body, tpudhi) = join_udh(&s.concat, &s.content, extra.tpudhi);
            // version=0x20 产 SubmitV20（V2.0 比 V3.0 少 fee_terminal_type/dest_terminal_type/link_id）；
            // 否则（0/0x30）产 SubmitV30。
            if extra.version == 0x20 {
                let mut sub = SubmitV20::new();
                sub.msg_id = extra.msg_id;
                sub.pk_total = extra.pk_total;
                sub.pk_number = extra.pk_number;
                sub.registered_delivery = if s.want_report { 1 } else { 0 };
                sub.msg_level = extra.msg_level;
                sub.service_id = extra.service_id;
                sub.fee_user_type = extra.fee_user_type;
                sub.fee_terminal_id = extra.fee_terminal_id;
                sub.tppid = extra.tppid;
                sub.tpudhi = tpudhi;
                sub.msg_fmt = fmt_from_encoding(s.encoding);
                sub.msg_src = extra.msg_src;
                sub.fee_type = extra.fee_type;
                sub.fee_code = extra.fee_code;
                sub.valid_time = extra.valid_time;
                sub.at_time = extra.at_time;
                sub.src_id = s.src.number.clone();
                sub.dest_usr_tl = s.dests.len() as u8;
                sub.dest_terminal_ids = s.dests.iter().map(|a| a.number.clone()).collect();
                sub.msg_content = udh_body;
                CmppMessage::SubmitV20 { sequence_id: seq, submit: sub }
            } else {
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
                sub.tpudhi = tpudhi;
                sub.msg_content = udh_body;
                sub.link_id = extra.link_id;
                CmppMessage::SubmitV30 { sequence_id: seq, submit: sub }
            }
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
        UnifiedMessage::DeliverResp => CmppMessage::DeliverResp {
            version: CmppVersion::V30,
            sequence_id: seq,
            // 统一模型 DeliverResp 不携带回执 MsgId，置默认；result=0 表示成功。
            resp: DeliverResp { msg_id: [0u8; 8], result: 0 },
        },
        UnifiedMessage::Ping => CmppMessage::ActiveTest { version: CmppVersion::V30, sequence_id: seq },
        UnifiedMessage::PingResp => CmppMessage::ActiveTestResp { version: CmppVersion::V30, sequence_id: seq },
        UnifiedMessage::Unbind => CmppMessage::Terminate { version: CmppVersion::V30, sequence_id: seq },
        UnifiedMessage::UnbindResp => CmppMessage::TerminateResp { version: CmppVersion::V30, sequence_id: seq },
        UnifiedMessage::Bind(b) => {
            // authenticator 已是算好的 16B MD5，直接取前 16 字节填入定长数组，不再算 MD5。
            let mut auth = [0u8; 16];
            let n = b.authenticator.len().min(16);
            auth[..n].copy_from_slice(&b.authenticator[..n]);
            CmppMessage::Connect {
                version: CmppVersion::V30,
                sequence_id: seq,
                connect: Connect {
                    source_addr: b.client_id.clone(),
                    authenticator_source: auth,
                    version: b.version,
                    timestamp: b.timestamp,
                },
            }
        }
        UnifiedMessage::Deliver(d) => {
            // 有 concat 则前置级联 UDH 到正文并置 tp_udhi=1；否则正文原样、tp_udhi=0。
            let (udh_body, tpudhi) = join_udh(&d.concat, &d.content, 0);
            let version = match &d.extra {
                ProtocolExtra::Cmpp(e) => e.version,
                _ => 0,
            };
            // MO 上行：registered_delivery=0。version=0x20 产 DeliverV20，否则 DeliverV30。
            if version == 0x20 {
                let mut dl = DeliverV20::new();
                dl.registered_delivery = 0;
                dl.src_terminal_id = d.src.number.clone();
                dl.dest_id = d.dest.number.clone();
                dl.tpudhi = tpudhi;
                dl.msg_content = udh_body;
                dl.msg_fmt = fmt_from_encoding(d.encoding);
                CmppMessage::DeliverV20 { sequence_id: seq, deliver: dl }
            } else {
                let mut dl = Deliver::new();
                dl.registered_delivery = 0;
                dl.src_terminal_id = d.src.number.clone();
                dl.dest_id = d.dest.number.clone();
                dl.tpudhi = tpudhi;
                dl.msg_content = udh_body;
                dl.msg_fmt = fmt_from_encoding(d.encoding);
                CmppMessage::DeliverV30 { sequence_id: seq, deliver: dl }
            }
        }
        UnifiedMessage::Report(r) => {
            // 状态报告下行：以 Deliver(registered_delivery=1) 承载。
            let mut dl = Deliver::new();
            dl.registered_delivery = 1;
            if let MessageId::Binary(b) = &r.msg_id {
                let n = b.len().min(8);
                dl.msg_id[..n].copy_from_slice(&b[..n]);
            }
            dl.dest_id = r.dest.number.clone();
            dl.src_terminal_id = r.src.number.clone();
            dl.msg_content = r.raw.clone();
            CmppMessage::DeliverV30 { sequence_id: seq, deliver: dl }
        }
        UnifiedMessage::BindResp(r) => {
            // best-effort：统一模型不携带 ISMG 回鉴权，authenticator_ismg 用默认全 0、version 用默认 0。
            // example 不依赖此路径；框架 AuthHandler 自走 codec 生成合规 ConnectResp，故此处够用。
            CmppMessage::ConnectResp {
                version: CmppVersion::V30,
                sequence_id: seq,
                resp: ConnectResp {
                    status: r.status,
                    authenticator_ismg: [0u8; 16],
                    version: 0,
                },
            }
        }
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
    fn encode(&self, msg: &UnifiedMessage, seq: Sequence) -> Result<Vec<u8>> {
        let cmpp = unified_to_cmpp(msg, seq)?;
        encode_message(&cmpp)
    }
}

impl CmppAdapter {
    /// 版本感知解码：CMPP V2.0/V3.0 命令字相同、仅字段布局不同，单凭字节无法区分，须由握手协商的
    /// `version` 决定。trait 的 `decode` 默认按 V3.0；V2.0 连接的解码方应调用本方法传入 `CmppVersion::V20`。
    pub fn decode_with_version(
        &self,
        frame: &Frame,
        version: CmppVersion,
    ) -> Result<UnifiedMessage> {
        let msg = decode_message_with_version(frame.data_as_slice(), Some(version as u8))?;
        Ok(cmpp_to_unified(msg))
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
        let reencoded = CmppAdapter.encode(&unified, Sequence::Plain(42)).unwrap();
        assert_eq!(reencoded, original, "CMPP V3.0 Submit 经统一模型往返后字节应无损一致");
    }

    #[test]
    fn connect_byte_roundtrip_via_unified() {
        // Bind：用 codec 构造 Connect PDU → decode → Bind → encode，断言字节无损一致。
        // authenticator_source 含非零字节，验证 encode 不重算 MD5 而是原样回填。
        let connect = Connect {
            source_addr: "106900".to_string(),
            authenticator_source: [0xab; 16],
            version: 0x30,
            timestamp: 0x01020304,
        };
        let original = Pdu::from(connect).to_pdu_bytes(7).to_vec();

        let unified = CmppAdapter.decode(&frame_of(original.clone())).unwrap();
        assert!(matches!(unified, UnifiedMessage::Bind(_)));
        let reencoded = CmppAdapter.encode(&unified, Sequence::Plain(7)).unwrap();
        assert_eq!(reencoded, original, "CMPP Connect 经统一模型往返后字节应无损一致");
    }

    #[test]
    fn deliver_mo_byte_roundtrip_via_unified() {
        // Deliver(MO)：registered_delivery=0。统一模型不承载 service_id/link_id 等 Deliver 方言字段，
        // 故只让原始 PDU 的这些字段保持 Deliver::new() 默认（空），才能字节级一致。
        let mut d = Deliver::new();
        d.registered_delivery = 0;
        d.src_terminal_id = "13800138000".to_string();
        d.dest_id = "1065900000".to_string();
        d.msg_fmt = 8;
        d.msg_content = b"\x4e\x2d\x65\x87".to_vec();
        let original = Pdu::from(d).to_pdu_bytes(5).to_vec();

        let unified = CmppAdapter.decode(&frame_of(original.clone())).unwrap();
        assert!(matches!(unified, UnifiedMessage::Deliver(_)));
        let reencoded = CmppAdapter.encode(&unified, Sequence::Plain(5)).unwrap();
        assert_eq!(reencoded, original, "CMPP Deliver(MO) 经统一模型往返后字节应无损一致");
    }

    #[test]
    fn report_msg_id_taken_from_content_not_envelope() {
        // 真机(cmos)联调暴露：CMPP 状态报告的**关联** Msg_Id 在正文体 [0..8]（= 原 submit 的
        // Msg_Id，SubmitResp 回的那个）；Deliver 信封 Msg_Id 是这条 Deliver 自身的 id（≠ submit，
        // cmos 实测为 submit_id+1）。UnifiedReport.msg_id 必须取正文体的，否则 Resp↔Report 无法关联。
        let content_msg_id = [0x67u8, 0x05, 0xf1, 0x03, 0x24, 0x66, 0x2f, 0x03];
        let mut body = Vec::new();
        body.extend_from_slice(&content_msg_id); // [0..8] 原 submit Msg_Id
        body.extend_from_slice(b"DELIVRD"); // [8..15] stat
        body.extend_from_slice(b"2606200730"); // [15..25] submit_time
        body.extend_from_slice(b"2606200731"); // [25..35] done_time

        let mut d = Deliver::new();
        d.registered_delivery = 1;
        d.msg_id = [0x11; 8]; // 信封 Msg_Id（与正文体不同）
        d.src_terminal_id = "10086".to_string();
        d.dest_id = "1065900000".to_string();
        d.msg_content = body;
        let bytes = Pdu::from(d).to_pdu_bytes(9).to_vec();

        match CmppAdapter.decode(&frame_of(bytes)).unwrap() {
            UnifiedMessage::Report(r) => assert_eq!(
                r.msg_id,
                MessageId::Binary(content_msg_id.to_vec()),
                "报告关联 Msg_Id 应取正文体[0..8]（原 submit id），而非 Deliver 信封 id"
            ),
            other => panic!("expected Report, got {other:?}"),
        }
    }

    #[test]
    fn report_byte_roundtrip_via_unified() {
        // Report：registered_delivery=1。统一 UnifiedReport 承载 msg_id/src/dest/raw，
        // 不承载 service_id/link_id 等方言字段，故原始 PDU 这些字段须保持默认（空）方可字节一致。
        // 注意 status 解码恒为 Unknown（有损点），但 encode 不读 status（仅 registered_delivery 标志），
        // 故字节往返仍可一致。
        let mut d = Deliver::new();
        d.registered_delivery = 1;
        d.msg_id = [0x67, 0x05, 0xf1, 0x03, 0x24, 0x66, 0x2f, 0x02];
        d.src_terminal_id = "10086".to_string();
        d.dest_id = "1065900000".to_string();
        d.msg_content = b"DELIVRD report body".to_vec();
        let original = Pdu::from(d).to_pdu_bytes(9).to_vec();

        let unified = CmppAdapter.decode(&frame_of(original.clone())).unwrap();
        assert!(matches!(unified, UnifiedMessage::Report(_)));
        let reencoded = CmppAdapter.encode(&unified, Sequence::Plain(9)).unwrap();
        assert_eq!(reencoded, original, "CMPP Report 经统一模型往返后字节应无损一致");
    }

    #[test]
    fn submit_concat_udh_roundtrip_via_unified() {
        // tp_udhi=1 + 8-bit 级联 UDH(05 00 03 ref total seq) + payload。
        // decode 应剥 UDH 填 concat、content 为纯 payload；encode 据 concat 重建 UDH 并置 tp_udhi=1，
        // 字节与原始无损一致。
        let mut s = Submit::new();
        s.src_id = "10086".to_string();
        s.dest_terminal_ids = vec!["13800138000".to_string()];
        s.dest_usr_tl = 1;
        s.msg_fmt = 8;
        s.tpudhi = 1;
        s.msg_content = vec![0x05, 0x00, 0x03, 7, 2, 1, 0x4e, 0x2d];
        let original = Pdu::from(s).to_pdu_bytes(30).to_vec();

        let unified = CmppAdapter.decode(&frame_of(original.clone())).unwrap();
        match &unified {
            UnifiedMessage::Submit(u) => {
                assert_eq!(u.concat, Some(Concat { reference: 7, total: 2, sequence: 1 }));
                assert_eq!(u.content, vec![0x4e, 0x2d], "content 应为剥离 UDH 后的纯 payload");
            }
            other => panic!("expected Submit, got {other:?}"),
        }
        let reencoded = CmppAdapter.encode(&unified, Sequence::Plain(30)).unwrap();
        assert_eq!(reencoded, original, "级联 Submit 经统一模型往返字节应无损一致");
    }

    #[test]
    fn deliver_mo_concat_udh_roundtrip_via_unified() {
        // MO（registered_delivery=0）级联：tp_udhi=1 + 8-bit 级联 UDH + payload，字节往返无损一致。
        let mut d = Deliver::new();
        d.registered_delivery = 0;
        d.src_terminal_id = "13800138000".to_string();
        d.dest_id = "10086".to_string();
        d.msg_fmt = 8;
        d.tpudhi = 1;
        d.msg_content = vec![0x05, 0x00, 0x03, 9, 3, 2, 0x65, 0x87];
        let original = Pdu::from(d).to_pdu_bytes(31).to_vec();

        let unified = CmppAdapter.decode(&frame_of(original.clone())).unwrap();
        match &unified {
            UnifiedMessage::Deliver(u) => {
                assert_eq!(u.concat, Some(Concat { reference: 9, total: 3, sequence: 2 }));
                assert_eq!(u.content, vec![0x65, 0x87], "content 应为剥离 UDH 后的纯 payload");
            }
            other => panic!("expected Deliver, got {other:?}"),
        }
        let reencoded = CmppAdapter.encode(&unified, Sequence::Plain(31)).unwrap();
        assert_eq!(reencoded, original, "级联 Deliver(MO) 经统一模型往返字节应无损一致");
    }

    #[test]
    fn decode_report_parses_delivery_status() {
        // registered_delivery=1，msg_content 为定长二进制报告（stat 在 [8..15]）→ status 应解析为 Delivered。
        let mut content = Vec::new();
        content.extend_from_slice(&[0u8; 8]); // msg_id(8)
        content.extend_from_slice(b"DELIVRD"); // stat(7)
        content.extend_from_slice(b"2601011200"); // submit_time(10)
        content.extend_from_slice(b"2601011201"); // done_time(10)
        let mut d = Deliver::new();
        d.registered_delivery = 1;
        d.dest_id = "1065900000".to_string();
        d.msg_content = content;
        let bytes = Pdu::from(d).to_pdu_bytes(10).to_vec();
        match CmppAdapter.decode(&frame_of(bytes)).unwrap() {
            UnifiedMessage::Report(r) => {
                assert_eq!(r.status, DeliveryStatus::Delivered, "stat DELIVRD 应解析为 Delivered");
            }
            other => panic!("expected Report, got {other:?}"),
        }
    }

    #[test]
    fn submit_v20_roundtrip_via_unified() {
        // V2.0：CmppExtra.version=0x20 时 encode 产 SubmitV20；decode_with_version(V20) 还原为
        // UnifiedMessage::Submit（而非 Unknown），且字节往返无损。CMPP V2.0/V3.0 命令字相同、
        // 仅靠握手 version 区分，故解码用版本感知的固有方法 decode_with_version。
        let submit = UnifiedMessage::Submit(UnifiedSubmit {
            src: Address::plain("10086"),
            dests: vec![Address::plain("13800138000")],
            content: b"hi".to_vec(),
            encoding: Encoding::Gbk,
            want_report: true,
            concat: None,
            extra: ProtocolExtra::Cmpp(CmppExtra { version: 0x20, ..Default::default() }),
            tlvs: vec![],
        });
        let bytes = CmppAdapter.encode(&submit, Sequence::Plain(7)).unwrap();
        let unified = CmppAdapter
            .decode_with_version(&frame_of(bytes.clone()), CmppVersion::V20)
            .unwrap();
        match &unified {
            UnifiedMessage::Submit(u) => {
                assert_eq!(u.src.number, "10086");
                assert_eq!(u.dests[0].number, "13800138000");
                assert_eq!(u.content, b"hi");
                match &u.extra {
                    ProtocolExtra::Cmpp(e) => assert_eq!(e.version, 0x20, "version 应透出为 V2.0"),
                    _ => panic!("extra 应为 Cmpp"),
                }
            }
            other => panic!("expected Submit, got {other:?}"),
        }
        let reencoded = CmppAdapter.encode(&unified, Sequence::Plain(7)).unwrap();
        assert_eq!(reencoded, bytes, "V2.0 Submit 经统一模型往返字节应无损一致");
    }

    #[test]
    fn deliver_v20_mo_roundtrip_via_unified() {
        // V2.0 MO：extra.version=0x20 → encode 产 DeliverV20；decode_with_version(V20) 还原为 Deliver，
        // 字节往返无损。
        let deliver = UnifiedMessage::Deliver(UnifiedDeliver {
            src: Address::plain("13800138000"),
            dest: Address::plain("10086"),
            content: b"mo".to_vec(),
            encoding: Encoding::Gbk,
            concat: None,
            extra: ProtocolExtra::Cmpp(CmppExtra { version: 0x20, ..Default::default() }),
            tlvs: vec![],
        });
        let bytes = CmppAdapter.encode(&deliver, Sequence::Plain(8)).unwrap();
        let unified = CmppAdapter
            .decode_with_version(&frame_of(bytes.clone()), CmppVersion::V20)
            .unwrap();
        match &unified {
            UnifiedMessage::Deliver(u) => {
                assert_eq!(u.src.number, "13800138000");
                assert_eq!(u.dest.number, "10086");
                assert_eq!(u.content, b"mo");
            }
            other => panic!("expected Deliver, got {other:?}"),
        }
        let reencoded = CmppAdapter.encode(&unified, Sequence::Plain(8)).unwrap();
        assert_eq!(reencoded, bytes, "V2.0 Deliver(MO) 经统一模型往返字节应无损一致");
    }
}
