//! 统一模型的语义类型：编码、投递状态、地址、分片、消息 ID、TLV、协议扩展。

/// 帧序列号（encode 方向用）。多数协议头序列是单个 u32（CMPP/SMGP/SMPP）；
/// SGIP 的序列是 12 字节复合 `node_id+timestamp+number`，且响应帧必须回显请求序列，
/// 单个 u32 装不下，故抽象为枚举由各 adapter 自行解释。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sequence {
    /// CMPP/SMGP/SMPP：头部单字段序列号。
    Plain(u32),
    /// SGIP：复合序列（node_id + timestamp(MMddHHmmss) + number）。
    Sgip { node_id: u32, timestamp: u32, number: u32 },
}

impl Sequence {
    /// 取「主序列号」：Plain 即其值；Sgip 取 number 分量。
    /// 供 CMPP/SMGP/SMPP adapter 从任意 Sequence 退化到 u32 使用。
    pub fn as_u32(self) -> u32 {
        match self {
            Sequence::Plain(n) => n,
            Sequence::Sgip { number, .. } => number,
        }
    }
}

impl From<u32> for Sequence {
    fn from(n: u32) -> Self {
        Sequence::Plain(n)
    }
}

/// 认证 bind 模式。SMPP 有收发/只发/只收三态（DUPLEX 端点须 Transceiver）；
/// CMPP/SMGP/SGIP 为单一 bind 语义，取默认 `Transceiver` 即可（其 encode 忽略此字段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BindMode {
    #[default]
    Transceiver,
    Transmitter,
    Receiver,
}

/// 短信编码语义（协议魔数由各 adapter 翻译，不上浮到此层）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Gsm7,
    Ascii,
    Ucs2,
    Gbk,
    Binary,
    /// 未识别的协议编码值，保留原值不丢失。
    Other(u8),
}

/// 投递状态语义。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryStatus {
    Delivered,
    Expired,
    Undeliverable,
    Accepted,
    Rejected,
    Unknown,
    /// 未识别的状态文本，保留原值。
    Other(String),
}

impl DeliveryStatus {
    /// 标准 7 字符投递状态码 → 语义状态（SMPP/CMPP/SMGP 投递回执通用，大小写/空白不敏感）。
    /// 未识别码（如 SMPP 的 `DELETED`）保留为 `Other`；空码归 `Unknown`。
    pub fn from_stat_code(code: &str) -> DeliveryStatus {
        match code.trim().to_ascii_uppercase().as_str() {
            "DELIVRD" => DeliveryStatus::Delivered,
            "EXPIRED" => DeliveryStatus::Expired,
            "UNDELIV" => DeliveryStatus::Undeliverable,
            "ACCEPTD" => DeliveryStatus::Accepted,
            "REJECTD" => DeliveryStatus::Rejected,
            "UNKNOWN" => DeliveryStatus::Unknown,
            "" => DeliveryStatus::Unknown,
            other => DeliveryStatus::Other(other.to_string()),
        }
    }

    /// 从投递回执文本（形如 `id:.. stat:DELIVRD err:..`）解析状态；
    /// SMPP/SMGP 回执正文为该格式。找不到 `stat:` 段时返回 `Unknown`。
    pub fn from_receipt_text(text: &[u8]) -> DeliveryStatus {
        let s = String::from_utf8_lossy(text);
        match s.find("stat:") {
            Some(pos) => {
                let code: String = s[pos + 5..]
                    .chars()
                    .take_while(|c| !c.is_whitespace())
                    .collect();
                DeliveryStatus::from_stat_code(&code)
            }
            None => DeliveryStatus::Unknown,
        }
    }
}

/// 短信地址。`ton`/`npi` 是「地址」概念的可选方言修饰（非 SMPP 协议为 None）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    pub number: String,
    pub ton: Option<u8>,
    pub npi: Option<u8>,
}

impl Address {
    /// 构造一个无 TON/NPI 修饰的纯号码地址（CMPP/SMGP/SGIP 用）。
    pub fn plain(number: impl Into<String>) -> Self {
        Self { number: number.into(), ton: None, npi: None }
    }
}

/// 长短信分片信息（UDH 的语义抽象）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Concat {
    pub reference: u16,
    pub total: u8,
    pub sequence: u8,
}

