//! 协议无关的入站消息上下文：对接方据此一步式回执，无需手接具体 codec。

use crate::ProtocolConnection;
use rsms_core::{EndpointConfig, IdGenerator, Result};
use rsms_model::Sequence;
use rsms_model::{ProtocolAdapter, UnifiedMessage};
use std::sync::Arc;

/// 协议无关的入站消息上下文。
///
/// 框架在每条入站业务消息上构造它并传给处理器；对接方用 [`reply`](Self::reply)
/// 一步回执，无需接触具体 codec、无需手剥序列号或拼字节。
pub struct MessageContext {
    /// 当前连接所属端点配置。
    pub endpoint: Arc<EndpointConfig>,
    /// 当前协议连接句柄。
    pub conn: Arc<dyn ProtocolConnection>,
    /// 该账号的序列号 / 消息 ID 生成器（框架保证存在，非 `Option`）。
    pub id_generator: Arc<dyn IdGenerator>,
    /// 当前连接协议对应的 adapter（由框架按协议注入；`rsms-business` 不依赖
    /// connector 的 adapter 登记表，故经构造注入而非内部查表）。
    adapter: &'static dyn ProtocolAdapter,
    /// 当前请求帧的「回显序列」，由框架以 `adapter.sequence_of(frame)` 解出后注入；
    /// `reply` 据此回显请求序列（SGIP 复合序列亦由 [`Sequence`] 承载）。
    frame_sequence: Sequence,
}

impl MessageContext {
    /// 构造上下文。`adapter` 与 `frame_sequence` 由框架按当前连接协议与请求帧注入。
    pub fn new(
        endpoint: Arc<EndpointConfig>,
        conn: Arc<dyn ProtocolConnection>,
        id_generator: Arc<dyn IdGenerator>,
        adapter: &'static dyn ProtocolAdapter,
        frame_sequence: Sequence,
    ) -> Self {
        Self { endpoint, conn, id_generator, adapter, frame_sequence }
    }

    /// 当前请求帧的「回显序列」（框架以 `adapter.sequence_of(frame)` 解出注入）。
    ///
    /// 业务据此把异步响应（如 SGIP 状态报告的 submit_sequence）关联回原请求——
    /// SGIP 的 Submit 无 msg_id、以帧头复合序列为关联键，迁移后是取该序列的唯一途径。
    pub fn frame_sequence(&self) -> Sequence {
        self.frame_sequence
    }

    /// 一步式回执：把统一消息编码为当前协议字节（回显请求帧序列）并写回对端。
    ///
    /// 等价于手工 `adapter.encode_with_version(&msg, sequence_of(frame), conn.protocol_version())? + conn.write_frame`，
    /// 但协议无关——同一份处理器代码在四协议下都生成正确的响应帧。
    /// CMPP V2.0 连接会产出 V2.0 规格应答（如 21B SubmitResp）；V3.0 及其余协议逐字节不变。
    pub async fn reply(&self, msg: UnifiedMessage) -> Result<()> {
        let bytes = self
            .adapter
            .encode_with_version(&msg, self.frame_sequence, self.conn.protocol_version().await)?;
        self.conn.write_frame(&bytes).await
    }
}

#[cfg(test)]
mod tests {
    use super::MessageContext;
    use crate::{ProtocolConnection, RateLimiter};
    use async_trait::async_trait;
    use rsms_codec_cmpp::adapter::CmppAdapter;
    use rsms_core::{EndpointConfig, IdGenerator, Result};
    use rsms_model::Sequence;
    use rsms_model::{MessageId, ProtocolAdapter, UnifiedMessage, UnifiedSubmitResp};
    use std::sync::{Arc, Mutex};

    /// 捕获 write_frame 字节的 mock 连接。
    #[derive(Default)]
    struct MockConn {
        frames: Mutex<Vec<Vec<u8>>>,
    }

    #[async_trait]
    impl ProtocolConnection for MockConn {
        fn id(&self) -> u64 {
            1
        }
        async fn write_frame(&self, data: &[u8]) -> Result<()> {
            self.frames.lock().unwrap().push(data.to_vec());
            Ok(())
        }
        async fn authenticated_account(&self) -> Option<String> {
            Some("acct".to_string())
        }
        async fn rate_limiter(&self) -> Option<Arc<dyn RateLimiter>> {
            None
        }
        async fn protocol_version(&self) -> Option<u8> {
            None
        }
    }

    struct MockIdGen;
    impl IdGenerator for MockIdGen {
        fn next_msg_id(&self) -> u64 {
            1
        }
        fn next_sequence_id(&self) -> u32 {
            1
        }
    }

    fn make_ctx(conn: Arc<MockConn>, seq: Sequence) -> MessageContext {
        MessageContext::new(
            Arc::new(EndpointConfig::new("ep", "127.0.0.1", 7890, 16, 60)),
            conn,
            Arc::new(MockIdGen),
            &CmppAdapter,
            seq,
        )
    }

    #[tokio::test]
    async fn reply_encodes_with_frame_sequence_then_writes() {
        // reply 实走 adapter.encode_with_version(msg, frame_sequence, conn.protocol_version()) 再 write_frame：
        // MockConn.protocol_version() 返回 None，encode_with_version(None) 默认转 encode，
        // 故此处与 adapter.encode(msg, frame_sequence) 逐字节等价。
        // 验证 MessageContext 的编排职责（不验证 codec 字节正确性——那是 adapter 自己的测试）。
        let conn = Arc::new(MockConn::default());
        let ctx = make_ctx(conn.clone(), Sequence::Plain(42));
        let msg = UnifiedMessage::SubmitResp(UnifiedSubmitResp {
            msg_id: MessageId::Binary(vec![1, 2, 3, 4, 5, 6, 7, 8]),
            status: 0,
        });

        ctx.reply(msg.clone()).await.unwrap();

        let written = conn.frames.lock().unwrap().clone();
        assert_eq!(written.len(), 1, "reply 应恰好写出一帧");
        let expected = CmppAdapter.encode(&msg, Sequence::Plain(42)).unwrap();
        assert_eq!(
            written[0], expected,
            "reply 写出的字节应等于 adapter.encode(msg, frame_sequence)"
        );
    }

    #[tokio::test]
    async fn id_generator_is_accessible_and_non_optional() {
        // id_generator 不再是 Option：可直接取用。
        let conn = Arc::new(MockConn::default());
        let ctx = make_ctx(conn, Sequence::Plain(1));
        assert_eq!(ctx.id_generator.next_sequence_id(), 1);
    }
}
