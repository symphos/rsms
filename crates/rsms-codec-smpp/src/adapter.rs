//! SmppAdapter：复用 decode_message + Pdu::to_pdu_bytes，做 SmppMessage ↔ UnifiedMessage 翻译。
//! 验证窄腰对 TON/NPI（落 Address）与 TLV（落 tlvs）的吸收能力。
//!
//! 已知限制（本轮）：encode 不支持 Bind/BindResp（仅 Submit/SubmitResp/DeliverResp/心跳/解绑，
//! 与 SMGP adapter 一致）；BindResp.status 暂填 0（SMPP 结果码在 command_status，decode 未透出）；
//! Report 路径未解析 MESSAGE_STATE 等 TLV，status 暂为 Unknown。

use crate::codec::Pdu;
use crate::datatypes::{
    CommandId, DeliverSm, DeliverSmResp, EnquireLink, EnquireLinkResp, SubmitSm, SubmitSmResp,
    Tlv, Unbind, UnbindResp,
};
use crate::message::{decode_message, SmppMessage};
use rsms_core::{Frame, Protocol, Result, RsmsError};
use rsms_model::{
    Address, DeliveryStatus, Encoding, MessageId, ProtocolAdapter, ProtocolExtra, SmppExtra, Tlv as UTlv,
    UnifiedBind, UnifiedDeliver, UnifiedMessage, UnifiedReport, UnifiedSubmit, UnifiedSubmitResp,
};

/// SMPP 协议适配器。
pub struct SmppAdapter;

// ── 编码翻译表（SMPP data_coding ↔ 统一 Encoding）──
fn encoding_from_dcs(dcs: u8) -> Encoding {
    match dcs {
        0x00 => Encoding::Gsm7,
        0x01 => Encoding::Ascii,
        0x02 => Encoding::Binary,
        0x10 => Encoding::Ucs2,
        other => Encoding::Other(other),
    }
}
fn dcs_from_encoding(enc: Encoding) -> u8 {
    match enc {
        Encoding::Gsm7 => 0x00,
        Encoding::Ascii => 0x01,
        Encoding::Binary => 0x02,
        Encoding::Ucs2 => 0x10,
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

fn bind_to_unified(system_id: String, password: String, interface_version: u8) -> UnifiedBind {
    UnifiedBind {
        client_id: system_id,
        authenticator: password.into_bytes(),
        timestamp: 0,
        version: interface_version,
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
        SmppMessage::BindTransmitter(b) => {
            UnifiedMessage::Bind(bind_to_unified(b.system_id, b.password, b.interface_version))
        }
        SmppMessage::BindReceiver(b) => {
            UnifiedMessage::Bind(bind_to_unified(b.system_id, b.password, b.interface_version))
        }
        SmppMessage::BindTransceiver(b) => {
            UnifiedMessage::Bind(bind_to_unified(b.system_id, b.password, b.interface_version))
        }
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
fn unified_to_smpp_bytes(msg: &UnifiedMessage, seq: u32) -> Result<Vec<u8>> {
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
    fn encode(&self, msg: &UnifiedMessage, sequence_id: u32) -> Result<Vec<u8>> {
        unified_to_smpp_bytes(msg, sequence_id)
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
        s.data_coding = 0x10; // Ucs2
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
        s.data_coding = 0x10;
        s.short_message = b"\x4e\x2d".to_vec();
        s.tlvs.push(crate::datatypes::Tlv::new(0x0204, vec![0x00, 0x2A]));
        let original = Pdu::from(s).to_pdu_bytes(42).to_vec();

        let unified = SmppAdapter.decode(&frame_of(original.clone())).unwrap();
        let reencoded = SmppAdapter.encode(&unified, 42).unwrap();
        assert_eq!(reencoded, original, "SMPP 经统一模型往返后字节应无损一致");
    }
}
