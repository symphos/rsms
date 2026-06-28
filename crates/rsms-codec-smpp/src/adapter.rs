//! SmppAdapter：复用 decode_message + Pdu::to_pdu_bytes，做 SmppMessage ↔ UnifiedMessage 翻译。
//! 验证窄腰对 TON/NPI（落 Address）与 TLV（落 tlvs）的吸收能力。
//!
//! 本轮 encode 覆盖：Submit/SubmitResp/Bind/BindResp/Deliver(MO)/Report/DeliverResp/心跳/解绑。
//! 已知限制：BindResp.status 暂填 0（SMPP 结果码在 command_status，decode 未透出，且无法区分三种
//! bind resp，统一默认 BindTransceiverResp）；Report 路径未解析 MESSAGE_STATE 等 TLV，status 暂为 Unknown。

use crate::codec::Pdu;
use crate::datatypes::{
    BindReceiver, BindTransceiver, BindTransceiverResp, BindTransmitter, CommandId, DeliverSm,
    DeliverSmResp, EnquireLink, EnquireLinkResp, SubmitSm, SubmitSmResp, Tlv, Unbind, UnbindResp,
};
use crate::message::{decode_message, SmppMessage};
use rsms_core::{Frame, Protocol, Result, RsmsError};
use rsms_model::{
    Address, BindMode, Concat, DeliveryStatus, Encoding, MessageId, ProtocolAdapter, ProtocolExtra,
    Sequence, SmppExtra, Tlv as UTlv, UnifiedBind, UnifiedDeliver, UnifiedMessage, UnifiedReport,
    UnifiedSubmit, UnifiedSubmitResp,
};

/// SMPP 协议适配器。
pub struct SmppAdapter;

// ── 编码翻译表（SMPP data_coding ↔ 统一 Encoding）──
fn encoding_from_dcs(dcs: u8) -> Encoding {
    match dcs {
        0x00 => Encoding::Gsm7,
        0x01 => Encoding::Ascii,
        0x02 => Encoding::Binary,
        // SMPP 3.4 标准 UCS2 = data_coding 0x08；0x10 是部分网关的非标用法，一并容错收下。
        0x08 | 0x10 => Encoding::Ucs2,
        other => Encoding::Other(other),
    }
}
fn dcs_from_encoding(enc: Encoding) -> u8 {
    match enc {
        Encoding::Gsm7 => 0x00,
        Encoding::Ascii => 0x01,
        Encoding::Binary => 0x02,
        // 发标准 0x08（cmos 等第三方按 0x08 解 UCS2；旧版发 0x10 仅靠对端宽松才侥幸）。
        Encoding::Ucs2 => 0x08,
        // SMPP 无 GBK 专属 dcs，落 0x00（有损：回译为 Gsm7）
        Encoding::Gbk => 0x00,
        Encoding::Other(v) => v,
    }
}

fn tlvs_to_unified(tlvs: &[Tlv]) -> Vec<UTlv> {
    tlvs.iter()
        .map(|t| UTlv { tag: t.tag, value: t.value.to_vec() })
        .collect()
}
fn tlvs_from_unified(tlvs: &[UTlv]) -> Vec<Tlv> {
    tlvs.iter().map(|t| Tlv::new(t.tag, t.value.clone())).collect()
}

/// 解码侧：esm_class 置 UDHI(0x40) 且正文以级联 UDH 开头时，剥离 UDH → (Some(concat), payload)；
/// 否则 (None, 原正文)。
fn split_udh(esm_class: u8, content: Vec<u8>) -> (Option<Concat>, Vec<u8>) {
    if esm_class & 0x40 != 0 {
        if let Some((c, off)) = Concat::from_udh(&content) {
            return (Some(c), content[off..].to_vec());
        }
    }
    (None, content)
}

/// 编码侧：有 concat 则前置级联 UDH 到正文并在 esm_class 置 UDHI(0x40)；否则原样。
fn join_udh(concat: &Option<Concat>, content: &[u8], esm_class: u8) -> (Vec<u8>, u8) {
    match concat {
        Some(c) => {
            let mut body = c.to_udh_prefix();
            body.extend_from_slice(content);
            (body, esm_class | 0x40)
        }
        None => (content.to_vec(), esm_class),
    }
}

/// 从 SMPP 投递回执正文（形如 `id:XXXX sub:.. stat:DELIVRD ..`）解析 `id:` 段。
/// 用于 receipted_message_id TLV 缺失时回退取关联用 msg_id。找不到/空返回 None。
fn receipt_id_from_text(text: &[u8]) -> Option<String> {
    let s = String::from_utf8_lossy(text);
    let pos = s.find("id:")?;
    let v: String = s[pos + 3..].chars().take_while(|c| !c.is_whitespace()).collect();
    if v.is_empty() { None } else { Some(v) }
}

