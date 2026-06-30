# WP3 · MessageHandler + RawFrameHandler 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 `rsms-business` 定义重塑后的对接面处理器抽象——协议无关的 `MessageHandler`（主路径，消费 `UnifiedMessage`）与裸帧 `RawFrameHandler`（逃生舱口），并提供与现有 `run_chain` 对称的 `run_message_chain` 派发助手，供 WP4 主循环解码后驱动。

**Architecture:** 两个 trait + 一个派发函数都放 `rsms-business`，消费 WP2 的 `MessageContext`，与现有 `BusinessHandler`/`InboundContext`/`run_chain` **并存**（WP4 才在主循环切换、退役旧路径）。`MessageHandler::on_message` 收 `&UnifiedMessage`（与现有 `BusinessHandler::on_message` 试点签名一致，多处理器共享一条消息无需 clone）。本 WP 不改 connector、不改 builder（builder 接受新 handler 与主循环驱动一并放 WP4）。

**Tech Stack:** Rust（edition 2024，1.85+）、`async-trait`、`tokio`（测试，WP2 已加 dev-dep）、`rsms-model`（`UnifiedMessage`）、`rsms-core`（`Frame`/`RawPdu`/`Result`）。

## Global Constraints

- 全程思考与输出**用中文**（仅代码语法关键词除外）——`AGENTS.md` 规定。
- 公共 API 必须有 `///` / `//!` 文档注释。
- `cargo clippy --workspace` 必须零告警。
- **构建/测试一律走 WSL**：`wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo ..."`。
- **本地 commit 走 Git Bash**（仓库 `autocrlf=true` + `.gitattributes` 强制 LF；切勿用 WSL git commit）。
- 工作分支：`feature/onboarding-ergonomics`（接 WP2 之后，HEAD = `61d5c1a`）。
- 本 WP 只动 `rsms-business`（新增 `message_handler.rs` + `lib.rs` 导出），不碰 connector / builder / 主循环，不动 `BusinessHandler`/`InboundContext`。

## 范围与决策

- **聚焦**：定义 `MessageHandler` + `RawFrameHandler` 两个 trait + `run_message_chain` 派发助手 + 单元测试。
- **不接入主循环、不改 builder**：让 `ServerBuilder`/`ClientBuilder` 接受新 handler、主循环 decode 后驱动 `on_message`、退役 `BusinessHandler` 等，全部归 **WP4**（一并改 connector）。WP3 是 WP4 的前置基础设施，本身不产生用户可见行为变化。
- **`RawFrameHandler` 暂不配 chain 助手**：逃生舱口接入方式（与 `MessageHandler` 的优先级、是否互斥）在 WP4 接入时确定；WP3 只定义 trait。

## File Structure

- `crates/rsms-business/src/message_handler.rs` — **新建**：`MessageHandler` + `RawFrameHandler` trait + `run_message_chain` + `#[cfg(test)] mod tests`。
- `crates/rsms-business/src/lib.rs` — 加 `mod message_handler;` + `pub use message_handler::{run_message_chain, MessageHandler, RawFrameHandler};`。

---

### Task 1: MessageHandler + RawFrameHandler + run_message_chain

**Files:**
- Create: `crates/rsms-business/src/message_handler.rs`
- Modify: `crates/rsms-business/src/lib.rs`

**Interfaces:**
- Consumes：`crate::MessageContext`（WP2）、`crate::{ProtocolConnection, RateLimiter}`（`lib.rs`，测试 mock 用）、`rsms_core::{EndpointConfig, Frame, RawPdu, IdGenerator, Result}`、`rsms_model::{UnifiedMessage, Sequence}`、真实 `rsms_codec_cmpp::adapter::CmppAdapter`（测试构造 `MessageContext` 用）。
- Produces（WP4 依赖）：
  ```rust
  #[async_trait]
  pub trait MessageHandler: Send + Sync {
      fn name(&self) -> &'static str;
      async fn on_message(&self, ctx: &MessageContext, msg: &UnifiedMessage) -> Result<()>;
  }
  #[async_trait]
  pub trait RawFrameHandler: Send + Sync {
      fn name(&self) -> &'static str;
      async fn on_frame(&self, ctx: &MessageContext, frame: &Frame) -> Result<()>;
  }
  pub async fn run_message_chain(
      ctx: &MessageContext,
      msg: &UnifiedMessage,
      handlers: &[Arc<dyn MessageHandler>],
  ) -> Result<()>;
  ```

- [ ] **Step 1: 写失败测试（mock 脚手架 + 派发/调用测试）**

新建 `crates/rsms-business/src/message_handler.rs`，先只写测试模块（trait 与函数尚未定义，编译应失败）：

```rust
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
```

- [ ] **Step 2: 运行测试确认失败**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-business message_handler"`
Expected: 编译失败 `cannot find ... MessageHandler` / `run_message_chain`（尚未定义）。

- [ ] **Step 3: 实现 traits + run_message_chain**

在 `crates/rsms-business/src/message_handler.rs` 顶部（`#[cfg(test)] mod tests` 之前）插入：

```rust
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
```

- [ ] **Step 4: 在 lib.rs 声明模块并导出**

`crates/rsms-business/src/lib.rs` 中 `pub use message_context::MessageContext;` 之后追加一行：

```rust
mod message_handler;
pub use message_handler::{run_message_chain, MessageHandler, RawFrameHandler};
```

- [ ] **Step 5: 运行测试确认通过**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-business"`
Expected: `run_message_chain_invokes_each_handler_in_order`、`run_message_chain_empty_is_ok`、`raw_frame_handler_receives_frame` 均 PASS，既有测试不回归。

- [ ] **Step 6: clippy 确认零告警**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy -p rsms-business"`
Expected: 无 `warning` / `error`。

- [ ] **Step 7: 提交（Git Bash）**

```bash
git add crates/rsms-business/src/message_handler.rs crates/rsms-business/src/lib.rs
git commit -m "feat(business): 新增 MessageHandler/RawFrameHandler trait + run_message_chain

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

- **Spec 覆盖**：实现 spec §3.1（`MessageHandler`）+ §3.6（`RawFrameHandler`），并加对称派发 `run_message_chain`。builder 接受新 handler、主循环驱动、退役旧 trait 属 WP4——已在「范围与决策」显式记录，非遗漏。
- **占位符扫描**：无 TBD/TODO；每步给完整可编译代码与确切命令。
- **类型一致**：`MessageHandler::on_message(&self, ctx: &MessageContext, msg: &UnifiedMessage) -> Result<()>`、`RawFrameHandler::on_frame(&self, ctx: &MessageContext, frame: &Frame) -> Result<()>`、`run_message_chain(ctx, msg, handlers: &[Arc<dyn MessageHandler>])` 在 Interfaces、实现、测试三处签名一致；`on_message` 收 `&UnifiedMessage` 与现有 `BusinessHandler::on_message` 试点一致；`Frame::new` + `RawPdu::from_vec(vec![0u8;20])` 与 `rsms-core` 实际 API 一致（无需 `bytes` 依赖）。

## 后续衔接

WP4 将在 `connection.rs` 主循环：把 `unified-shadow` 的 `adapter_for(protocol).decode(frame)` 转正 → 构造 `MessageContext::new(endpoint, conn, id_gen, adapter_for(protocol), adapter.sequence_of(frame))` → 调 `run_message_chain(&ctx, &msg, &message_handlers)`；并改 `ServerBuilder`/`ClientBuilder` 接受 `MessageHandler`/`RawFrameHandler`，退役 `BusinessHandler`/`InboundContext`/`run_chain`。
