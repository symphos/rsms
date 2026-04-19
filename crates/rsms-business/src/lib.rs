//! 业务处理器扩展（对齐 `BusinessHandlerInterface`）。
use async_trait::async_trait;
use rsms_core::{EndpointConfig, Frame, Result};
use std::sync::Arc;

pub struct InboundContext {
    pub endpoint: Arc<EndpointConfig>,
    pub conn: Arc<dyn ProtocolConnection>,
}

#[async_trait]
pub trait BusinessHandler: Send + Sync {
    fn name(&self) -> &'static str;
    async fn on_inbound(&self, ctx: &InboundContext, frame: &Frame) -> Result<()>;
}

pub struct NoopBusiness;

#[async_trait]
impl BusinessHandler for NoopBusiness {
    fn name(&self) -> &'static str {
        "noop"
    }

    async fn on_inbound(&self, ctx: &InboundContext, frame: &Frame) -> Result<()> {
        tracing::debug!(endpoint = %ctx.endpoint.id, conn_id = ctx.conn.id(), pdu_len = frame.len(), "noop business handler");
        Ok(())
    }
}

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
}

pub async fn run_chain(
    endpoint: Arc<EndpointConfig>,
    conn: Arc<dyn ProtocolConnection>,
    handlers: &[Arc<dyn BusinessHandler>],
    frame: &Frame,
) -> Result<()> {
    let ctx = InboundContext { endpoint, conn };
    if handlers.is_empty() {
        return NoopBusiness.on_inbound(&ctx, frame).await;
    }
    for h in handlers {
        h.on_inbound(&ctx, frame).await?;
    }
    Ok(())
}