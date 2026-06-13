//! 统一模型的语义类型：编码、投递状态、地址、分片、消息 ID、TLV、协议扩展。

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
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CmppExtra {
    /// CMPP 版本魔数：0x30=V3.0（本轮唯一支持）。0x20 预留。
    pub version: u8,
    /// 8 字节 msg_id，空 Vec 视为 [0u8;8]。
    pub msg_id: Vec<u8>,
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
}
