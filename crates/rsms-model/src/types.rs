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

/// 协议特有方言字段（typed，非 map）。试点期只填 Smgp。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolExtra {
    None,
    Smgp(SmgpExtra),
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
}
