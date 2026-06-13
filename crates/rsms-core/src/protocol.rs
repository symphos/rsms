//! 协议类型枚举：统一编解码偏移与帧长，替代易拼错的协议字符串。
//!
//! `EndpointConfig.protocol` 此前是裸 `String`，拼错（如 `"SMPP"`、`"smpp "`）或漏设会
//! 静默退化为 CMPP 偏移，导致 sequence_id 提取错乱、响应匹配失败，且仅在运行期以丢消息
//! 形式暴露。改用本枚举后，协议类型在编译期固定，偏移由枚举派生，根除该类陷阱。

use std::str::FromStr;

use crate::RsmsError;

/// 支持的短信协议类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Protocol {
    /// 中国移动 CMPP 2.0/3.0（12 字节头，sequence_id 在偏移 8）
    #[default]
    Cmpp,
    /// 中国电信 SMGP 3.0（12 字节头，sequence_id 在偏移 8）
    Smgp,
    /// SMPP 3.4/5.0（16 字节头，sequence_id 在偏移 12）
    Smpp,
    /// 中国联通 SGIP 1.2（20 字节头，sequence_id 在偏移 12）
    Sgip,
}

impl Protocol {
    /// PDU 头长度（字节）：CMPP/SMGP 12，SMPP 16，SGIP 20。
    pub fn header_len(&self) -> usize {
        match self {
            Protocol::Cmpp | Protocol::Smgp => 12,
            Protocol::Smpp => 16,
            Protocol::Sgip => 20,
        }
    }

    /// sequence_id 在 PDU 中的字节偏移：CMPP/SMGP 在 8，SMPP/SGIP 在 12。
    pub fn seq_offset(&self) -> usize {
        match self {
            Protocol::Smpp | Protocol::Sgip => 12,
            Protocol::Cmpp | Protocol::Smgp => 8,
        }
    }

    /// 协议名小写字符串（用于日志/展示与对外兼容）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Protocol::Cmpp => "cmpp",
            Protocol::Smgp => "smgp",
            Protocol::Smpp => "smpp",
            Protocol::Sgip => "sgip",
        }
    }
}

impl std::fmt::Display for Protocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Protocol {
    type Err = RsmsError;

    /// 大小写不敏感解析；未知协议返回 `RsmsError::Other`。
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "cmpp" => Ok(Protocol::Cmpp),
            "smgp" => Ok(Protocol::Smgp),
            "smpp" => Ok(Protocol::Smpp),
            "sgip" => Ok(Protocol::Sgip),
            other => Err(RsmsError::Other(format!("unknown protocol: {other}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_match_protocol_headers() {
        assert_eq!(Protocol::Cmpp.seq_offset(), 8);
        assert_eq!(Protocol::Smgp.seq_offset(), 8);
        assert_eq!(Protocol::Smpp.seq_offset(), 12);
        assert_eq!(Protocol::Sgip.seq_offset(), 12);
        assert_eq!(Protocol::Cmpp.header_len(), 12);
        assert_eq!(Protocol::Smpp.header_len(), 16);
        assert_eq!(Protocol::Sgip.header_len(), 20);
    }

    #[test]
    fn parse_is_case_insensitive_and_rejects_unknown() {
        assert_eq!("SMPP".parse::<Protocol>().unwrap(), Protocol::Smpp);
        assert_eq!(" sgip ".parse::<Protocol>().unwrap(), Protocol::Sgip);
        assert!("foo".parse::<Protocol>().is_err());
    }

    #[test]
    fn default_is_cmpp() {
        assert_eq!(Protocol::default(), Protocol::Cmpp);
    }
}