fn submit_to_unified(s: SubmitSm) -> UnifiedSubmit {
    // 级联长短信：esm_class bit6(0x40)=UDHI 表示正文以 UDH 开头。剥离 UDH → concat 承载分段信息、
    // content 为纯 payload；非 UDHI 或无有效级联 UDH 时 concat=None、content 原样。
    let (concat, content) = split_udh(s.esm_class, s.short_message);
    UnifiedSubmit {
        src: Address {
            number: s.source_addr,
            ton: Some(s.source_addr_ton),
            npi: Some(s.source_addr_npi),
        },
        dests: vec![Address {
            number: s.destination_addr,
            ton: Some(s.dest_addr_ton),
            npi: Some(s.dest_addr_npi),
        }],
        content,
        encoding: encoding_from_dcs(s.data_coding),
        want_report: s.registered_delivery & 0x01 != 0,
        concat,
        extra: ProtocolExtra::Smpp(SmppExtra {
            service_type: s.service_type,
            esm_class: s.esm_class,
            protocol_id: s.protocol_id,
            priority_flag: s.priority_flag,
            schedule_delivery_time: s.schedule_delivery_time,
            validity_period: s.validity_period,
            registered_delivery: s.registered_delivery,
            replace_if_present_flag: s.replace_if_present_flag,
            sm_default_msg_id: s.sm_default_msg_id,
        }),
        tlvs: tlvs_to_unified(&s.tlvs),
    }
}

fn deliver_to_unified(d: DeliverSm) -> UnifiedMessage {
    let dest = Address {
        number: d.destination_addr,
        ton: Some(d.dest_addr_ton),
        npi: Some(d.dest_addr_npi),
    };
    if d.esm_class & 0x04 != 0 {
        // 投递回执 msg_id：receipted_message_id TLV(0x001E) 优先；缺失/空时回退解析 short_message
        // 回执正文的 "id:" 段——真机(cmos)联调实测其 deliver_sm 回执不带该 TLV，msg_id 只在正文
        // id: 字段，不回退则 UnifiedReport.msg_id 为空、Resp↔Report 永不关联（on_report 全丢）。
        let msg_id = d
            .tlvs
            .iter()
            .find(|t| t.tag == 0x001E)
            .map(|t| String::from_utf8_lossy(&t.value).trim_end_matches('\0').to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| receipt_id_from_text(&d.short_message))
            .map(MessageId::Text)
            .unwrap_or_else(|| MessageId::Text(String::new()));
        UnifiedMessage::Report(UnifiedReport {
            msg_id,
            // 回执正文（short_message）形如 id:.. stat:DELIVRD ..，解析 stat 段 → DeliveryStatus。
            // 注：MESSAGE_STATE(0x0427) TLV 的数字状态本轮未用，以文本 stat 为准（cmos 等网关均带 stat 文本）。
            status: DeliveryStatus::from_receipt_text(&d.short_message),
            // 报告源地址：从 DeliverSm 的 source_addr/ton/npi 落入统一模型，
            // encode 回报告时据此还原 DeliverSm.source_addr。
            src: Address {
                number: d.source_addr.clone(),
                ton: Some(d.source_addr_ton),
                npi: Some(d.source_addr_npi),
            },
            dest,
            raw: d.short_message,
        })
    } else {
        // MO 长短信分段：同 Submit，按 UDHI 剥离级联 UDH → concat。
        let (concat, content) = split_udh(d.esm_class, d.short_message);
        UnifiedMessage::Deliver(UnifiedDeliver {
            src: Address {
                number: d.source_addr,
                ton: Some(d.source_addr_ton),
                npi: Some(d.source_addr_npi),
            },
            dest,
            content,
            encoding: encoding_from_dcs(d.data_coding),
            concat,
            extra: ProtocolExtra::None,
            tlvs: tlvs_to_unified(&d.tlvs),
        })
    }
}

fn bind_to_unified(
    system_id: String,
    password: String,
    system_type: String,
    interface_version: u8,
    mode: BindMode,
) -> UnifiedBind {
    UnifiedBind {
        client_id: system_id,
        // SMPP 口令为明文，直接落字节（与 CMPP/SMGP 的 16B MD5 不同）。
        authenticator: password.into_bytes(),
        timestamp: 0,
        version: interface_version,
        // system_type（CMT 等）透出；mode 按 bind 变体区分收发/只发/只收；SMPP 无 login_mode。
        system_type: Some(system_type),
        mode,
        login_mode: None,
    }
}

