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
    Address, BindMode, DeliveryStatus, Encoding, MessageId, ProtocolAdapter, ProtocolExtra, Sequence,
    SmppExtra, Tlv as UTlv, UnifiedBind, UnifiedDeliver, UnifiedMessage, UnifiedReport, UnifiedSubmit,
    UnifiedSubmitResp,
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

fn submit_to_unified(s: SubmitSm) -> UnifiedSubmit {
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
        content: s.short_message,
        encoding: encoding_from_dcs(s.data_coding),
        want_report: s.registered_delivery & 0x01 != 0,
        concat: None,
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
        // 投递回执：receipted_message_id TLV(0x001E) 优先，否则空
        let msg_id = d
            .tlvs
            .iter()
            .find(|t| t.tag == 0x001E)
            .map(|t| MessageId::Text(String::from_utf8_lossy(&t.value).trim_end_matches('\0').to_string()))
            .unwrap_or_else(|| MessageId::Text(String::new()));
        UnifiedMessage::Report(UnifiedReport {
            msg_id,
            // 注：DeliverSm 回执的 MESSAGE_STATE(0x0427)/NETWORK_ERROR_CODE 等 TLV 本轮未解析，
            // status 暂为 Unknown，raw 仅含 short_message。精确状态解析待后续。
            status: DeliveryStatus::Unknown,
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
        UnifiedMessage::Deliver(UnifiedDeliver {
            src: Address {
                number: d.source_addr,
                ton: Some(d.source_addr_ton),
                npi: Some(d.source_addr_npi),
            },
            dest,
            content: d.short_message,
            encoding: encoding_from_dcs(d.data_coding),
            concat: None,
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
            status: 0,
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
        // SMPP BindResp 的结果码在 PDU command_status，decode_message 读后丢弃、未透出；
        // 本轮 BindResp.status 暂填 0（不可用 sc_interface_version 充当 status——它是服务端协议版本，非结果码）。
        SmppMessage::BindTransmitterResp(_)
        | SmppMessage::BindReceiverResp(_)
        | SmppMessage::BindTransceiverResp(_) => {
            UnifiedMessage::BindResp(rsms_model::UnifiedBindResp { status: 0 })
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
            sm.esm_class = extra.esm_class;
            sm.protocol_id = extra.protocol_id;
            sm.priority_flag = extra.priority_flag;
            sm.schedule_delivery_time = extra.schedule_delivery_time;
            sm.validity_period = extra.validity_period;
            sm.registered_delivery = extra.registered_delivery;
            sm.replace_if_present_flag = extra.replace_if_present_flag;
            sm.data_coding = dcs_from_encoding(s.encoding);
            sm.sm_default_msg_id = extra.sm_default_msg_id;
            sm.short_message = s.content.clone();
            sm.tlvs = tlvs_from_unified(&s.tlvs);
            Pdu::from(sm)
        }
        UnifiedMessage::SubmitResp(r) => {
            let message_id = match &r.msg_id {
                MessageId::Text(t) => t.clone(),
                MessageId::Binary(b) => String::from_utf8_lossy(b).into_owned(),
            };
            Pdu::from(SubmitSmResp { message_id })
        }
        UnifiedMessage::DeliverResp => Pdu::from(DeliverSmResp { message_id: String::new() }),
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
        UnifiedMessage::BindResp(_) => {
            // best-effort：统一模型 BindResp 仅带 status，无法区分 transceiver/transmitter/receiver resp，
            // 默认产出 BindTransceiverResp。结果码在头部 command_status，本 codec 路径不透传 status；
            // example 不依赖此分支，框架 AuthHandler 走 codec 直接构造响应。
            Pdu::from(BindTransceiverResp { system_id: String::new(), sc_interface_version: 0 })
        }
        UnifiedMessage::Deliver(d) => {
            // MO 上行 → DeliverSm（esm_class 非回执位）。
            let extra = match &d.extra {
                ProtocolExtra::Smpp(e) => e.clone(),
                _ => SmppExtra::default(),
            };
            let mut sm = DeliverSm {
                service_type: extra.service_type,
                source_addr_ton: d.src.ton.unwrap_or(0),
                source_addr_npi: d.src.npi.unwrap_or(0),
                source_addr: d.src.number.clone(),
                dest_addr_ton: d.dest.ton.unwrap_or(0),
                dest_addr_npi: d.dest.npi.unwrap_or(0),
                destination_addr: d.dest.number.clone(),
                esm_class: extra.esm_class,
                protocol_id: extra.protocol_id,
                priority_flag: extra.priority_flag,
                schedule_delivery_time: extra.schedule_delivery_time,
                validity_period: extra.validity_period,
                registered_delivery: extra.registered_delivery,
                replace_if_present_flag: extra.replace_if_present_flag,
                data_coding: dcs_from_encoding(d.encoding),
                sm_default_msg_id: extra.sm_default_msg_id,
                short_message: d.content.clone(),
                tlvs: tlvs_from_unified(&d.tlvs),
            };
            // ProtocolExtra::None 时 esm_class 已为 0；MO 不应置回执位，保持 extra 提供的值。
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
}
