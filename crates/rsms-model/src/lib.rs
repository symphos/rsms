//! 协议无关的统一短信消息模型与协议适配器 trait（窄腰层）。

mod adapter;
mod message;
mod types;

pub use adapter::ProtocolAdapter;
pub use message::*;
pub use types::*;