fn smpp_to_unified(msg: SmppMessage) -> UnifiedMessage {
    match msg {
        SmppMessage::SubmitSm(s) => UnifiedMessage::Submit(submit_to_unified(s)),
        SmppMessage::SubmitSmResp(r) => UnifiedMessage::SubmitResp(UnifiedSubmitResp {
            msg_id: MessageId::Text(r.message_id),
            // SMPP 结果码在头部 command_status，经 decode 透传到统一模型 status。
            status: r.command_status,
        }),
        SmppMessage::DeliverSm(d) => deliver_to_unified(d),
        SmppMessage::DeliverSmResp(_) => UnifiedMessage::DeliverResp,
        SmppMessage::BindTransmitter(b) => UnifiedMessage::Bind(bind_to_unified(
            b.system_id,
            b.password,
            b.system_type,
            b.interface_version,
            BindMode::Transmitter,
        )),
        SmppMessage::BindReceiver(b) => UnifiedMessage::Bind(bind_to_unified(
            b.system_id,
            b.password,
            b.system_type,
            b.interface_version,
            BindMode::Receiver,
        )),
        SmppMessage::BindTransceiver(b) => UnifiedMessage::Bind(bind_to_unified(
            b.system_id,
            b.password,
            b.system_type,
            b.interface_version,
            BindMode::Transceiver,
        )),
        // SMPP BindResp 的结果码在 PDU command_status，经 decode 透传到统一模型 status
        // （sc_interface_version 是服务端协议版本、非结果码，不可充当 status）。
        SmppMessage::BindTransmitterResp(r) => {
            UnifiedMessage::BindResp(rsms_model::UnifiedBindResp { status: r.command_status })
        }
        SmppMessage::BindReceiverResp(r) => {
            UnifiedMessage::BindResp(rsms_model::UnifiedBindResp { status: r.command_status })
        }
        SmppMessage::BindTransceiverResp(r) => {
            UnifiedMessage::BindResp(rsms_model::UnifiedBindResp { status: r.command_status })
        }
        SmppMessage::EnquireLink(_) => UnifiedMessage::Ping,
        SmppMessage::EnquireLinkResp(_) => UnifiedMessage::PingResp,
        SmppMessage::Unbind(_) => UnifiedMessage::Unbind,
        SmppMessage::UnbindResp(_) => UnifiedMessage::UnbindResp,
        SmppMessage::Unknown { command_id, body } => UnifiedMessage::Unknown { command_id, raw: body },
        // Query/Cancel/GenericNack 等次要消息本轮退化为 Unknown（仅 shadow 日志可见），
        // 保留真实 command_id 便于诊断；match 改为穷尽，未来新增变体会触发编译错误而非静默归零。
        SmppMessage::QuerySm(_) => UnifiedMessage::Unknown { command_id: CommandId::QUERY_SM as u32, raw: vec![] },
        SmppMessage::QuerySmResp(_) => UnifiedMessage::Unknown { command_id: CommandId::QUERY_SM_RESP as u32, raw: vec![] },
        SmppMessage::CancelSm(_) => UnifiedMessage::Unknown { command_id: CommandId::CANCEL_SM as u32, raw: vec![] },
        SmppMessage::CancelSmResp(_) => UnifiedMessage::Unknown { command_id: CommandId::CANCEL_SM_RESP as u32, raw: vec![] },
        SmppMessage::GenericNack(_) => UnifiedMessage::Unknown { command_id: CommandId::GENERIC_NACK as u32, raw: vec![] },
    }
}

