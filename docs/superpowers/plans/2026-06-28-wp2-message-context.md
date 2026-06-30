# WP2 · MessageContext + reply 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 `rsms-business` 引入协议无关的 `MessageContext`，提供一步式回执 `reply(UnifiedMessage)`（自动 `adapter.encode(msg, 当前帧序列) + write_frame`），并把 `id_generator` 从 `Option` 收紧为必有值——为 WP3/WP4 把窄腰接到对接面铺基础设施。

**Architecture:** `MessageContext` 放 `rsms-business`，与现有 `InboundContext` 并存（WP4 才在主循环切换、删旧）。因 `rsms-business` 不依赖 `rsms-connector`，adapter 不能经 `adapter_registry` 取，而是由构造方注入 `&'static dyn ProtocolAdapter`（`rsms-model` 的 trait）；当前帧的回显序列 `frame_sequence: Sequence` 同样构造时注入（connector 侧将以 `adapter.sequence_of(frame)` 传入）。`reply` 内部即 `adapter.encode(&msg, self.frame_sequence)? → conn.write_frame`。

**Tech Stack:** Rust（edition 2024，1.85+）、`async-trait`、`tokio`（测试）、`rsms-model`（`UnifiedMessage`/`Sequence`/`ProtocolAdapter`）、`rsms-core`（`IdGenerator`/`EndpointConfig`）。

## Global Constraints

- 全程思考与输出**用中文**（仅代码语法关键词除外）——`AGENTS.md` 规定。
- 公共 API 必须有 `///` / `//!` 文档注释。
- `cargo clippy --workspace` 必须零告警。
- **构建/测试一律走 WSL**：`wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo ..."`。
- **本地 commit 走 Git Bash**（仓库 `autocrlf=true` + `.gitattributes` 强制 LF；切勿用 WSL git commit）。
- 工作分支：`feature/onboarding-ergonomics`（接 WP1 之后，HEAD = `4fd29b6`）。
- 本 WP 只动 `rsms-business`（新增 `MessageContext` + 测试 + dev-dep），不碰 connector 主循环、不删 `InboundContext`、不改 `BusinessHandler`（留待 WP3/WP4）。

## 范围与决策（请 review 时确认）

- **聚焦**：`MessageContext` 类型 + `reply(UnifiedMessage)` + `id_generator` 非 `Option`。
- **`send`（主动下发、自分配序列）按 YAGNI 推迟**：出站消息走 `MessageSource`/`run_outbound_fetcher`，`ctx.send` 用例罕见，且 SGIP 复合序列自分配不平凡。后续真有需要再加。
- **`channel_key` getter 挪到护栏阶段（spec §4.4）**：它与 `MessageSource::fetch` 参数 `account`→`channel_key` 改名是一组 breaking，放同一 WP 更连贯；WP2 不引入。
- **`id_generator` 非 `Option`**：`MessageContext` 持 `Arc<dyn IdGenerator>`。服务端「鉴权前帧」如何拿到默认生成器，是 WP4 把 `MessageContext` 接入主循环时的问题（届时框架注入默认 `SimpleIdGenerator`）；WP2 只定义类型 + 单元测试。
- **WP2 不产生用户可见行为变化**：它是 WP3/WP4 的基础设施，价值在 WP4 主循环切换后兑现。

## File Structure

- `crates/rsms-business/src/message_context.rs` — **新建**：`MessageContext` 结构 + `new` + `reply` + `#[cfg(test)] mod tests`（mock `ProtocolConnection`/`IdGenerator` + reply 测试）。
- `crates/rsms-business/src/lib.rs` — 加 `mod message_context;` + `pub use message_context::MessageContext;`。
- `crates/rsms-business/Cargo.toml` — 加 `[dev-dependencies] tokio`（reply 为 async，测试需运行时）。

---

### Task 1: MessageContext 类型 + reply

**Files:**
- Create: `crates/rsms-business/src/message_context.rs`
- Modify: `crates/rsms-business/src/lib.rs`（顶部加模块声明与导出）
- Modify: `crates/rsms-business/Cargo.toml`（加 dev-dependency）

**Interfaces:**
- Consumes：`rsms_business::ProtocolConnection`（trait，`lib.rs:84`，含 `write_frame`）、`rsms_business::RateLimiter`（`lib.rs:78`，mock 需返回）、`rsms_core::{EndpointConfig, IdGenerator, Result}`、`rsms_model::{UnifiedMessage, ProtocolAdapter, types::Sequence}`、真实 `rsms_codec_cmpp::adapter::CmppAdapter`（测试用）。
- Produces（WP3/WP4 依赖）：
  ```rust
  pub struct MessageContext {
      pub endpoint: std::sync::Arc<rsms_core::EndpointConfig>,
      pub conn: std::sync::Arc<dyn ProtocolConnection>,
      pub id_generator: std::sync::Arc<dyn rsms_core::IdGenerator>,
      // 私有：adapter: &'static dyn ProtocolAdapter, frame_sequence: Sequence
  }
  impl MessageContext {
      pub fn new(
          endpoint: Arc<EndpointConfig>,
          conn: Arc<dyn ProtocolConnection>,
          id_generator: Arc<dyn IdGenerator>,
          adapter: &'static dyn ProtocolAdapter,
          frame_sequence: Sequence,
      ) -> Self;
      pub async fn reply(&self, msg: UnifiedMessage) -> Result<()>;
  }
  ```

