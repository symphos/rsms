//! 对接面入站处理器抽象：协议无关的 [`MessageHandler`]（重塑后的主路径）与
//! 裸帧 [`RawFrameHandler`]（逃生舱口）。WP4 起由主循环驱动；当前与 `BusinessHandler` 并存。

use crate::MessageContext;
use async_trait::async_trait;
use rsms_core::{Frame, Result};
use rsms_model::UnifiedMessage;
use std::sync::Arc;

/// 协议无关的业务处理器（重塑后的主路径）。
///
/// 框架自动把入站帧解码为 [`UnifiedMessage`] 后调用 `on_message`；对接方面向统一
/// 模型编程、用 [`MessageContext::reply`](crate::MessageContext::reply) 回执，无需接触具体 codec。
#[async_trait]
pub trait MessageHandler: Send + Sync {
    /// 处理器唯一名称（日志/调试用）。
    fn name(&self) -> &'static str;

    /// 处理一条已解码的统一消息。
    ///
    /// 返回 `Err` 时框架中断处理链并记录错误（连接不强制断开）。
    async fn on_message(&self, ctx: &MessageContext, msg: &UnifiedMessage) -> Result<()>;
}

/// 裸帧逃生舱口：极少数需直接处理协议字节的高级场景使用。
/// 绝大多数对接只用 [`MessageHandler`]。
#[async_trait]
pub trait RawFrameHandler: Send + Sync {
    /// 处理器唯一名称（日志/调试用）。
    fn name(&self) -> &'static str;

    /// 处理一条原始入站帧（含协议头）。
    async fn on_frame(&self, ctx: &MessageContext, frame: &Frame) -> Result<()>;
}

/// 顺序驱动一组 [`MessageHandler`]：对同一条消息依次调用各处理器，
/// 任一返回 `Err` 即中断并上抛；空链为 no-op。
///
/// 与面向 `BusinessHandler` 的 `run_chain` 对称，供 WP4 主循环在解码后调用。
pub async fn run_message_chain(
    ctx: &MessageContext,
    msg: &UnifiedMessage,
    handlers: &[Arc<dyn MessageHandler>],
) -> Result<()> {
    for h in handlers {
        h.on_message(ctx, msg).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{run_message_chain, MessageHandler, RawFrameHandler};
    use crate::{MessageContext, ProtocolConnection, RateLimiter};
    use async_trait::async_trait;
    use rsms_codec_cmpp::adapter::CmppAdapter;
    use rsms_core::{EndpointConfig, Frame, IdGenerator, RawPdu, Result};
    use rsms_model::{Sequence, UnifiedMessage};
    use std::sync::{Arc, Mutex};

    struct NoopConn;
    #[async_trait]
    impl ProtocolConnection for NoopConn {
        fn id(&self) -> u64 {
            1
        }
        async fn write_frame(&self, _data: &[u8]) -> Result<()> {
            Ok(())
        }
        async fn authenticated_account(&self) -> Option<String> {
            None
        }
        async fn rate_limiter(&self) -> Option<Arc<dyn RateLimiter>> {
            None
        }
        async fn protocol_version(&self) -> Option<u8> {
            None
        }
    }

    struct OneIdGen;
    impl IdGenerator for OneIdGen {
        fn next_msg_id(&self) -> u64 {
            1
        }
        fn next_sequence_id(&self) -> u32 {
            1
        }
    }

    fn make_ctx() -> MessageContext {
        MessageContext::new(
            Arc::new(EndpointConfig::new("ep", "127.0.0.1", 7890, 16, 60)),
            Arc::new(NoopConn),
            Arc::new(OneIdGen),
            &CmppAdapter,
            Sequence::Plain(1),
        )
    }

    #[derive(Default)]
    struct RecordingMessageHandler {
        seen: Mutex<Vec<String>>,
    }
    #[async_trait]
    impl MessageHandler for RecordingMessageHandler {
        fn name(&self) -> &'static str {
            "rec-msg"
        }
        async fn on_message(&self, _ctx: &MessageContext, msg: &UnifiedMessage) -> Result<()> {
            self.seen.lock().unwrap().push(format!("{msg:?}"));
            Ok(())
        }
    }

    #[derive(Default)]
    struct RecordingRawHandler {
        count: Mutex<u32>,
    }
    #[async_trait]
    impl RawFrameHandler for RecordingRawHandler {
        fn name(&self) -> &'static str {
            "rec-raw"
        }
        async fn on_frame(&self, _ctx: &MessageContext, _frame: &Frame) -> Result<()> {
            *self.count.lock().unwrap() += 1;
            Ok(())
        }
    }

    #[tokio::test]
    async fn run_message_chain_invokes_each_handler_in_order() {
        let ctx = make_ctx();
        let h1 = Arc::new(RecordingMessageHandler::default());
        let h2 = Arc::new(RecordingMessageHandler::default());
        let handlers: Vec<Arc<dyn MessageHandler>> = vec![h1.clone(), h2.clone()];
        let msg = UnifiedMessage::Ping;

        run_message_chain(&ctx, &msg, &handlers).await.unwrap();

        assert_eq!(h1.seen.lock().unwrap().len(), 1, "第一个 handler 应被调用一次");
        assert_eq!(h2.seen.lock().unwrap().len(), 1, "第二个 handler 应被调用一次");
        assert!(
            h1.seen.lock().unwrap()[0].contains("Ping"),
            "handler 应收到传入的那条消息"
        );
    }

    #[tokio::test]
    async fn run_message_chain_empty_is_ok() {
        let ctx = make_ctx();
        let handlers: Vec<Arc<dyn MessageHandler>> = vec![];
        run_message_chain(&ctx, &UnifiedMessage::Ping, &handlers)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn raw_frame_handler_receives_frame() {
        let ctx = make_ctx();
        let h = RecordingRawHandler::default();
        let frame = Frame::new(0x8000_0005, 1, RawPdu::from_vec(vec![0u8; 20]));
        h.on_frame(&ctx, &frame).await.unwrap();
        assert_eq!(*h.count.lock().unwrap(), 1);
    }
}
