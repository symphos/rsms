//! 业务处理器扩展（对齐 `BusinessHandlerInterface`）。
use async_trait::async_trait;

mod message_context;
pub use message_context::MessageContext;
use rsms_core::{EndpointConfig, Frame, IdGenerator, Result};
use std::sync::Arc;

/// 入站消息的上下文，由框架在每次 `BusinessHandler::on_inbound` 调用前构造并传入。
pub struct InboundContext {
    /// 当前连接所属的端点配置（含协议、鉴权、日志级别等元数据）。
    pub endpoint: Arc<EndpointConfig>,
    /// 当前协议连接句柄，可用于向对端写回响应帧（`write_frame`）。
    pub conn: Arc<dyn ProtocolConnection>,
    /// 该账号的序列号/消息 ID 生成器。
    ///
    /// 服务端连接在账号完成鉴权后由框架注入；客户端连接始终有值。
    /// 鉴权前收到的帧（如 Connect PDU 本身）此字段为 `None`，
    /// 业务方使用前需先 `if let Some(gen) = &ctx.id_generator`。
    pub id_generator: Option<Arc<dyn IdGenerator>>,
}

/// 业务逻辑处理器，由用户实现并注册到服务端或客户端。
///
/// 框架在每收到一个完整入站帧后调用 `on_inbound`，并将上下文与原始帧同时传入。
/// 实现需保证线程安全（`Send + Sync`），通常以 `Arc<MyHandler>` 挂载。
#[async_trait]
pub trait BusinessHandler: Send + Sync {
    /// 返回该处理器的唯一名称，用于日志和调试追踪。
    fn name(&self) -> &'static str;

    /// 处理一条入站帧。
    ///
    /// **框架核心契约**：框架不自动发送 `SubmitResp`/`SubmitSmResp`。
    /// 业务方收到 Submit 类 PDU 后，**必须**自行通过 `ctx.conn.write_frame()` 写回响应帧，
    /// 否则对端滑动窗口将被耗尽，导致吞吐假死（对端无法继续发送新消息）。
    ///
    /// # 参数
    /// - `ctx`：当前连接的上下文，含端点配置、连接句柄和 ID 生成器。
    /// - `frame`：收到的原始帧字节，含完整 PDU（包括协议头）。
    ///
    /// # 返回
    /// 返回 `Ok(())` 表示处理成功，框架继续下一个处理器；
    /// 返回 `Err` 时框架中断处理链并记录错误（连接不会被强制断开）。
    async fn on_inbound(&self, ctx: &InboundContext, frame: &Frame) -> Result<()>;

    /// 统一模型入站回调（窄腰试点）。默认空实现，旧实现者无需改动。
    ///
    /// 业务可选择覆盖此方法，以对协议无关的 [`rsms_model::UnifiedMessage`] 编程，
    /// 而无需直接解析原始帧。与 [`on_inbound`](Self::on_inbound) 并存——框架不自动
    /// 调用此方法，需由适配层或业务自行在 `on_inbound` 中触发。
    ///
    /// # 参数
    /// - `ctx`：当前连接的上下文（与 `on_inbound` 相同）。
    /// - `msg`：已解码的协议无关统一消息。
    #[allow(unused_variables)]
    async fn on_message(
        &self,
        ctx: &InboundContext,
        msg: &rsms_model::UnifiedMessage,
    ) -> Result<()> {
        Ok(())
    }
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
    /// 握手协商的协议版本字节（如 CMPP 2.0 = `0x20`、3.0 = `0x30`）；未握手或协议无版本概念时为 `None`。
    ///
    /// 业务方据此做版本感知的解码/编码（CMPP 2.0 与 3.0 命令字相同但字段宽度不同）。
    async fn protocol_version(&self) -> Option<u8>;
}

pub async fn run_chain(
    endpoint: Arc<EndpointConfig>,
    conn: Arc<dyn ProtocolConnection>,
    handlers: &[Arc<dyn BusinessHandler>],
    frame: &Frame,
    id_generator: Option<Arc<dyn IdGenerator>>,
) -> Result<()> {
    let ctx = InboundContext { endpoint, conn, id_generator };
    if handlers.is_empty() {
        return NoopBusiness.on_inbound(&ctx, frame).await;
    }
    for h in handlers {
        h.on_inbound(&ctx, frame).await?;
    }
    Ok(())
}