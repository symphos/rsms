//! 协议无关的统一消息枚举与各消息结构。

use crate::types::{
    Address, BindMode, Concat, DeliveryStatus, Encoding, MessageId, ProtocolExtra, Tlv,
};

/// 统一消息（主干；Query/Cancel 等次要消息后续补充）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnifiedMessage {
    Submit(UnifiedSubmit),
    SubmitResp(UnifiedSubmitResp),
    Deliver(UnifiedDeliver),
    DeliverResp,
    Report(UnifiedReport),
    Bind(UnifiedBind),
    BindResp(UnifiedBindResp),
    Unbind,
    UnbindResp,
    Ping,
    PingResp,
    /// 未识别命令，保留原始 body 不丢帧。
    Unknown { command_id: u32, raw: Vec<u8> },
}

/// MT 提交。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnifiedSubmit {
    pub src: Address,
    pub dests: Vec<Address>,
    pub content: Vec<u8>,
    pub encoding: Encoding,
    pub want_report: bool,
    pub concat: Option<Concat>,
    pub extra: ProtocolExtra,
    pub tlvs: Vec<Tlv>,
}

/// MT 提交响应。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnifiedSubmitResp {
    pub msg_id: MessageId,
    pub status: u32,
}

impl UnifiedSubmitResp {
    /// 提交是否成功。`status == 0` 在四协议通用为成功（CMPP Result / SMGP Status /
    /// SMPP ESME_ROK / SGIP Result 均 0=ok）；非 0 的具体含义按协议查 `status` 原始码。
    pub fn is_success(&self) -> bool {
        self.status == 0
    }
}

/// MO 上行（用户发来的短信）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnifiedDeliver {
    pub src: Address,
    pub dest: Address,
    pub content: Vec<u8>,
    pub encoding: Encoding,
    pub concat: Option<Concat>,
    pub extra: ProtocolExtra,
    pub tlvs: Vec<Tlv>,
}

/// 投递状态报告（统一抽象，不论底层是 Deliver 还是独立 Report 命令）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnifiedReport {
    pub msg_id: MessageId,
    pub status: DeliveryStatus,
    /// 报告源地址（构造下行 Deliver-报告需要；CMPP/SMGP/SMPP 的 src_terminal_id 等）。
    pub src: Address,
    pub dest: Address,
    /// 原始报告正文，便于业务需要时取协议原始信息。
    pub raw: Vec<u8>,
}

/// 认证请求（CMPP Connect/SMGP Login/SMPP Bind/SGIP Bind）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnifiedBind {
    pub client_id: String,
    /// 鉴权凭据：CMPP/SMGP 为 16 字节 MD5；SMPP/SGIP 为明文口令字节。
    pub authenticator: Vec<u8>,
    pub timestamp: u32,
    /// 协议版本；SGIP 复用此字段承载 login_type（1=SP→SMG）。
    pub version: u8,
    /// SMPP system_type（CMT 等）；其余协议为 None。
    pub system_type: Option<String>,
    /// SMPP bind 模式（Transceiver/Transmitter/Receiver）；其余协议取默认值、encode 时忽略。
    pub mode: BindMode,
    /// SMGP login_mode（0/1/2，DUPLEX 端点须 2）；其余协议为 None。
    pub login_mode: Option<u8>,
}

/// 认证响应。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnifiedBindResp {
    pub status: u32,
}

impl UnifiedBindResp {
    /// 认证是否成功。`status == 0` 在四协议通用为成功；非 0 含义按协议查原始码。
    pub fn is_success(&self) -> bool {
        self.status == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Address;

    #[test]
    fn build_submit_message() {
        let m = UnifiedMessage::Submit(UnifiedSubmit {
            src: Address::plain("1065900000"),
            dests: vec![Address::plain("13800138000")],
            content: b"hello".to_vec(),
            encoding: Encoding::Gbk,
            want_report: true,
            concat: None,
            extra: ProtocolExtra::None,
            tlvs: vec![],
        });
        assert!(matches!(m, UnifiedMessage::Submit(_)));
    }

    #[test]
    fn submit_resp_is_success_reflects_status() {
        // status==0 即成功（四协议通用），非 0 即失败。
        let ok = UnifiedSubmitResp { msg_id: MessageId::Binary(vec![]), status: 0 };
        let fail = UnifiedSubmitResp { msg_id: MessageId::Binary(vec![]), status: 0x45 };
        assert!(ok.is_success());
        assert!(!fail.is_success());
    }

    #[test]
    fn bind_resp_is_success_reflects_status() {
        assert!(UnifiedBindResp { status: 0 }.is_success());
        assert!(!UnifiedBindResp { status: 0x0D }.is_success());
    }
}
