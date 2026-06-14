//! SmgpAdapter：复用现有 decode_message/encode_message，做 SmgpMessage ↔ UnifiedMessage 翻译。

use crate::datatypes::{
    Deliver, DeliverResp, Login, LoginResp, OptionalParameters, SmgpMsgId, Submit, SubmitResp,
    Tlv as SmgpTlv,
};
use crate::message::{decode_message, encode_message, SmgpMessage};
use rsms_core::{Frame, Protocol, Result, RsmsError};
use rsms_model::{
    Address, BindMode, DeliveryStatus, Encoding, MessageId, ProtocolAdapter, ProtocolExtra,
    Sequence, SmgpExtra, UnifiedBind, UnifiedBindResp, UnifiedDeliver, UnifiedMessage,
    UnifiedReport, UnifiedSubmit, UnifiedSubmitResp,
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
        // SMGP 无 GSM7 专属 msg_fmt，按 ASCII(0) 落地（有损：回译为 Ascii）
        Encoding::Ascii | Encoding::Gsm7 => 0,
        Encoding::Binary => 4,
        Encoding::Ucs2 => 8,
        Encoding::Gbk => 15,
        Encoding::Other(v) => v,
    }
}

// ──────────────────────────────────────────────
// SMGP 可选参数(TLV) ↔ 统一模型 Tlv 互转
// 注：SMGP 把 TP_UDHI/TP_PID 等放在可选参数里（不像 SMPP 的 esm_class 固定位）。
// 长短信必须带 TP_UDHI=1，否则对端不知道正文含 UDH、不会重组（联调实测：缺它则 cmos 把
// UDH 字节当正文，长短信内容/echo 全错）。这里做按值长度的通用类型映射，wire 字节无歧义。
// ──────────────────────────────────────────────

fn optional_params_to_tlvs(op: &OptionalParameters) -> Vec<rsms_model::Tlv> {
    op.tlvs
        .iter()
        .map(|t| {
            let (tag, value) = match t {
                SmgpTlv::Byte { tag, value } => (*tag, vec![*value]),
                SmgpTlv::Short { tag, value } => (*tag, value.to_be_bytes().to_vec()),
                SmgpTlv::Int { tag, value } => (*tag, value.to_be_bytes().to_vec()),
                SmgpTlv::String { tag, value } => (*tag, value.as_bytes().to_vec()),
                SmgpTlv::Octets { tag, value } => (*tag, value.clone()),
                SmgpTlv::Empty { tag } => (*tag, vec![]),
            };
            rsms_model::Tlv { tag, value }
        })
        .collect()
}

fn tlvs_to_optional_params(tlvs: &[rsms_model::Tlv]) -> OptionalParameters {
    let mut op = OptionalParameters::new();
    for t in tlvs {
        // 按值长度选 TLV 类型：1→Byte、2→Short、4→Int、0→Empty、其余→Octets。
        // 这些类型在 wire 上都是 tag+len+value，故同长度互换字节一致、roundtrip 无损。
        let tlv = match t.value.as_slice() {
            [b] => SmgpTlv::Byte { tag: t.tag, value: *b },
            [a, b] => SmgpTlv::Short { tag: t.tag, value: u16::from_be_bytes([*a, *b]) },
            [a, b, c, d] => SmgpTlv::Int { tag: t.tag, value: u32::from_be_bytes([*a, *b, *c, *d]) },
            [] => SmgpTlv::Empty { tag: t.tag },
            other => SmgpTlv::Octets { tag: t.tag, value: other.to_vec() },
        };
        op.add(tlv);
    }
    op
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
        tlvs: optional_params_to_tlvs(&s.optional_params),
    }
}

fn deliver_to_unified(d: Deliver) -> UnifiedMessage {
    if d.is_report != 0 {
        // SMGP 报告正文为文本（形如 id:.. stat:DELIVRD ..），解析 stat 段 → DeliveryStatus。
        let status = DeliveryStatus::from_receipt_text(&d.msg_content);
        UnifiedMessage::Report(UnifiedReport {
            msg_id: MessageId::Binary(d.msg_id.bytes.to_vec()),
            status,
            // 报告源地址：SMGP Deliver-报告的 src_term_id（构造下行回执需要）。
            src: Address::plain(d.src_term_id.clone()),
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
            tlvs: optional_params_to_tlvs(&d.optional_params),
        })
    }
}

