//! 协议适配器 trait：各协议 codec 实现它，负责帧 ↔ 统一消息的双向翻译。

use crate::message::UnifiedMessage;
use crate::types::Sequence;
use rsms_core::{Frame, Protocol, Result};

/// 协议适配器。实现者把已切好的帧解码为统一消息，以及把统一消息编码为帧字节。
pub trait ProtocolAdapter: Send + Sync {
    /// 该适配器对应的协议。
    fn protocol(&self) -> Protocol;

    /// 把一个已切好边界的帧解码为统一消息。
    fn decode(&self, frame: &Frame) -> Result<UnifiedMessage>;

    /// 把统一消息编码为完整帧字节（含协议头，写入序列号）。
    /// `seq` 用 [`Sequence`] 抽象：CMPP/SMGP/SMPP 取 `Plain(u32)`，
    /// SGIP 取 `Sgip{node_id,timestamp,number}`（响应帧须回显请求序列）。
    fn encode(&self, msg: &UnifiedMessage, seq: Sequence) -> Result<Vec<u8>>;

    /// 从请求帧取出「回复时应回显的序列」。多数协议头序列是单 u32，默认取 `frame.sequence_id`；
    /// SGIP 序列是 12 字节复合，需 override 解出 `Sequence::Sgip`（避免业务层手剥字节偏移）。
    fn sequence_of(&self, frame: &Frame) -> Sequence {
        Sequence::Plain(frame.sequence_id)
    }
}