- [ ] **Step 1: 加 dev-dependency**

在 `crates/rsms-business/Cargo.toml` 末尾追加：

```toml
[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt"] }
```

> 若 `tokio` 不在 `[workspace.dependencies]`，改用根 workspace 已用的版本写法（参照 `examples/cmpp_server/Cargo.toml` 的 tokio 依赖行照抄 features）。

- [ ] **Step 2: 写失败测试（先建 mock 脚手架 + reply 测试）**

新建 `crates/rsms-business/src/message_context.rs`，先只写测试模块（结构体尚未实现，编译应失败）：

```rust
//! 协议无关的入站消息上下文：对接方据此一步式回执，无需手接具体 codec。

#[cfg(test)]
mod tests {
    use super::MessageContext;
    use crate::{ProtocolConnection, RateLimiter};
    use async_trait::async_trait;
    use rsms_codec_cmpp::adapter::CmppAdapter;
    use rsms_core::{EndpointConfig, IdGenerator, Result};
    use rsms_model::types::Sequence;
    use rsms_model::{MessageId, UnifiedMessage, UnifiedSubmitResp};
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
        // reply 应等价 adapter.encode(msg, frame_sequence) 再 write_frame：
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
```

- [ ] **Step 3: 运行测试确认失败**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-business message_context"`
Expected: 编译失败 `cannot find ... MessageContext`（结构体尚未定义）。

- [ ] **Step 4: 实现 MessageContext**

在 `crates/rsms-business/src/message_context.rs` 顶部（`#[cfg(test)] mod tests` 之前）插入实现：

```rust
use crate::ProtocolConnection;
use rsms_core::{EndpointConfig, IdGenerator, Result};
use rsms_model::types::Sequence;
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

    /// 一步式回执：把统一消息编码为当前协议字节（回显请求帧序列）并写回对端。
    ///
    /// 等价于手工 `adapter.encode(&msg, sequence_of(frame))? + conn.write_frame`，
    /// 但协议无关——同一份处理器代码在四协议下都生成正确的响应帧。
    pub async fn reply(&self, msg: UnifiedMessage) -> Result<()> {
        let bytes = self.adapter.encode(&msg, self.frame_sequence)?;
        self.conn.write_frame(&bytes).await
    }
}
```

- [ ] **Step 5: 在 lib.rs 声明模块并导出**

`crates/rsms-business/src/lib.rs` 顶部（`use` 之后、`InboundContext` 定义之前）加：

```rust
mod message_context;
pub use message_context::MessageContext;
```

- [ ] **Step 6: 运行测试确认通过**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-business"`
Expected: `reply_encodes_with_frame_sequence_then_writes`、`id_generator_is_accessible_and_non_optional` 均 PASS，既有测试不回归。

- [ ] **Step 7: clippy 确认零告警**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy -p rsms-business"`
Expected: 无 `warning` / `error`。

- [ ] **Step 8: 提交（Git Bash）**

```bash
git add crates/rsms-business/src/message_context.rs crates/rsms-business/src/lib.rs crates/rsms-business/Cargo.toml
git commit -m "feat(business): 新增 MessageContext + reply（协议无关一步回执）

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

- **Spec 覆盖**：实现 spec §3.2 的 `MessageContext` + `reply` + `id_generator` 去 `Option`。`send` 与 `channel_key` 按上文「范围与决策」分别推迟到后续 WP / 护栏阶段——已在计划显式记录，非遗漏。
- **占位符扫描**：无 TBD/TODO；每步给出完整可编译代码与确切命令。Step 1 的 tokio 写法给了「workspace 缺失时照抄 examples」的确切回退，非占位。
- **类型一致**：`MessageContext::new` 参数顺序 `(endpoint, conn, id_generator, adapter, frame_sequence)` 在 Interfaces、实现、测试 `make_ctx` 三处一致；`reply(&self, msg: UnifiedMessage) -> Result<()>` 一致；字段 `endpoint/conn/id_generator` 为 `pub`、`adapter/frame_sequence` 私有，与测试只访问 `id_generator` 吻合。

## 后续衔接

WP2 产出的 `MessageContext` 将由 **WP4** 在 `connection.rs` 主循环接入：把 `unified-shadow` 的 `adapter_for(protocol).decode(frame)` 转正，构造 `MessageContext::new(..., adapter_for(protocol), adapter.sequence_of(frame))` 并驱动 **WP3** 的 `MessageHandler::on_message`。`InboundContext`/`BusinessHandler` 在 WP4 一并退役。
