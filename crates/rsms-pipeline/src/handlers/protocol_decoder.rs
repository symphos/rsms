//! Protocol Type enum

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolType {
    Cmpp,
    Sgip,
    Smgp,
    Smpp,
}

impl ProtocolType {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "cmpp" => Some(ProtocolType::Cmpp),
            "sgip" => Some(ProtocolType::Sgip),
            "smgp" => Some(ProtocolType::Smgp),
            "smpp" => Some(ProtocolType::Smpp),
            _ => None,
        }
    }
}

impl fmt::Display for ProtocolType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProtocolType::Cmpp => write!(f, "CMPP"),
            ProtocolType::Sgip => write!(f, "SGIP"),
            ProtocolType::Smgp => write!(f, "SMGP"),
            ProtocolType::Smpp => write!(f, "SMPP"),
        }
    }
}
