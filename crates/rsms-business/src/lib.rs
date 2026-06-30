//! rsms-business：统一消息处理器（`MessageHandler`）与协议抽象层（`RateLimiter`、`ProtocolConnection`）。
use async_trait::async_trait;
use rsms_core::Result;
use std::sync::Arc;

mod message_context;
pub use message_context::MessageContext;

mod message_handler;
pub use message_handler::{run_message_chain, MessageHandler, RawFrameHandler};

#[async_trait]
pub trait RateLimiter: Send + Sync {
    async fn try_acquire(&self) -> bool;
    async fn acquire(&self, timeout: std::time::Duration) -> bool;
}

#[async_trait]
pub trait ProtocolConnection: Send + Sync {
    fn id(&self) -> u64;
    async fn write_frame(&self, data: &[u8]) -> Result<()>;
    async fn authenticated_account(&self) -> Option<String>;
    async fn rate_limiter(&self) -> Option<Arc<dyn RateLimiter>>;
    /// 握手协商的协议版本字节（如 CMPP 2.0 = `0x20`、3.0 = `0x30`）；未握手或协议无版本概念时为 `None`。
    ///
    /// 业务方据此做版本感知的解码/编码（CMPP 2.0 与 3.0 命令字相同但字段宽度不同）。
    async fn protocol_version(&self) -> Option<u8>;
}