fn login_to_unified(l: Login) -> UnifiedBind {
    UnifiedBind {
        client_id: l.client_id,
        authenticator: l.authenticator.to_vec(),
        timestamp: l.timestamp,
        version: l.version,
        // SMGP 无 system_type；mode 取默认（SMGP 不区分收发模式，encode 时忽略）。
        system_type: None,
        mode: BindMode::default(),
        // SMGP 的 login_mode 进统一模型（0/1/2，DUPLEX 端点须 2）。
        login_mode: Some(l.login_mode),
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
            // authenticator/version 试点期不进统一模型，待后续补充（统一模型暂无对应字段）
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

fn unified_to_smgp(msg: &UnifiedMessage, seq: Sequence) -> Result<SmgpMessage> {
    // SMGP 头部为单字段序列号，从任意 Sequence 退化到 u32。
    let seq = seq.as_u32();
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
            // 把统一模型 tlvs 映射为 SMGP 可选参数（含长短信必需的 TP_UDHI=1）。
            sub.optional_params = tlvs_to_optional_params(&s.tlvs);
            SmgpMessage::Submit {
                sequence_id: seq,
                submit: sub,
            }
        }
        UnifiedMessage::SubmitResp(r) => {
            // 试pilot期 SMGP msg_id 固定为 10 字节；跨协议场景下超过 10 字节的 msg_id
            // 会被静默截断——这是已知有损行为，待统一模型支持可变长 id 后再消除。
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
            // adapter 的 DeliverResp 不携带回执 MsgId（统一模型无此字段），置默认；仅用于 shadow/收敛。
            resp: DeliverResp { msg_id: SmgpMsgId::default(), status: 0 },
        },
        UnifiedMessage::Bind(b) => {
            // authenticator 取前 16 字节填入定长 [u8;16]（不足右补 0）。
            // 注意：此处不计算 MD5——上层（业务/AuthHandler）须传入已算好的 16B 摘要。
            let mut auth = [0u8; 16];
            let n = b.authenticator.len().min(16);
            auth[..n].copy_from_slice(&b.authenticator[..n]);
            SmgpMessage::Login {
                sequence_id: seq,
                login: Login {
                    client_id: b.client_id.clone(),
                    authenticator: auth,
                    // login_mode 缺省按 2（DUPLEX 端点要求）。
                    login_mode: b.login_mode.unwrap_or(2),
                    timestamp: b.timestamp,
                    version: b.version,
                },
            }
        }
        UnifiedMessage::BindResp(r) => {
            // best-effort：统一模型 BindResp 仅有 status，authenticator/version 取默认。
            // example 不依赖此分支；框架 AuthHandler 自走 codec 构造 LoginResp。
            SmgpMessage::LoginResp {
                sequence_id: seq,
                resp: LoginResp {
                    status: r.status,
                    authenticator: [0u8; 16],
                    version: 0,
                },
            }
        }
        UnifiedMessage::Deliver(d) => {
            // 上行 MO：is_report=0，其余字段走 Deliver::new() 默认（recv_time/reserve 等）。
            let mut deliver = Deliver::new();
            deliver.is_report = 0;
            deliver.src_term_id = d.src.number.clone();
            deliver.dest_term_id = d.dest.number.clone();
            deliver.msg_content = d.content.clone();
            deliver.msg_fmt = fmt_from_encoding(d.encoding);
            // 下行 MO 的可选参数（含长短信 TP_UDHI=1）。
            deliver.optional_params = tlvs_to_optional_params(&d.tlvs);
            SmgpMessage::Deliver {
                sequence_id: seq,
                deliver,
            }
        }
        UnifiedMessage::Report(r) => {
            // 下行回执：is_report=1，msg_id 取统一模型 MessageId 前 10 字节填入 SmgpMsgId。
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
            let mut deliver = Deliver::new();
            deliver.is_report = 1;
            deliver.msg_id = SmgpMsgId::new(bytes10);
            deliver.src_term_id = r.src.number.clone();
            deliver.dest_term_id = r.dest.number.clone();
            deliver.msg_content = r.raw.clone();
            SmgpMessage::Deliver {
                sequence_id: seq,
                deliver,
            }
        }
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
    fn encode(&self, msg: &UnifiedMessage, seq: Sequence) -> Result<Vec<u8>> {
        let smgp = unified_to_smgp(msg, seq)?;
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
    use rsms_core::RawPdu;

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
        let reencoded = SmgpAdapter
            .encode(&unified, Sequence::Plain(42))
            .unwrap();
        assert_eq!(reencoded, original, "经统一模型往返后字节应无损一致");
    }

    #[test]
    fn submit_byte_roundtrip_full_fields() {
        // 填满字段的往返测试：验证所有 SmgpExtra 字段、非默认编码、want_report
        // 都能在 decode → UnifiedMessage → encode 中无损保持。
        let mut submit = Submit::new();
        submit.src_term_id = "1065900000".to_string();
        submit.dest_term_ids.push("13800138000".to_string());
        submit.dest_term_id_count = 1;
        submit.msg_content = b"\x4e\x2d\x65\x87".to_vec(); // UCS-2 "中文"
        submit.need_report = 1;
        submit.msg_fmt = 8; // Ucs2
        submit.service_id = "SVCTEST".to_string();
        submit.fee_type = "02".to_string();
        submit.fee_code = "000100".to_string();
        submit.fixed_fee = "000000".to_string();
        submit.msg_type = 6;
        submit.priority = 2;
        submit.charge_term_id = "13900000001".to_string();
        submit.valid_time = "20261231235959".to_string();
        submit.at_time = "20260101000000".to_string();

        let original = Pdu::from(submit).to_pdu_bytes(99).to_vec();
        let unified = SmgpAdapter.decode(&frame_of(original.clone())).unwrap();

        // 验证 decode 方向字段正确搬运
        match &unified {
            UnifiedMessage::Submit(s) => {
                assert_eq!(s.want_report, true, "need_report=1 应翻译为 want_report=true");
                assert!(
                    matches!(s.encoding, rsms_model::Encoding::Ucs2),
                    "msg_fmt=8 应翻译为 Ucs2"
                );
                match &s.extra {
                    ProtocolExtra::Smgp(e) => {
                        assert_eq!(e.service_id, "SVCTEST");
                        assert_eq!(e.fee_type, "02");
                        assert_eq!(e.fee_code, "000100");
                        assert_eq!(e.fixed_fee, "000000");
                        assert_eq!(e.msg_type, 6);
                        assert_eq!(e.priority, 2);
                        assert_eq!(e.charge_term_id, "13900000001");
                        assert_eq!(e.valid_time, "20261231235959");
                        assert_eq!(e.at_time, "20260101000000");
                    }
                    _ => panic!("extra 应为 ProtocolExtra::Smgp"),
                }
            }
            _ => panic!("expected UnifiedMessage::Submit"),
        }

        // 验证 encode 方向字节无损
        let reencoded = SmgpAdapter
            .encode(&unified, Sequence::Plain(99))
            .unwrap();
        assert_eq!(reencoded, original, "填满字段后经统一模型往返字节应无损一致");
    }

    #[test]
    fn bind_byte_roundtrip_via_unified() {
        // SMGP Login → bytes → decode → UnifiedMessage::Bind → encode → bytes，字节应一致。
        // Login 的全部字段（client_id/authenticator/login_mode/timestamp/version）均进统一模型，
        // 故字节往返无损。
        let login = Login {
            client_id: "client01".to_string(),
            authenticator: [0x12u8; 16],
            login_mode: 2,
            timestamp: 0x01020304,
            version: 0x30,
        };
        let original = Pdu::from(login).to_pdu_bytes(11).to_vec();
        let unified = SmgpAdapter.decode(&frame_of(original.clone())).unwrap();
        match &unified {
            UnifiedMessage::Bind(b) => {
                assert_eq!(b.client_id, "client01");
                assert_eq!(b.authenticator, vec![0x12u8; 16]);
                assert_eq!(b.login_mode, Some(2), "login_mode 须进统一模型");
                assert_eq!(b.timestamp, 0x01020304);
                assert_eq!(b.version, 0x30);
            }
            _ => panic!("expected UnifiedMessage::Bind"),
        }
        let reencoded = SmgpAdapter
            .encode(&unified, Sequence::Plain(11))
            .unwrap();
        assert_eq!(reencoded, original, "Login 经统一模型往返字节应无损一致");
    }

    #[test]
    fn deliver_mo_byte_roundtrip_via_unified() {
        // SMGP Deliver(MO, is_report=0) → bytes → decode → UnifiedMessage::Deliver → encode → bytes。
        // UnifiedDeliver 不携带 recv_time/reserve/optional_params，故原始 Deliver 只用 new() 默认值
        // 填这些字段，往返字节即无损一致。
        let mut deliver = Deliver::new();
        deliver.is_report = 0;
        deliver.src_term_id = "13800138000".to_string();
        deliver.dest_term_id = "1065900000".to_string();
        deliver.msg_content = b"MO hello".to_vec();
        deliver.msg_fmt = 8; // Ucs2，验证 encoding 往返
        let original = Pdu::from(deliver).to_pdu_bytes(12).to_vec();
        let unified = SmgpAdapter.decode(&frame_of(original.clone())).unwrap();
        match &unified {
            UnifiedMessage::Deliver(d) => {
                assert_eq!(d.src.number, "13800138000");
                assert_eq!(d.dest.number, "1065900000");
                assert_eq!(d.content, b"MO hello");
                assert!(matches!(d.encoding, Encoding::Ucs2), "msg_fmt=8 应为 Ucs2");
            }
            _ => panic!("expected UnifiedMessage::Deliver"),
        }
        let reencoded = SmgpAdapter
            .encode(&unified, Sequence::Plain(12))
            .unwrap();
        assert_eq!(reencoded, original, "Deliver(MO) 经统一模型往返字节应无损一致");
    }

    #[test]
    fn report_byte_roundtrip_via_unified() {
        // SMGP Deliver(报告, is_report=1) → bytes → decode → UnifiedMessage::Report → encode → bytes。
        // UnifiedReport 保留 msg_id/src/dest/raw；msg_fmt/recv_time/reserve 不进模型，故原始
        // Deliver 这些字段须用 new() 默认值（msg_fmt=0），往返字节才无损一致。
        let mut deliver = Deliver::new();
        deliver.is_report = 1;
        deliver.msg_id = SmgpMsgId::new([0x68, 0x24, 0xEF, 0x06, 0x14, 0x00, 0x18, 0x71, 0x21, 0x80]);
        deliver.src_term_id = "1065900000".to_string();
        deliver.dest_term_id = "13800138000".to_string();
        deliver.msg_content = b"id:report-raw".to_vec();
        let original = Pdu::from(deliver).to_pdu_bytes(13).to_vec();
        let unified = SmgpAdapter.decode(&frame_of(original.clone())).unwrap();
        match &unified {
            UnifiedMessage::Report(r) => {
                assert_eq!(
                    r.msg_id,
                    MessageId::Binary(vec![
                        0x68, 0x24, 0xEF, 0x06, 0x14, 0x00, 0x18, 0x71, 0x21, 0x80
                    ])
                );
                assert_eq!(r.src.number, "1065900000", "报告源地址须进统一模型");
                assert_eq!(r.dest.number, "13800138000");
                assert_eq!(r.raw, b"id:report-raw");
            }
            _ => panic!("expected UnifiedMessage::Report"),
        }
        let reencoded = SmgpAdapter
            .encode(&unified, Sequence::Plain(13))
            .unwrap();
        assert_eq!(reencoded, original, "Deliver(报告) 经统一模型往返字节应无损一致");
    }

    #[test]
    fn decode_report_parses_delivery_status() {
        // is_report=1 的 Deliver，正文含 stat:DELIVRD → 统一模型 status 应解析为 Delivered。
        let mut d = crate::datatypes::Deliver::new();
        d.is_report = 1;
        d.dest_term_id = "1065900000".to_string();
        d.msg_content = b"id:1234567890 sub:001 dlvrd:001 stat:DELIVRD err:000 text:hi".to_vec();
        let bytes = Pdu::from(d).to_pdu_bytes(14).to_vec();
        match SmgpAdapter.decode(&frame_of(bytes)).unwrap() {
            UnifiedMessage::Report(r) => {
                assert_eq!(r.status, DeliveryStatus::Delivered, "stat:DELIVRD 应解析为 Delivered");
            }
            _ => panic!("expected Report"),
        }
    }
}