// ── Encode 方向：UnifiedMessage → SMPP struct → Pdu → bytes ──
fn unified_to_smpp_bytes(msg: &UnifiedMessage, seq: Sequence) -> Result<Vec<u8>> {
    // SMPP 头序列为单字段 u32：从任意 Sequence 退化取主序列号。
    let seq = seq.as_u32();
    let pdu: Pdu = match msg {
        UnifiedMessage::Submit(s) => {
            let extra = match &s.extra {
                ProtocolExtra::Smpp(e) => e.clone(),
                _ => SmppExtra::default(),
            };
            let dest = s.dests.first().cloned().unwrap_or(Address::plain(""));
            let mut sm = SubmitSm::new();
            sm.service_type = extra.service_type;
            sm.source_addr_ton = s.src.ton.unwrap_or(0);
            sm.source_addr_npi = s.src.npi.unwrap_or(0);
            sm.source_addr = s.src.number.clone();
            sm.dest_addr_ton = dest.ton.unwrap_or(0);
            sm.dest_addr_npi = dest.npi.unwrap_or(0);
            sm.destination_addr = dest.number;
            // 有 concat 则前置级联 UDH 并置 esm_class UDHI(0x40)。
            let (udh_body, esm_class) = join_udh(&s.concat, &s.content, extra.esm_class);
            sm.esm_class = esm_class;
            sm.protocol_id = extra.protocol_id;
            sm.priority_flag = extra.priority_flag;
            sm.schedule_delivery_time = extra.schedule_delivery_time;
            sm.validity_period = extra.validity_period;
            sm.registered_delivery = extra.registered_delivery;
            sm.replace_if_present_flag = extra.replace_if_present_flag;
            sm.data_coding = dcs_from_encoding(s.encoding);
            sm.sm_default_msg_id = extra.sm_default_msg_id;
            sm.short_message = udh_body;
            sm.tlvs = tlvs_from_unified(&s.tlvs);
            Pdu::from(sm)
        }
        UnifiedMessage::SubmitResp(r) => {
            let message_id = match &r.msg_id {
                MessageId::Text(t) => t.clone(),
                MessageId::Binary(b) => String::from_utf8_lossy(b).into_owned(),
            };
            // 统一模型 status 写回 SubmitSmResp.command_status → to_pdu_bytes 写进头部。
            Pdu::from(SubmitSmResp { message_id, command_status: r.status })
        }
        // SMPP 报告经 DeliverSm(esm_class=0x04) 承载，对报告的响应即 DeliverSmResp。
        UnifiedMessage::DeliverResp | UnifiedMessage::ReportResp => Pdu::from(DeliverSmResp { message_id: String::new() }),
        UnifiedMessage::Bind(b) => {
            // 按 mode 选 bind 变体。system_id=client_id，口令明文（authenticator 为明文字节），
            // system_type 缺省为空串，interface_version 取 version。addr_ton/npi/address_range 默认。
            let system_id = b.client_id.clone();
            let password = String::from_utf8_lossy(&b.authenticator).into_owned();
            let system_type = b.system_type.clone().unwrap_or_default();
            let version = b.version;
            match b.mode {
                BindMode::Transceiver => {
                    Pdu::from(BindTransceiver::new(&system_id, &password, &system_type, version))
                }
                BindMode::Transmitter => {
                    Pdu::from(BindTransmitter::new(&system_id, &password, &system_type, version))
                }
                BindMode::Receiver => {
                    Pdu::from(BindReceiver::new(&system_id, &password, &system_type, version))
                }
            }
        }
        UnifiedMessage::BindResp(r) => {
            // 统一模型 BindResp 仅带 status，无法区分 transceiver/transmitter/receiver resp，默认产出
            // BindTransceiverResp；status 写回 command_status → to_pdu_bytes 写进头部（rsms 作 server 回失败）。
            Pdu::from(BindTransceiverResp {
                system_id: String::new(),
                sc_interface_version: 0,
                command_status: r.status,
            })
        }
        UnifiedMessage::Deliver(d) => {
            // MO 上行 → DeliverSm（esm_class 非回执位）。
            let extra = match &d.extra {
                ProtocolExtra::Smpp(e) => e.clone(),
                _ => SmppExtra::default(),
            };
            // 有 concat 则前置级联 UDH 并置 esm_class UDHI(0x40)。
            let (udh_body, esm_class) = join_udh(&d.concat, &d.content, extra.esm_class);
            let mut sm = DeliverSm {
                service_type: extra.service_type,
                source_addr_ton: d.src.ton.unwrap_or(0),
                source_addr_npi: d.src.npi.unwrap_or(0),
                source_addr: d.src.number.clone(),
                dest_addr_ton: d.dest.ton.unwrap_or(0),
                dest_addr_npi: d.dest.npi.unwrap_or(0),
                destination_addr: d.dest.number.clone(),
                esm_class,
                protocol_id: extra.protocol_id,
                priority_flag: extra.priority_flag,
                schedule_delivery_time: extra.schedule_delivery_time,
                validity_period: extra.validity_period,
                registered_delivery: extra.registered_delivery,
                replace_if_present_flag: extra.replace_if_present_flag,
                data_coding: dcs_from_encoding(d.encoding),
                sm_default_msg_id: extra.sm_default_msg_id,
                short_message: udh_body,
                tlvs: tlvs_from_unified(&d.tlvs),
            };
            // MO 不应置回执位（保留 UDHI 等其它位）。
            sm.esm_class &= !0x04;
            Pdu::from(sm)
        }
        UnifiedMessage::Report(r) => {
            // 投递回执 → DeliverSm（esm_class=0x04）。receipted_message_id TLV(0x001E) 与 decode 侧对称：
            // value 为 msg_id 文本字节（Binary 时按 lossy 文本化，best-effort）。
            let msg_id_text = match &r.msg_id {
                MessageId::Text(t) => t.clone(),
                MessageId::Binary(b) => String::from_utf8_lossy(b).into_owned(),
            };
            let tlvs = vec![Tlv::new(0x001E, msg_id_text.into_bytes())];
            // 保留原 raw 报告正文作为 short_message。
            let sm = DeliverSm {
                service_type: String::new(),
                source_addr_ton: r.src.ton.unwrap_or(0),
                source_addr_npi: r.src.npi.unwrap_or(0),
                source_addr: r.src.number.clone(),
                dest_addr_ton: r.dest.ton.unwrap_or(0),
                dest_addr_npi: r.dest.npi.unwrap_or(0),
                destination_addr: r.dest.number.clone(),
                esm_class: 0x04,
                protocol_id: 0,
                priority_flag: 0,
                schedule_delivery_time: String::new(),
                validity_period: String::new(),
                registered_delivery: 0,
                replace_if_present_flag: 0,
                data_coding: 0,
                sm_default_msg_id: 0,
                short_message: r.raw.clone(),
                tlvs,
            };
            Pdu::from(sm)
        }
        UnifiedMessage::Ping => Pdu::from(EnquireLink),
        UnifiedMessage::PingResp => Pdu::from(EnquireLinkResp),
        UnifiedMessage::Unbind => Pdu::from(Unbind),
        UnifiedMessage::UnbindResp => Pdu::from(UnbindResp),
        other => {
            return Err(RsmsError::Other(format!(
                "SMPP encode 暂不支持该消息类型: {other:?}"
            )))
        }
    };
    Ok(pdu.to_pdu_bytes(seq).to_vec())
}