impl Concat {
    /// 构造级联短信 UDH 前缀字节（含 UDHL 前缀）。`reference<=255` 用 8-bit IE(0x00)、
    /// 否则用 16-bit IE(0x08)。供 encode 时前置到正文。遵循 GSM 03.40 UDH 格式。
    pub fn to_udh_prefix(&self) -> Vec<u8> {
        if self.reference <= 255 {
            // UDHL=5: IEI(0x00) IEDL(3) ref(1) total seq
            vec![0x05, 0x00, 0x03, self.reference as u8, self.total, self.sequence]
        } else {
            // UDHL=6: IEI(0x08) IEDL(4) ref_hi ref_lo total seq
            vec![
                0x06,
                0x08,
                0x04,
                (self.reference >> 8) as u8,
                self.reference as u8,
                self.total,
                self.sequence,
            ]
        }
    }

    /// 从正文开头解析级联 UDH。返回 `Some((concat, payload_offset))`——`payload_offset` 为
    /// UDH 占用的总字节数（含 UDHL 前缀），调用方据此切出纯 payload。
    /// 非级联 UDH / 长度不足 / 空内容返回 `None`。
    pub fn from_udh(content: &[u8]) -> Option<(Concat, usize)> {
        if content.is_empty() {
            return None;
        }
        let udhl = content[0] as usize;
        if udhl == 0 || content.len() < 1 + udhl {
            return None;
        }
        let udh = &content[1..1 + udhl];
        // 仅识别级联 IE：8-bit(0x00)/16-bit(0x08)。
        match udh.first().copied()? {
            0x08 if udh.len() >= 6 => Some((
                Concat {
                    reference: u16::from_be_bytes([udh[2], udh[3]]),
                    total: udh[4],
                    sequence: udh[5],
                },
                1 + udhl,
            )),
            0x00 if udh.len() >= 5 => Some((
                Concat {
                    reference: udh[2] as u16,
                    total: udh[3],
                    sequence: udh[4],
                },
                1 + udhl,
            )),
            _ => None,
        }
    }
}

/// 统一消息 ID，吸收各协议形态（CMPP [u8;8]/SMGP 10B/SGIP Sequence/SMPP String）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageId {
    Binary(Vec<u8>),
    Text(String),
}

/// 可选 TLV 参数（SMPP/SMGP）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tlv {
    pub tag: u16,
    pub value: Vec<u8>,
}

/// 协议特有方言字段（typed，非 map）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolExtra {
    None,
    Smgp(SmgpExtra),
    Smpp(SmppExtra),
    Sgip(SgipExtra),
    Cmpp(CmppExtra),
}

/// SMGP 特有字段（计费/类型/优先级/时间）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SmgpExtra {
    pub msg_type: u8,
    pub priority: u8,
    pub service_id: String,
    pub fee_type: String,
    pub fee_code: String,
    pub fixed_fee: String,
    pub charge_term_id: String,
    pub valid_time: String,
    pub at_time: String,
}

/// SMPP 特有方言字段（SubmitSm/DeliverSm 中不进核心模型的部分）。
/// 注：源/目的地址的 ton/npi 进 `Address`，data_coding 进 `Encoding`，
/// TLV 进 `UnifiedSubmit::tlvs`，故此处不含这些。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SmppExtra {
    pub service_type: String,
    pub esm_class: u8,
    pub protocol_id: u8,
    pub priority_flag: u8,
    pub schedule_delivery_time: String,
    pub validity_period: String,
    /// 完整 registered_delivery（bit0 即 want_report，但保留整字节以无损往返）。
    pub registered_delivery: u8,
    pub replace_if_present_flag: u8,
    pub sm_default_msg_id: u8,
}

/// SGIP 特有方言字段（Submit 中不进核心模型的部分）。
/// 注：sp_number→src，user_numbers→dests，message_content→content，
/// msg_fmt→encoding，report_flag→want_report，reserve 默认 [0;8] 不入此处。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SgipExtra {
    pub charge_number: String,
    pub corp_id: String,
    pub service_type: String,
    /// SGIP fee_type 为单字节枚举，区别于 SMGP/CMPP 同名字段的字符串形式。
    pub fee_type: u8,
    pub fee_value: String,
    pub given_value: String,
    pub agent_flag: u8,
    pub morelate_to_mt_flag: u8,
    pub priority: u8,
    pub expire_time: String,
    pub schedule_time: String,
    pub tppid: u8,
    pub tpudhi: u8,
    pub message_type: u8,
}

