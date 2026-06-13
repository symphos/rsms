//! 协议适配器 trait：各协议 codec 实现它，负责帧 ↔ 统一消息的双向翻译。

use crate::message::UnifiedMessage;
use rsms_core::{Frame, Protocol, Result};

/// 协议适配器。实现者把已切好的帧解码为统一消息，以及把统一消息编码为帧字节。
pub trait ProtocolAdapter: Send + Sync {
    /// 该适配器对应的协议。
    fn protocol(&self) -> Protocol;

    /// 把一个已切好边界的帧解码为统一消息。
    fn decode(&self, frame: &Frame) -> Result<UnifiedMessage>;

    /// 把统一消息编码为完整帧字节（含协议头，写入 sequence_id）。
    fn encode(&self, msg: &UnifiedMessage, sequence_id: u32) -> Result<Vec<u8>>;
}