impl ProtocolAdapter for SmppAdapter {
    fn protocol(&self) -> Protocol {
        Protocol::Smpp
    }
    fn decode(&self, frame: &Frame) -> Result<UnifiedMessage> {
        let msg = decode_message(frame.data_as_slice())?;
        Ok(smpp_to_unified(msg))
    }
    fn encode(&self, msg: &UnifiedMessage, seq: Sequence) -> Result<Vec<u8>> {
        unified_to_smpp_bytes(msg, seq)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::Pdu;
    use crate::datatypes::{DeliverSm, SubmitSm};
    use rsms_core::RawPdu;

    fn frame_of(bytes: Vec<u8>) -> Frame {
        Frame::from(RawPdu::from_vec(bytes))
    }

    #[test]
    fn decode_submit_carries_ton_npi_and_tlv() {
        let mut s = SubmitSm::new();
        s.source_addr = "1065900000".to_string();
        s.source_addr_ton = 5;
        s.source_addr_npi = 0;
        s.destination_addr = "13800138000".to_string();
        s.dest_addr_ton = 1;
        s.dest_addr_npi = 1;
        s.data_coding = 0x08; // Ucs2（SMPP 标准）
        s.short_message = b"Hi".to_vec();
        s.tlvs.push(crate::datatypes::Tlv::new(0x0204, vec![0x00, 0x2A]));
        let bytes = Pdu::from(s).to_pdu_bytes(7).to_vec();

        match SmppAdapter.decode(&frame_of(bytes)).unwrap() {
            UnifiedMessage::Submit(u) => {
                assert_eq!(u.src.number, "1065900000");
                assert_eq!(u.src.ton, Some(5));
                assert_eq!(u.dests[0].ton, Some(1));
                assert_eq!(u.dests[0].npi, Some(1));
                assert!(matches!(u.encoding, Encoding::Ucs2));
                assert_eq!(u.tlvs.len(), 1);
                assert_eq!(u.tlvs[0].tag, 0x0204);
                assert_eq!(u.tlvs[0].value, vec![0x00, 0x2A]);
            }
            _ => panic!("expected Submit"),
        }
    }

    #[test]
    fn decode_deliver_receipt_is_report() {
        let mut d = DeliverSm {
            service_type: String::new(),
            source_addr_ton: 0,
            source_addr_npi: 0,
            source_addr: String::new(),
            dest_addr_ton: 0,
            dest_addr_npi: 0,
            destination_addr: String::new(),
            esm_class: 0,
            protocol_id: 0,
            priority_flag: 0,
            schedule_delivery_time: String::new(),
            validity_period: String::new(),
            registered_delivery: 0,
            replace_if_present_flag: 0,
            data_coding: 0,
            sm_default_msg_id: 0,
            short_message: vec![],
            tlvs: vec![],
        };
        d.esm_class = 0x04; // delivery receipt
        d.destination_addr = "1065900000".to_string();
        let bytes = Pdu::from(d).to_pdu_bytes(8).to_vec();
        assert!(matches!(
            SmppAdapter.decode(&frame_of(bytes)).unwrap(),
            UnifiedMessage::Report(_)
        ));
    }

    #[test]
    fn report_msg_id_falls_back_to_receipt_text_when_no_tlv() {
        // 真机(cmos)联调暴露：deliver_sm 投递回执常**不带** receipted_message_id TLV(0x001E)，
        // msg_id 只在回执正文 short_message 的 "id:" 段。须回退解析，否则 UnifiedReport.msg_id 为空、
        // 无法与 SubmitSmResp(message_id) 关联。
        let mut d = DeliverSm {
            service_type: String::new(),
            source_addr_ton: 0,
            source_addr_npi: 0,
            source_addr: "1065900000".to_string(),
            dest_addr_ton: 0,
            dest_addr_npi: 0,
            destination_addr: "13800138000".to_string(),
            esm_class: 0x04,
            protocol_id: 0,
            priority_flag: 0,
            schedule_delivery_time: String::new(),
            validity_period: String::new(),
            registered_delivery: 0,
            replace_if_present_flag: 0,
            data_coding: 0,
            sm_default_msg_id: 0,
            short_message: b"id:0620155538080677202838 stat:DELIVRD".to_vec(),
            tlvs: vec![], // 关键：无 receipted_message_id TLV
        };
        d.esm_class = 0x04;
        let bytes = Pdu::from(d).to_pdu_bytes(9).to_vec();
        match SmppAdapter.decode(&frame_of(bytes)).unwrap() {
            UnifiedMessage::Report(r) => assert_eq!(
                r.msg_id,
                MessageId::Text("0620155538080677202838".to_string()),
                "无 TLV 时应从回执正文 id: 段取 msg_id"
            ),
            other => panic!("expected Report, got {other:?}"),
        }
    }

    #[test]
    fn submit_byte_roundtrip_via_unified() {
        let mut s = SubmitSm::new();
        s.service_type = "CMT".to_string();
        s.source_addr_ton = 5;
        s.source_addr_npi = 0;
        s.source_addr = "1065900000".to_string();
        s.dest_addr_ton = 1;
        s.dest_addr_npi = 1;
        s.destination_addr = "13800138000".to_string();
        s.esm_class = 0;
        s.registered_delivery = 1;
        s.data_coding = 0x08;
        s.short_message = b"\x4e\x2d".to_vec();
        s.tlvs.push(crate::datatypes::Tlv::new(0x0204, vec![0x00, 0x2A]));
        let original = Pdu::from(s).to_pdu_bytes(42).to_vec();

        let unified = SmppAdapter.decode(&frame_of(original.clone())).unwrap();
        let reencoded = SmppAdapter.encode(&unified, Sequence::Plain(42)).unwrap();
        assert_eq!(reencoded, original, "SMPP 经统一模型往返后字节应无损一致");
    }

    #[test]
    fn bind_transceiver_byte_roundtrip_via_unified() {
        // Bind（用 BindTransceiver）字节往返：system_id/password/system_type/interface_version
        // 经统一模型解码再编码应无损一致（addr_ton/npi/address_range 默认值对称）。
        let bt = BindTransceiver::new("900001", "secret", "CMT", 0x34);
        let original = Pdu::from(bt).to_pdu_bytes(11).to_vec();

        let unified = SmppAdapter.decode(&frame_of(original.clone())).unwrap();
        match &unified {
            UnifiedMessage::Bind(b) => {
                assert_eq!(b.client_id, "900001");
                assert_eq!(b.authenticator, b"secret".to_vec());
                assert_eq!(b.system_type.as_deref(), Some("CMT"));
                assert_eq!(b.mode, BindMode::Transceiver);
                assert_eq!(b.login_mode, None);
                assert_eq!(b.version, 0x34);
            }
            other => panic!("expected Bind, got {other:?}"),
        }
        let reencoded = SmppAdapter.encode(&unified, Sequence::Plain(11)).unwrap();
        assert_eq!(reencoded, original, "BindTransceiver 经统一模型往返后字节应无损一致");
    }

    #[test]
    fn deliver_mo_byte_roundtrip_via_unified() {
        // Deliver(MO) 字节往返：esm_class 非回执位时解码为 UnifiedDeliver；
        // src/dest 的 ton/npi 落 Address，data_coding 落 Encoding，TLV 落 tlvs。
        let mut d = DeliverSm {
            service_type: String::new(),
            source_addr_ton: 1,
            source_addr_npi: 1,
            source_addr: "13800138000".to_string(),
            dest_addr_ton: 5,
            dest_addr_npi: 0,
            destination_addr: "1065900000".to_string(),
            esm_class: 0,
            protocol_id: 0,
            priority_flag: 0,
            schedule_delivery_time: String::new(),
            validity_period: String::new(),
            registered_delivery: 0,
            replace_if_present_flag: 0,
            data_coding: 0x08,
            sm_default_msg_id: 0,
            short_message: b"\x4e\x2d".to_vec(),
            tlvs: vec![Tlv::new(0x0204, vec![0x00, 0x2A])],
        };
        d.esm_class = 0; // MO，非回执
        let original = Pdu::from(d).to_pdu_bytes(12).to_vec();

        let unified = SmppAdapter.decode(&frame_of(original.clone())).unwrap();
        assert!(matches!(unified, UnifiedMessage::Deliver(_)), "expected Deliver");
        let reencoded = SmppAdapter.encode(&unified, Sequence::Plain(12)).unwrap();
        // 注：decode 侧 UnifiedDeliver.extra=ProtocolExtra::None（不透传 service_type 等），
        // 但原 DeliverSm 这些字段均为默认值，故本例可达字节无损一致。
        assert_eq!(reencoded, original, "DeliverSm(MO) 经统一模型往返后字节应无损一致");
    }

    #[test]
    fn report_byte_roundtrip_via_unified() {
        // Report 字节往返：esm_class=0x04 + receipted_message_id TLV(0x001E)。
        // decode 侧丢弃 protocol_id/priority 等（Report 不透传 extra），故走
        // decode∘encode∘decode 语义稳定验证（而非字节级无损）。
        let d = DeliverSm {
            service_type: String::new(),
            source_addr_ton: 5,
            source_addr_npi: 0,
            source_addr: "1065900000".to_string(),
            dest_addr_ton: 1,
            dest_addr_npi: 1,
            destination_addr: "13800138000".to_string(),
            esm_class: 0x04,
            protocol_id: 0,
            priority_flag: 0,
            schedule_delivery_time: String::new(),
            validity_period: String::new(),
            registered_delivery: 0,
            replace_if_present_flag: 0,
            data_coding: 0,
            sm_default_msg_id: 0,
            short_message: b"id:9876543210 stat:DELIVRD".to_vec(),
            tlvs: vec![Tlv::new(0x001E, b"9876543210".to_vec())],
        };
        let original = Pdu::from(d).to_pdu_bytes(13).to_vec();

        let unified1 = SmppAdapter.decode(&frame_of(original.clone())).unwrap();
        match &unified1 {
            UnifiedMessage::Report(r) => {
                assert_eq!(r.msg_id, MessageId::Text("9876543210".to_string()));
                assert_eq!(r.src.number, "1065900000");
                assert_eq!(r.dest.number, "13800138000");
            }
            other => panic!("expected Report, got {other:?}"),
        }
        // 降级：decode∘encode∘decode 语义稳定。
        let reencoded = SmppAdapter.encode(&unified1, Sequence::Plain(13)).unwrap();
        let unified2 = SmppAdapter.decode(&frame_of(reencoded)).unwrap();
        assert_eq!(unified1, unified2, "Report 经统一模型往返后语义应稳定");
    }

    // ── 结果码透出（#1）：SMPP 结果码在 16B 头的 command_status，需经 adapter 透到统一模型 ──

    /// 把整包 PDU 的 command_status（字节 8..12）设为指定值（测试夹具）。
    fn set_command_status(mut bytes: Vec<u8>, status: u32) -> Vec<u8> {
        bytes[8..12].copy_from_slice(&status.to_be_bytes());
        bytes
    }

    #[test]
    fn decode_submit_resp_surfaces_command_status() {
        // 非 0 command_status（ESME_RSUBMITFAIL=0x45）应透出到 UnifiedSubmitResp.status。
        let bytes = Pdu::from(SubmitSmResp { message_id: "9".to_string(), command_status: 0 })
            .to_pdu_bytes(7)
            .to_vec();
        let bytes = set_command_status(bytes, 0x0000_0045);
        match SmppAdapter.decode(&frame_of(bytes)).unwrap() {
            UnifiedMessage::SubmitResp(r) => {
                assert_eq!(r.status, 0x45, "command_status 应透出到 status，而非恒 0");
            }
            other => panic!("expected SubmitResp, got {other:?}"),
        }
    }

    #[test]
    fn decode_bind_resp_surfaces_command_status() {
        // 非 0 command_status（ESME_RBINDFAIL=0x0D）应透出到 UnifiedBindResp.status。
        let bytes = Pdu::from(BindTransceiverResp {
            system_id: "900001".to_string(),
            sc_interface_version: 0x34,
            command_status: 0,
        })
        .to_pdu_bytes(11)
        .to_vec();
        let bytes = set_command_status(bytes, 0x0000_000D);
        match SmppAdapter.decode(&frame_of(bytes)).unwrap() {
            UnifiedMessage::BindResp(r) => {
                assert_eq!(r.status, 0x0D, "bind command_status 应透出到 status");
            }
            other => panic!("expected BindResp, got {other:?}"),
        }
    }

    #[test]
    fn encode_submit_resp_writes_command_status() {
        // UnifiedSubmitResp.status 应写进 PDU 头部 command_status（rsms 作 server 回失败）。
        let msg = UnifiedMessage::SubmitResp(rsms_model::UnifiedSubmitResp {
            msg_id: MessageId::Text("9".to_string()),
            status: 0x58, // ESME_RTHROTTLED
        });
        let bytes = SmppAdapter.encode(&msg, Sequence::Plain(1)).unwrap();
        let cs = u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        assert_eq!(cs, 0x58, "status 应写入 command_status，而非恒 0");
    }

    #[test]
    fn encode_bind_resp_writes_command_status() {
        let msg = UnifiedMessage::BindResp(rsms_model::UnifiedBindResp { status: 0x0D });
        let bytes = SmppAdapter.encode(&msg, Sequence::Plain(1)).unwrap();
        let cs = u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        assert_eq!(cs, 0x0D, "bind status 应写入 command_status");
    }

    #[test]
    fn decode_report_parses_delivery_status() {
        // esm_class=0x04 的投递回执，正文含 stat:DELIVRD → 统一模型 status 应解析为 Delivered。
        let d = DeliverSm {
            service_type: String::new(),
            source_addr_ton: 0,
            source_addr_npi: 0,
            source_addr: String::new(),
            dest_addr_ton: 0,
            dest_addr_npi: 0,
            destination_addr: "1065900000".to_string(),
            esm_class: 0x04,
            protocol_id: 0,
            priority_flag: 0,
            schedule_delivery_time: String::new(),
            validity_period: String::new(),
            registered_delivery: 0,
            replace_if_present_flag: 0,
            data_coding: 0,
            sm_default_msg_id: 0,
            short_message: b"id:9876543210 sub:001 dlvrd:001 stat:DELIVRD err:000 text:hi".to_vec(),
            tlvs: vec![Tlv::new(0x001E, b"9876543210".to_vec())],
        };
        let bytes = Pdu::from(d).to_pdu_bytes(20).to_vec();
        match SmppAdapter.decode(&frame_of(bytes)).unwrap() {
            UnifiedMessage::Report(r) => {
                assert_eq!(r.status, DeliveryStatus::Delivered, "stat:DELIVRD 应解析为 Delivered");
            }
            other => panic!("expected Report, got {other:?}"),
        }
    }

    #[test]
    fn submit_concat_udh_roundtrip_via_unified() {
        // esm_class 0x40(UDHI) + 8-bit 级联 UDH(05 00 03 ref total seq) + payload。
        let mut s = SubmitSm::new();
        s.source_addr = "10086".to_string();
        s.destination_addr = "13800138000".to_string();
        s.esm_class = 0x40;
        s.data_coding = 0;
        s.short_message = vec![0x05, 0x00, 0x03, 7, 2, 1, b'A', b'B'];
        let original = Pdu::from(s).to_pdu_bytes(30).to_vec();

        let unified = SmppAdapter.decode(&frame_of(original.clone())).unwrap();
        match &unified {
            UnifiedMessage::Submit(u) => {
                assert_eq!(u.concat, Some(Concat { reference: 7, total: 2, sequence: 1 }));
                assert_eq!(u.content, b"AB", "content 应为剥离 UDH 后的纯 payload");
            }
            other => panic!("expected Submit, got {other:?}"),
        }
        // encode 重建 UDH + 置 esm_class 0x40 → 字节与原始无损一致。
        let reencoded = SmppAdapter.encode(&unified, Sequence::Plain(30)).unwrap();
        assert_eq!(reencoded, original, "级联 Submit 经统一模型往返字节应无损一致");
    }

    #[test]
    fn report_resp_equals_deliver_resp() {
        let seq = Sequence::Plain(42);
        let a = SmppAdapter.encode(&UnifiedMessage::ReportResp, seq).unwrap();
        let b = SmppAdapter.encode(&UnifiedMessage::DeliverResp, seq).unwrap();
        assert_eq!(a, b);
    }
}