/// CMPP 特有方言字段（V3.0 Submit 中不进核心模型的部分）。
/// 注：src_id→src，dest_terminal_ids→dests，msg_content→content，
/// msg_fmt→encoding，registered_delivery→want_report，dest_usr_tl 由 dests.len() 推导。
/// 本轮仅 V3.0：encode 恒生成 SubmitV30，故不带 version 字段（V2.0 支持时再加并附逻辑）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CmppExtra {
    /// 8 字节 msg_id（与 codec `Submit::msg_id` 同型，Default 即 [0u8;8]）。
    pub msg_id: [u8; 8],
    pub pk_total: u8,
    pub pk_number: u8,
    pub msg_level: u8,
    pub service_id: String,
    pub fee_user_type: u8,
    pub fee_terminal_id: String,
    pub fee_terminal_type: u8,
    pub tppid: u8,
    pub tpudhi: u8,
    pub msg_src: String,
    pub fee_type: String,
    pub fee_code: String,
    pub valid_time: String,
    pub at_time: String,
    pub dest_terminal_type: u8,
    pub link_id: String,
    /// CMPP 协议版本字节：`0x20`=V2.0、`0x30`=V3.0、`0`(默认)按 V3.0 处理。
    /// encode 据此选 SubmitV20/SubmitV30；decode 由版本感知路径填入。
    pub version: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_plain_has_no_ton_npi() {
        let a = Address::plain("13800138000");
        assert_eq!(a.number, "13800138000");
        assert!(a.ton.is_none() && a.npi.is_none());
    }

    #[test]
    fn protocol_extra_default_smgp() {
        let e = ProtocolExtra::Smgp(SmgpExtra::default());
        assert!(matches!(e, ProtocolExtra::Smgp(_)));
    }

    #[test]
    fn protocol_extra_three_new_variants() {
        assert!(matches!(
            ProtocolExtra::Smpp(SmppExtra::default()),
            ProtocolExtra::Smpp(_)
        ));
        assert!(matches!(
            ProtocolExtra::Sgip(SgipExtra::default()),
            ProtocolExtra::Sgip(_)
        ));
        assert!(matches!(
            ProtocolExtra::Cmpp(CmppExtra::default()),
            ProtocolExtra::Cmpp(_)
        ));
    }

    #[test]
    fn delivery_status_from_stat_code_maps_standard_codes() {
        assert_eq!(DeliveryStatus::from_stat_code("DELIVRD"), DeliveryStatus::Delivered);
        assert_eq!(DeliveryStatus::from_stat_code("EXPIRED"), DeliveryStatus::Expired);
        assert_eq!(DeliveryStatus::from_stat_code("UNDELIV"), DeliveryStatus::Undeliverable);
        assert_eq!(DeliveryStatus::from_stat_code("ACCEPTD"), DeliveryStatus::Accepted);
        assert_eq!(DeliveryStatus::from_stat_code("REJECTD"), DeliveryStatus::Rejected);
        assert_eq!(DeliveryStatus::from_stat_code("UNKNOWN"), DeliveryStatus::Unknown);
        // 大小写/空白不敏感
        assert_eq!(DeliveryStatus::from_stat_code(" delivrd "), DeliveryStatus::Delivered);
        // 未识别码保留原值（如 SMPP 的 DELETED）
        assert_eq!(
            DeliveryStatus::from_stat_code("DELETED"),
            DeliveryStatus::Other("DELETED".to_string())
        );
    }

    #[test]
    fn delivery_status_from_receipt_text_extracts_stat() {
        let text = b"id:9876543210 sub:001 dlvrd:001 stat:DELIVRD err:000 text:hi";
        assert_eq!(DeliveryStatus::from_receipt_text(text), DeliveryStatus::Delivered);
        // 无 stat: 段 → Unknown
        assert_eq!(DeliveryStatus::from_receipt_text(b"no stat here"), DeliveryStatus::Unknown);
    }

    #[test]
    fn concat_udh_8bit_roundtrip() {
        // reference<=255 → 8-bit UDH：UDHL=5, [05 00 03 ref total seq]。
        let c = Concat { reference: 42, total: 3, sequence: 2 };
        assert_eq!(c.to_udh_prefix(), vec![0x05, 0x00, 0x03, 42, 3, 2]);
        let mut content = c.to_udh_prefix();
        content.extend_from_slice(b"payload");
        let (parsed, off) = Concat::from_udh(&content).unwrap();
        assert_eq!(parsed, c);
        assert_eq!(&content[off..], b"payload");
    }

    #[test]
    fn concat_udh_16bit_roundtrip() {
        // reference>255 → 16-bit UDH：UDHL=6, [06 08 04 ref_hi ref_lo total seq]。
        let c = Concat { reference: 300, total: 4, sequence: 1 }; // 300=0x012C
        assert_eq!(c.to_udh_prefix(), vec![0x06, 0x08, 0x04, 0x01, 0x2C, 4, 1]);
        let (parsed, off) = Concat::from_udh(&c.to_udh_prefix()).unwrap();
        assert_eq!(parsed, c);
        assert_eq!(off, 7);
    }

    #[test]
    fn concat_from_udh_none_when_absent() {
        assert!(Concat::from_udh(b"hello").is_none());
        assert!(Concat::from_udh(b"").is_none());
    }
}
