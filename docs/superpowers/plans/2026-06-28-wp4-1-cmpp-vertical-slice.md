# WP4-1（CMPP 垂直切片）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: 用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐 task 执行本计划。步骤用 checkbox（`- [ ]`）跟踪。
> **语言要求**：本仓库全程思考与输出用中文（仅代码英文关键词除外，见 AGENTS.md / CLAUDE.md）。

**Goal:** 把窄腰统一处理链（`MessageHandler` + `MessageContext::reply`）接入 CMPP 的服务端主循环与客户端读循环，打通「解码→统一消息→on_message→一步回执」全链路并以零丢失压测验收，作为四协议横向铺开前的模式样板。

**Architecture:** 用「临时并存桥」做垂直切片——`ServerBuilder`/`run_connection` 新增 `message_handlers` 字段、`ClientBuilder` 新增 `with_message_handler`，在 `HandleResult::Continue` 后按「新 handler 列表是否非空」择路：非空走 `adapter.decode → MessageContext → run_message_chain`，否则保留旧 `BusinessHandler`/`ClientHandler` 路径。CMPP 的 example 与 test 迁到新路径验证，SMGP/SMPP/SGIP 暂留旧路径。并存桥与旧 trait 在 WP4-3 收尾删除。

**Tech Stack:** Rust edition 2024 / rustc ≥ 1.85；tokio async；`rsms-business`（`MessageHandler`/`MessageContext`/`run_message_chain`，WP2-3 已落地）；`rsms-model`（`UnifiedMessage`/`ProtocolAdapter`）；`rsms-connector`（主循环/builder）；`rsms-codec-cmpp`（`CmppAdapter`）。

## Global Constraints

- **允许 breaking、无需向后兼容**：项目 `0.0.1` 未发布、无外部接入方（用户 2026-06-28 拍板）。
- **clippy 零告警**：`cargo clippy --workspace` 必须 warning-free（CONTRIBUTING 要求）。
- **公共 API 必须有 doc 注释**（`///` 或 `//!`）。
- **cargo 一律走 WSL**，命令前缀 `RUSTFLAGS='--cap-lints allow'`（Windows 上 rustc 1.94 对 async Send 诊断会 ICE，`--cap-lints allow` 规避）。**commit 走 Git Bash**（远程 SSH 须走 WSL；见 [[git-remote-via-wsl]]）。
- **压测必须 WARN 日志**：`EndpointConfig` 已配 `.with_log_level(WARN)`，禁止下调（INFO 下 300s 压测 240 万行日志、吞吐从 ~12500 崩到 ~2700 TPS）。
- **压测验收线 = 零丢失**：sent 仅在 `send_request` 成功后计数。
- **不触碰协议层 `handle_frame` 分支**（含握手/心跳/关闭）：WP4-1 仅改 `Continue` 之后的业务链路。

---

## 已敲定设计决策（D1/D2/D3，2026-06-28）

> 三者均采用 `docs/superpowers/plans/2026-06-28-wp4-design-notes.md` §4 的推荐项，并明确 WP4-1 的落地边界。

### D1 = (a)，但 **WP4-1 不实现版本感知**
- **结论**：版本感知内化方案选 (a)——WP4-3 给 `ProtocolAdapter` 加 `decode_with_version(&self, frame, version: Option<u8>)`（默认转 `decode`），框架在驱动层按 `conn.protocol_version()` 调用，仅 CMPP override。
- **WP4-1 边界**：服务端/客户端并存桥一律用基础 `adapter.decode`（等价 CMPP V3.0 线路布局）。
- **代价与对策**：新路径**入站 Submit 解码 + SubmitResp 回执**在 WP4-1 期间退化为 V3.0；CMPP V2.0 专项的 `tests/cmpp/cmpp20_test.rs`（test target `cmpp20`）在 WP4-1 **保留旧 `BusinessHandler` 路径、不迁移**（并存桥保护），待 WP4-3 落地 D1a 后再迁。出站 MO/Report 的版本感知不受影响（仍由 example 自己的 `FileMessageSource` 用 `CmppAdapter.encode_with_version` 编码）。

### D2 = (a)：客户端并存桥用 `with_message_handler`
- **结论**：`ClientBuilder` 加 `with_message_handler(Arc<dyn MessageHandler>)`，与构造时的 `client_handler` 二选一；`run_client_read_loop` 按 `message_handler` 是否非空择路。
- **配套**：`ClientBuilder::new` 第二参仍强制 `client_handler`（SMGP/SMPP/SGIP client example 共用、不可改签名），故框架内置 `NoopClientHandler` 供迁移后的 CMPP client 占位。WP4-3 删 `client_handler` 时一并清理。

### D3 = (b)，**WP4-1 不实现心跳收归**
- **结论**：心跳 resp 收归选 (b)——WP4-3 在 decode 驱动层对 `UnifiedMessage::Ping` 由框架自动 `reply(PingResp)`、不进 `on_message`。
- **WP4-1 边界**：不触碰任何协议 `handle_frame` 的心跳分支。CMPP 服务端心跳现状（`handle_frame` 对 ActiveTest 落 `_ => Stop`、不进业务链）保持不变；迁移后行为与现状**逐帧一致**。

---

## 文件结构与职责

| 文件 | 改动 | 职责 |
|---|---|---|
| `crates/rsms-connector/src/server.rs` | 改 | `ServerBuilder`/`BoundServer` 增 `message_handlers` 字段与 setter，`serve()`/`run()` 透传 |
| `crates/rsms-connector/src/connection.rs` | 改 | `run_connection` 增 `message_handlers` 参数；`Continue` 分支按列表非空择路（新增窄腰路径 + 连接级 fallback id_gen） |
| `crates/rsms-connector/src/client.rs` | 改 | `ClientConnection` 增 `id_generator` 字段 + `impl rsms_business::ProtocolConnection`；`ClientBuilder` 增 `with_message_handler`；`run_client_read_loop` 增参数择路；新增 `NoopClientHandler` |
| `examples/cmpp_server/src/main.rs` | 改 | `CmppBusinessHandler` 迁 `MessageHandler`，回执用 `ctx.reply` |
| `examples/cmpp_client/src/main.rs` | 改 | `CmppClientHandler` 迁 `MessageHandler`，`DeliverResp` 用 `ctx.reply` |
| `tests/cmpp/cmpp_test.rs` | 改 | 本地 `ServerHandler`/`ClientState` 迁新 trait，本地 `start_test_server` 用 `message_handlers` |
| `tests/cmpp/stress_test.rs`、`tests/cmpp/multi_account_stress_test.rs` | 改 | 本地 handler 迁新 trait，零丢失复验 |

> 不触碰 `tests/common/src/server.rs`（被其他协议共用）；CMPP 压测/集成测试均自带本地 handler 与本地 `start_test_server`。

---

## Task 1：服务端并存桥（`ServerBuilder` / `run_connection` 接 `message_handlers`）

**Files:**
- Modify: `crates/rsms-connector/src/server.rs`（`ServerBuilder` 41-50/`new` 52-64/setter 66-76/`serve` 110-134/`BoundServer` struct 16-29/`run` 180-200）
- Modify: `crates/rsms-connector/src/connection.rs`（import 行 2；`run_connection` 签名 329-340；循环外 357 附近；`Continue` 分支 433-438）
- Test: 复用 `tests/cmpp/cmpp_test.rs`（`cmpp-integration`）验旧路径零回归 + 新增 `ServerBuilder` setter 单测

**Interfaces:**
- Consumes（WP2-3 既有，签名核对过）：
  - `rsms_business::MessageHandler`（`fn name(&self)->&'static str` + `async fn on_message(&self, &MessageContext, &UnifiedMessage)->Result<()>`）
  - `rsms_business::run_message_chain(&MessageContext, &UnifiedMessage, &[Arc<dyn MessageHandler>]) -> Result<()>`
  - `rsms_business::MessageContext::new(endpoint: Arc<EndpointConfig>, conn: Arc<dyn rsms_business::ProtocolConnection>, id_generator: Arc<dyn IdGenerator>, adapter: &'static dyn ProtocolAdapter, frame_sequence: Sequence) -> MessageContext`
  - `crate::adapter_registry::adapter_for(Protocol) -> &'static dyn ProtocolAdapter`
  - `ProtocolAdapter::decode(&self,&Frame)->Result<UnifiedMessage>`、`::sequence_of(&self,&Frame)->Sequence`
  - `crate::SimpleIdGenerator::new() -> SimpleIdGenerator`（impl `IdGenerator`）
- Produces（后续 task 依赖）：
  - `ServerBuilder::message_handler(self, Arc<dyn MessageHandler>) -> Self`
  - `ServerBuilder::message_handlers(self, Vec<Arc<dyn MessageHandler>>) -> Self`
  - `run_connection(read, conn, handlers, message_handlers, account_pool, account_config_provider, auth_handler, protocol, event_handler, metrics, shutdown)`（在 `handlers` 后插入 `message_handlers: Vec<Arc<dyn MessageHandler>>`）

- [ ] **Step 1：`connection.rs` 扩 import**

把第 2 行：
```rust
use rsms_business::{run_chain, BusinessHandler, ProtocolConnection as BusinessProtocolConnection, RateLimiter};
```
改为：
```rust
use rsms_business::{
    run_chain, run_message_chain, BusinessHandler, MessageContext, MessageHandler,
    ProtocolConnection as BusinessProtocolConnection, RateLimiter,
};
```

- [ ] **Step 2：`run_connection` 加 `message_handlers` 参数**

把签名（329-340）的 `handlers` 行之后插入一行：
```rust
pub async fn run_connection(
    read: OwnedReadHalf,
    conn: Arc<Connection>,
    handlers: Vec<Arc<dyn BusinessHandler>>,
    message_handlers: Vec<Arc<dyn MessageHandler>>,
    account_pool: Option<Arc<AccountPool>>,
    // …其余参数不变…
```

- [ ] **Step 3：循环外建连接级 fallback id_gen**

在读循环 `loop {` 之前（约 365 行，紧接 `let poll = …;` 之后）插入：
```rust
    // 窄腰路径要求 MessageContext.id_generator 非 Option；account pool 在 run_chain 之后才注册，
    // 首帧时连接可能尚未入池。为此每连接持一个 fallback 生成器，未入池时用它，保证连接内计数连续。
    let fallback_id_gen: Arc<dyn rsms_core::IdGenerator> = Arc::new(crate::SimpleIdGenerator::new());
```

- [ ] **Step 4：`Continue` 分支按列表非空择路**

把 433-438 的整块：
```rust
            if action == HandleResult::Continue {
                let id_gen = conn_arc.account_connections().await.map(|ac| ac.id_generator().clone());
                if let Err(e) = run_chain(conn.config.clone(), conn_arc.clone() as Arc<dyn rsms_business::ProtocolConnection>, &handlers, &frame, id_gen).await {
                    error!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "business: {}", e);
                }
            }
```
替换为：
```rust
            if action == HandleResult::Continue {
                if message_handlers.is_empty() {
                    // 旧路径（BusinessHandler）：行为保持不变。
                    let id_gen = conn_arc.account_connections().await.map(|ac| ac.id_generator().clone());
                    if let Err(e) = run_chain(conn.config.clone(), conn_arc.clone() as Arc<dyn rsms_business::ProtocolConnection>, &handlers, &frame, id_gen).await {
                        error!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "business: {}", e);
                    }
                } else {
                    // 窄腰主路径：按协议解码为统一消息，构造 MessageContext，顺序驱动 MessageHandler 链。
                    use rsms_model::ProtocolAdapter as _;
                    let adapter = crate::adapter_registry::adapter_for(protocol);
                    match adapter.decode(&frame) {
                        Ok(unified) => {
                            let id_gen = conn_arc
                                .account_connections()
                                .await
                                .map(|ac| ac.id_generator().clone())
                                .unwrap_or_else(|| fallback_id_gen.clone());
                            let ctx = MessageContext::new(
                                conn.config.clone(),
                                conn_arc.clone() as Arc<dyn rsms_business::ProtocolConnection>,
                                id_gen,
                                adapter,
                                adapter.sequence_of(&frame),
                            );
                            if let Err(e) = run_message_chain(&ctx, &unified, &message_handlers).await {
                                error!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "message business: {}", e);
                            }
                        }
                        // 解码失败仅告警、跳过该帧（对齐 example 现状，不因业务消息解码失败断连）。
                        Err(e) => {
                            tracing::warn!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "统一模型解码失败（跳过该帧）: {e}");
                        }
                    }
                }
            }
```

- [ ] **Step 5：`server.rs` 扩 import**

把第 7 行：
```rust
use rsms_business::BusinessHandler;
```
改为：
```rust
use rsms_business::{BusinessHandler, MessageHandler};
```

- [ ] **Step 6：`BoundServer` 与 `ServerBuilder` 加字段**

`BoundServer` struct（16-29）在 `handlers` 行后插入：
```rust
    message_handlers: Vec<Arc<dyn MessageHandler>>,
```
`ServerBuilder` struct（41-50）在 `handlers` 行后插入同一行；`ServerBuilder::new`（52-64）在 `handlers: Vec::new(),` 后插入：
```rust
            message_handlers: Vec::new(),
```

- [ ] **Step 7：加 `ServerBuilder` setter**

在 `handlers()` 方法（73-76）之后插入：
```rust
    /// 追加一个窄腰统一消息处理器（重塑后主路径）。设置任意一个即让本连接走
    /// 「解码→UnifiedMessage→on_message」链，旧 `BusinessHandler` 列表被忽略。
    pub fn message_handler(mut self, handler: Arc<dyn MessageHandler>) -> Self {
        self.message_handlers.push(handler);
        self
    }

    /// 一次性设置窄腰处理器列表（覆盖已有）。
    pub fn message_handlers(mut self, handlers: Vec<Arc<dyn MessageHandler>>) -> Self {
        self.message_handlers = handlers;
        self
    }
```

- [ ] **Step 8：`serve()` 透传**

`serve()` 构造 `BoundServer`（120-133）在 `handlers: self.handlers,` 后插入：
```rust
            message_handlers: self.message_handlers,
```

- [ ] **Step 9：`run()` 透传给 `run_connection`**

`run()` spawn 前（181 附近 `let h = self.handlers.clone();` 后）插入：
```rust
            let mh = self.message_handlers.clone();
```
并把 198 行 `run_connection(read, Arc::clone(&conn), h, Some(account_pool2), …)` 改为在 `h` 后插入 `mh`：
```rust
                run_connection(read, Arc::clone(&conn), h, mh, Some(account_pool2), account_config_provider, auth_handler_clone, protocol, event_handler_clone, metrics_clone, shutdown_clone).await;
```

- [ ] **Step 10：写 `ServerBuilder` setter 单测**

在 `crates/rsms-connector/src/server.rs` 末尾新增（若已有 `#[cfg(test)] mod tests` 则并入）：
```rust
#[cfg(test)]
mod wp4_bridge_tests {
    use super::*;
    use async_trait::async_trait;
    use rsms_business::{MessageContext, MessageHandler};
    use rsms_model::UnifiedMessage;

    struct DummyMh;
    #[async_trait]
    impl MessageHandler for DummyMh {
        fn name(&self) -> &'static str { "dummy" }
        async fn on_message(&self, _ctx: &MessageContext, _msg: &UnifiedMessage) -> rsms_core::Result<()> { Ok(()) }
    }

    #[test]
    fn message_handlers_setter_stores_handlers() {
        let cfg = Arc::new(rsms_core::EndpointConfig::new("ep", "127.0.0.1", 0, 8, 60));
        let b = ServerBuilder::new(cfg)
            .message_handler(Arc::new(DummyMh))
            .message_handler(Arc::new(DummyMh));
        assert_eq!(b.message_handlers.len(), 2, "message_handler 应累加进列表");
    }
}
```

- [ ] **Step 11：编译 + 单测 + 旧路径零回归**

Run（WSL）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo build --workspace 2>&1 | tail -20"
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-connector --lib 2>&1 | grep -E 'test result|error' | tail -10"
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test cmpp-integration 2>&1 | grep -E 'test result|error' | tail -10"
```
Expected：workspace 编译通过；`message_handlers_setter_stores_handlers` PASS；`cmpp-integration` 全绿（仍走旧 `BusinessHandler` 路径，证明并存桥未破坏旧路径）。

- [ ] **Step 12：clippy + commit**

Run：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy -p rsms-connector 2>&1 | grep -E 'warning|error' | tail -10"
```
Expected：无 `warning`/`error` 行。然后（Git Bash）：
```bash
git add crates/rsms-connector/src/server.rs crates/rsms-connector/src/connection.rs
git commit -m "feat(wp4-1): 服务端主循环并存桥——ServerBuilder/run_connection 支持 message_handlers"
```

---

## Task 2：客户端并存桥（`ClientConnection` 接 `rsms_business::ProtocolConnection` + `ClientBuilder::with_message_handler`）

**Files:**
- Modify: `crates/rsms-connector/src/client.rs`（import；`ClientConnection` struct 203-228/`new` 230-259；`impl ... ProtocolConnection` 块 565 后新增；`ClientHandler` trait 573-577 后新增 `NoopClientHandler`；`ClientBuilder` struct 587-596/`new` 599-614/setter 643 后；`connect` 解构 646-656 与 spawn 685-692、751-759；`run_client_read_loop` 签名 783-789 与分发 883-913）
- Test: 新增最小客户端 `MessageHandler` 单测（验 `with_message_handler` 存值 + `NoopClientHandler` 可构造）

**Interfaces:**
- Consumes：Task 1 的 `MessageContext`/`run_message_chain`（此处仅用 `MessageContext::new` + `on_message`）；`rsms_business::ProtocolConnection`（5 方法：`id`/`write_frame`/`authenticated_account`/`rate_limiter`/`protocol_version`）；`rsms_business::RateLimiter`。
- Produces：
  - `ClientConnection.id_generator: Arc<dyn IdGenerator>`（pub(crate) 字段，供 `run_client_read_loop` 读）
  - `impl rsms_business::ProtocolConnection for ClientConnection`
  - `pub struct NoopClientHandler;`（impl `ClientHandler`，name=`"noop-client"`、`on_inbound` 返回 `Ok(())`）
  - `ClientBuilder::with_message_handler(self, Arc<dyn MessageHandler>) -> Self`

- [ ] **Step 1：扩 import**

`client.rs` 顶部 import 区加（与既有 `use rsms_business::...` 合并，注意别名避免与 `crate::protocol::ProtocolConnection` 冲突）：
```rust
use rsms_business::{
    MessageContext, MessageHandler, ProtocolConnection as BusinessProtocolConnection,
    RateLimiter as BusinessRateLimiter,
};
```

- [ ] **Step 2：`ClientConnection` 加 `id_generator` 字段**

struct（203-228）`tasks: Mutex<Vec<JoinHandle<()>>>,` 后插入：
```rust
    /// 该连接的 ID/序列号生成器。窄腰 `MessageContext` 要求非 Option；客户端无连接池语义，自持一个。
    pub(crate) id_generator: Arc<dyn rsms_core::IdGenerator>,
```
`new`（238-258 的 `Arc::new(Self { … })`）在 `tasks: Mutex::new(Vec::new()),` 后插入：
```rust
            id_generator: Arc::new(crate::SimpleIdGenerator::new()),
```

- [ ] **Step 3：为 `ClientConnection` 补 `impl rsms_business::ProtocolConnection`**

在 `crate::protocol::ProtocolConnection for ClientConnection` 块结束（565 行 `}`）之后插入：
```rust
/// 窄腰 `MessageContext` 要求 `rsms_business::ProtocolConnection`；客户端连接据此参与统一处理链。
/// 与上方 `crate::protocol::ProtocolConnection` 委托同一份连接状态（客户端无限流，故 `rate_limiter` 恒 `None`）。
#[async_trait]
impl BusinessProtocolConnection for ClientConnection {
    fn id(&self) -> u64 {
        self.id
    }

    async fn write_frame(&self, data: &[u8]) -> Result<()> {
        ClientConnection::write_frame(self, data).await
    }

    async fn authenticated_account(&self) -> Option<String> {
        Some(self.endpoint.id.clone())
    }

    async fn rate_limiter(&self) -> Option<Arc<dyn BusinessRateLimiter>> {
        None
    }

    async fn protocol_version(&self) -> Option<u8> {
        self.ctx.lock().await.protocol_version()
    }
}
```

- [ ] **Step 4：新增 `NoopClientHandler`**

在 `ClientHandler` trait（573-577）之后插入：
```rust
/// 空客户端处理器：迁到 [`ClientBuilder::with_message_handler`] 后，`ClientBuilder::new` 仍强制
/// 传入一个 `ClientHandler`（其他协议 example 共用该签名），用它占位。WP4-3 删 `client_handler` 时移除。
pub struct NoopClientHandler;

#[async_trait]
impl ClientHandler for NoopClientHandler {
    fn name(&self) -> &'static str {
        "noop-client"
    }
    async fn on_inbound(&self, _ctx: &ClientContext<'_>, _frame: &Frame) -> Result<()> {
        Ok(())
    }
}
```
并确保 `NoopClientHandler` 经 `crates/rsms-connector/src/lib.rs` 对外导出（在导出 `ClientBuilder`/`ClientHandler` 的同一 `pub use` 分组追加 `NoopClientHandler`）。

- [ ] **Step 5：`ClientBuilder` 加 `message_handler` 字段 + setter**

struct（587-596）`client_handler: Arc<dyn ClientHandler>,` 后插入：
```rust
    message_handler: Option<Arc<dyn MessageHandler>>,
```
`new`（604-613 的 `Self { … }`）在 `client_handler,` 后插入：
```rust
            message_handler: None,
```
在 `event_handler()` setter（640-643）之后插入：
```rust
    /// 注入窄腰统一消息处理器（重塑后主路径）。设置后读循环把入站帧解码为 `UnifiedMessage`
    /// 并调 `on_message`；与构造时的 `client_handler`（裸帧旧路径）二选一，设置它即覆盖旧路径。
    pub fn with_message_handler(mut self, handler: Arc<dyn MessageHandler>) -> Self {
        self.message_handler = Some(handler);
        self
    }
```

- [ ] **Step 6：`connect()` 解构并透传**

`connect()`（647-656）的 `let ClientBuilder { … } = self;` 解构里 `client_handler,` 后加 `message_handler,`。然后两处 spawn（685-692、751-759）在调用 `run_client_read_loop(conn_clone, client_handler_clone, …)` 前各 clone 一份：
```rust
        let message_handler_clone = message_handler.clone();
```
并把两处 `run_client_read_loop(conn_clone, client_handler_clone, …)` 改为在 `client_handler_clone` 后插入 `message_handler_clone`（与 Step 7 的新签名一致）。

> 注：`message_handler` 在两个 spawn 间被各 clone 一次；若闭包按 move 捕获导致借用问题，在每个 spawn 前就近 `let message_handler_clone = message_handler.clone();`。

- [ ] **Step 7：`run_client_read_loop` 加参数并择路**

签名（783-789）在 `client_handler` 后插入参数：
```rust
async fn run_client_read_loop(
    conn: Arc<ClientConnection>,
    client_handler: Arc<dyn ClientHandler>,
    message_handler: Option<Arc<dyn MessageHandler>>,
    decoder: Arc<tokio::sync::Mutex<Box<dyn FrameDecoder>>>,
    event_handler: Option<Arc<dyn ClientEventHandler>>,
    metrics: Arc<dyn Metrics>,
) {
```
把分发块（883-913，含 `let ctx = ClientContext {…}`、INFO 日志、shadow decode、`client_handler.on_inbound`）替换为——**保留 INFO 日志与 shadow decode 在外层共用**，仅按 `message_handler` 择路构造 ctx 与调用：
```rust
            if conn.endpoint.log_level.is_none_or(|max| tracing::Level::INFO <= max) {
                tracing::info!(
                    conn_id = conn.id,
                    remote_ip = %conn.remote_ip(),
                    remote_port = conn.remote_port(),
                    len = frame.len(),
                    cmd_id = frame.command_id,
                    "received frame"
                );
            }

            // 影子比对（客户端收包方向）：unified-shadow 开启时对每帧统一解码并打日志，只观测不接管。
            #[cfg(feature = "unified-shadow")]
            {
                use rsms_model::ProtocolAdapter as _;
                let protocol = conn.endpoint.protocol;
                match crate::adapter_registry::adapter_for(protocol).decode(&frame) {
                    Ok(unified) => tracing::debug!(conn_id = conn.id, proto = protocol.as_str(), cmd_id = frame.command_id, ?unified, "shadow decode ok"),
                    Err(e) => tracing::warn!(conn_id = conn.id, proto = protocol.as_str(), cmd_id = frame.command_id, "shadow decode err: {e}"),
                }
            }

            if let Some(mh) = &message_handler {
                // 窄腰主路径：解码为统一消息，构造 MessageContext，调 on_message。
                use rsms_model::ProtocolAdapter as _;
                let adapter = crate::adapter_registry::adapter_for(conn.endpoint.protocol);
                match adapter.decode(&frame) {
                    Ok(unified) => {
                        let ctx = MessageContext::new(
                            conn.endpoint.clone(),
                            conn.clone() as Arc<dyn BusinessProtocolConnection>,
                            conn.id_generator.clone(),
                            adapter,
                            adapter.sequence_of(&frame),
                        );
                        if let Err(e) = mh.on_message(&ctx, &unified).await {
                            error!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "message handler error: {}", e);
                        }
                    }
                    Err(e) => tracing::warn!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "统一模型解码失败（跳过该帧）: {e}"),
                }
            } else {
                let ctx = ClientContext {
                    endpoint: &conn.endpoint,
                    conn: &conn,
                };
                if let Err(e) = client_handler.on_inbound(&ctx, &frame).await {
                    error!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "client handler error: {}", e);
                }
            }
```

- [ ] **Step 8：写客户端并存桥单测**

在 `client.rs` 测试模块（无则新增 `#[cfg(test)] mod wp4_client_tests`）加：
```rust
#[cfg(test)]
mod wp4_client_tests {
    use super::*;
    use async_trait::async_trait;
    use rsms_business::{MessageContext, MessageHandler};
    use rsms_model::UnifiedMessage;

    struct DummyMh;
    #[async_trait]
    impl MessageHandler for DummyMh {
        fn name(&self) -> &'static str { "dummy" }
        async fn on_message(&self, _ctx: &MessageContext, _msg: &UnifiedMessage) -> rsms_core::Result<()> { Ok(()) }
    }

    #[test]
    fn with_message_handler_stores_handler() {
        let ep = Arc::new(rsms_core::EndpointConfig::new("ep", "127.0.0.1", 7890, 16, 60));
        let b = ClientBuilder::new(ep, Arc::new(NoopClientHandler), crate::CmppDecoder)
            .with_message_handler(Arc::new(DummyMh));
        assert!(b.message_handler.is_some(), "with_message_handler 应存入处理器");
    }
}
```

- [ ] **Step 9：编译 + 单测 + 既有客户端测试零回归**

Run（WSL）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo build --workspace 2>&1 | tail -20"
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-connector --lib 2>&1 | grep -E 'test result|error' | tail -10"
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test cmpp-integration 2>&1 | grep -E 'test result|error' | tail -10"
```
Expected：编译通过；`with_message_handler_stores_handler` PASS；`cmpp-integration` 仍全绿（客户端走旧路径）。

- [ ] **Step 10：clippy + commit**

Run：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy -p rsms-connector 2>&1 | grep -E 'warning|error' | tail -10"
```
Expected：无告警。然后（Git Bash）：
```bash
git add crates/rsms-connector/src/client.rs crates/rsms-connector/src/lib.rs
git commit -m "feat(wp4-1): 客户端并存桥——ClientConnection 接 rsms_business::ProtocolConnection + ClientBuilder::with_message_handler"
```

---

## Task 3：CMPP server example 迁移到 `MessageHandler` + `ctx.reply`

**Files:**
- Modify: `examples/cmpp_server/src/main.rs`（import 27-51；`impl BusinessHandler for CmppBusinessHandler` 364-398；`handle_submit` 400-495；`main` 656-660）

**Interfaces:**
- Consumes：Task 1 的 `ServerBuilder::message_handlers` + 服务端窄腰路径；`MessageContext::reply(UnifiedMessage) -> Result<()>`。
- Produces：无（example 终端）。

> **行为变化（D1）**：迁移后入站 Submit 解码与 SubmitResp 回执走 V3.0；出站 MO/Report 仍由 `FileMessageSource` 用 `encode_with_version` 按版本编码（不变）。example 默认对 V3.0 客户端，OK。

- [ ] **Step 1：改 import**

把 28-29 行：
```rust
use rsms_business::BusinessHandler;
use rsms_business::InboundContext;
```
改为：
```rust
use rsms_business::{MessageContext, MessageHandler};
```
删除 33 行 `use rsms_codec_cmpp::CmppVersion;` **当且仅当** `build_deliver_report`/`build_deliver_mo_with_udh` 之外不再引用 `CmppVersion`——这两个函数仍用 `CmppVersion::V20`，故 **保留** 该 import。`Frame` 若仅 `on_inbound` 用到则从 39-41 的 `rsms_core` import 中删除（编译器报未用即删）。

- [ ] **Step 2：`impl BusinessHandler` 换为 `impl MessageHandler`**

把 364-398 整块替换为：
```rust
#[async_trait]
impl MessageHandler for CmppBusinessHandler {
    fn name(&self) -> &'static str {
        "cmpp-business"
    }

    async fn on_message(&self, ctx: &MessageContext, msg: &UnifiedMessage) -> Result<()> {
        // 框架已按协议解码为统一消息（WP4-1 走 V3.0 基础解码），业务直接按枚举分支处理。
        match msg {
            UnifiedMessage::Submit(submit) => {
                self.handle_submit(ctx, submit).await?;
            }
            UnifiedMessage::Ping => {
                tracing::debug!(conn_id = ctx.conn.id(), "收到 ActiveTest（心跳）");
            }
            _ => {}
        }
        Ok(())
    }
}
```

- [ ] **Step 3：`handle_submit` 改签名 + 用 `ctx.reply` 回执**

把 400-434 行（`impl CmppBusinessHandler { async fn handle_submit(...)` 直到写出 SubmitResp 的 `ctx.conn.write_frame(&resp_bytes).await?;`）替换为：
```rust
impl CmppBusinessHandler {
    async fn handle_submit(
        &self,
        ctx: &MessageContext,
        submit: &rsms_model::UnifiedSubmit,
    ) -> Result<()> {
        let phone = submit
            .dests
            .first()
            .map(|a| a.number.as_str())
            .unwrap_or("unknown")
            .to_string();

        // CMPP 方言 msg_id 落在 ProtocolExtra::Cmpp；长短信级联已被 adapter 剥进 submit.concat（窄腰）。
        let msg_id = match &submit.extra {
            ProtocolExtra::Cmpp(e) => e.msg_id,
            _ => [0u8; 8],
        };

        // 一步回执（窄腰）：框架按请求帧序列编码 SubmitResp 并写回，业务不再手剥序列/手拼字节。
        ctx.reply(UnifiedMessage::SubmitResp(rsms_model::UnifiedSubmitResp {
            msg_id: MessageId::Binary(msg_id.to_vec()),
            status: 0,
        }))
        .await?;
```
> 该替换删去了原 428-433 的 `version` 分支与 `encode_with_version`/`write_frame`，并去掉 `handle_submit` 原签名里的 `frame: &Frame` 与 `version: Option<u8>` 参数。

- [ ] **Step 4：修正 `handle_submit` 余下块对 `submit` 的借用**

`submit` 现为 `&UnifiedSubmit`（原为 owned）。把 436-491 行内：
- `if let Some(c) = submit.concat {` 改为 `if let Some(c) = &submit.concat {`
- 该分支内 `c.reference`/`c.total`/`c.sequence`/`c.to_udh_prefix()` 不变（`c` 现为 `&Concat`，方法仍可调）
- `submit.content`（两处：`seg_bytes.extend_from_slice(&submit.content);` 与 else 分支 `decode_text(&submit.content, submit.encoding)`）不变（已是 `&`）
- `submit.encoding`（`Encoding` 实现 `Copy`）不变
- `want_report` 分支改为就地取版本：
```rust
        // 需要状态报告 → 通过 MessageSource 异步发送（出站仍按协商版本编码，不受窄腰入站路径影响）。
        if submit.want_report {
            if let Some(account) = ctx.conn.authenticated_account().await {
                let version = ctx.conn.protocol_version().await;
                let report = build_deliver_report(&account, &msg_id, &phone, version);
                self.msg_source.push(&account, report).await;
            }
        }

        Ok(())
    }
}
```
（`build_deliver_report`/`build_deliver_mo_with_udh`/`FileMessageSource`/`CmppServerEventHandler` 等保持不变。）

- [ ] **Step 5：`main` 改用 `message_handlers`**

把 657-660 行的 `.handlers(vec![Arc::new(CmppBusinessHandler { … })])` 改为：
```rust
        .message_handlers(vec![Arc::new(CmppBusinessHandler {
            msg_source: msg_source.clone(),
            merger: Arc::new(std::sync::Mutex::new(LongMessageMerger::new())),
        })])
```

- [ ] **Step 6：编译该 example**

Run（WSL）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo build -p cmpp_server 2>&1 | tail -20"
```
Expected：编译通过、无未用 import 告警（按编译器提示清理 `Frame`/未用项）。

- [ ] **Step 7：clippy + commit**

Run：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy -p cmpp_server 2>&1 | grep -E 'warning|error' | tail -10"
```
Expected：无告警。然后（Git Bash）：
```bash
git add examples/cmpp_server/src/main.rs
git commit -m "refactor(wp4-1): CMPP server example 迁移到 MessageHandler + ctx.reply"
```

---

## Task 4：CMPP client example 迁移到 `MessageHandler` + `ctx.reply`

**Files:**
- Modify: `examples/cmpp_client/src/main.rs`（import 19-30；`impl ClientHandler for CmppClientHandler` 320-379；`reply_deliver_resp` 381-385；`main` 中 `ClientBuilder::new` 427-430）

**Interfaces:**
- Consumes：Task 2 的 `ClientBuilder::with_message_handler` + `NoopClientHandler`；`MessageContext::reply`。
- Produces：无。

- [ ] **Step 1：改 import**

把 19-23 行：
```rust
use async_trait::async_trait;
use rsms_codec_cmpp::adapter::CmppAdapter;
use rsms_codec_cmpp::compute_connect_auth;
use rsms_connector::client::{ClientContext, ClientHandler};
use rsms_connector::{ClientBuilder, CmppDecoder, MessageItem, MessageSource};
```
改为（去掉 `ClientContext`/`ClientHandler`，加 `NoopClientHandler` 与 business 的 `MessageContext`/`MessageHandler`）：
```rust
use async_trait::async_trait;
use rsms_business::{MessageContext, MessageHandler};
use rsms_codec_cmpp::adapter::CmppAdapter;
use rsms_codec_cmpp::compute_connect_auth;
use rsms_connector::{ClientBuilder, CmppDecoder, MessageItem, MessageSource, NoopClientHandler};
```
> `CmppAdapter` 仍由 `build_submit`/`ClientMessageSource` 编码出站、`main` 编码 Bind/Unbind 用，保留。`Frame` 若仅旧 `on_inbound` 用到则从 24 行 `rsms_core` import 删除。

- [ ] **Step 2：`impl ClientHandler` 换为 `impl MessageHandler`**

把 320-379 整块替换为（去掉内部 `CmppAdapter.decode(frame)`，直接 match 框架已解码的 `msg`）：
```rust
#[async_trait]
impl MessageHandler for CmppClientHandler {
    fn name(&self) -> &'static str {
        "cmpp-client"
    }

    async fn on_message(&self, ctx: &MessageContext, msg: &UnifiedMessage) -> Result<()> {
        match msg {
            UnifiedMessage::BindResp(resp) => {
                if resp.status == 0 {
                    tracing::info!("✓ CMPP 认证成功");
                    self.authenticated.store(true, Ordering::Relaxed);
                } else {
                    tracing::error!("✗ CMPP 认证失败: status={}", resp.status);
                }
            }
            UnifiedMessage::SubmitResp(resp) => {
                let count = self.submit_count.fetch_add(1, Ordering::Relaxed) + 1;
                let id = match &resp.msg_id {
                    MessageId::Text(t) => t.clone(),
                    MessageId::Binary(b) => b.iter().map(|x| format!("{:02x}", x)).collect(),
                };
                tracing::info!("[{}] SubmitResp: msg_id={}, result={}", count, id, resp.status);
            }
            UnifiedMessage::Report(report) => {
                let count = self.report_count.fetch_add(1, Ordering::Relaxed) + 1;
                let msg_id = match &report.msg_id {
                    MessageId::Text(t) => t.clone(),
                    MessageId::Binary(b) => b.iter().map(|x| format!("{:02x}", x)).collect(),
                };
                tracing::info!(
                    "[{}] 状态报告: msg_id={}, src={}, dest={}, raw={}",
                    count,
                    msg_id,
                    report.src.number,
                    report.dest.number,
                    String::from_utf8_lossy(&report.raw)
                );
                // 一步回执（窄腰）：框架按请求帧序列编码 DeliverResp 并写回。
                ctx.reply(UnifiedMessage::DeliverResp).await?;
            }
            UnifiedMessage::Deliver(deliver) => {
                self.handle_mo(
                    &deliver.src.number,
                    deliver.content.clone(),
                    deliver.encoding,
                    deliver.concat.clone(),
                );
                ctx.reply(UnifiedMessage::DeliverResp).await?;
            }
            UnifiedMessage::PingResp => tracing::info!("✓ 收到心跳响应 (ActiveTestResp)"),
            UnifiedMessage::UnbindResp => tracing::info!("收到 Terminate 响应，连接将关闭"),
            other => tracing::debug!("收到未处理统一消息: {:?}", other),
        }

        Ok(())
    }
}
```
> `msg` 为 `&UnifiedMessage`，故 `resp`/`report`/`deliver` 均为借用：`resp.msg_id` 改 `&resp.msg_id`、`deliver.content`/`deliver.concat` 改 `.clone()`（`handle_mo` 签名收 owned `Vec<u8>`/`Option<Concat>`，保持不变）。

- [ ] **Step 3：删除 `reply_deliver_resp` 辅助函数**

删去 381-385 行（已被 `ctx.reply(UnifiedMessage::DeliverResp)` 取代）：
```rust
async fn reply_deliver_resp(ctx: &ClientContext<'_>, frame: &Frame) -> Result<()> {
    let bytes = CmppAdapter.encode(&UnifiedMessage::DeliverResp, CmppAdapter.sequence_of(frame))?;
    ctx.conn.write_frame(&bytes).await
}
```

- [ ] **Step 4：`main` 改用 `with_message_handler` + `NoopClientHandler` 占位**

把 427-430 行：
```rust
    let conn = ClientBuilder::new(endpoint, handler, CmppDecoder)
        .message_source(msg_source as Arc<dyn MessageSource>)
        .connect()
        .await?;
```
改为：
```rust
    // 窄腰主路径：业务处理器经 with_message_handler 注入；new 第二参用 NoopClientHandler 占位
    // （new 签名暂仍强制 ClientHandler，WP4-3 删旧路径时清理）。
    let conn = ClientBuilder::new(endpoint, Arc::new(NoopClientHandler), CmppDecoder)
        .with_message_handler(handler)
        .message_source(msg_source as Arc<dyn MessageSource>)
        .connect()
        .await?;
```
> `handler` 现需为 `Arc<dyn MessageHandler>`。它由 409 行 `Arc::new(CmppClientHandler::new(...))` 构造，类型自动满足（`CmppClientHandler` 现 impl `MessageHandler`）。

- [ ] **Step 5：编译该 example**

Run（WSL）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo build -p cmpp_client 2>&1 | tail -20"
```
Expected：编译通过、无未用 import 告警。

- [ ] **Step 6：clippy + commit**

Run：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy -p cmpp_client 2>&1 | grep -E 'warning|error' | tail -10"
```
Expected：无告警。然后（Git Bash）：
```bash
git add examples/cmpp_client/src/main.rs
git commit -m "refactor(wp4-1): CMPP client example 迁移到 MessageHandler + ctx.reply"
```

---

## Task 5：CMPP 集成测试脚手架迁移（`cmpp_test.rs`）

**Files:**
- Modify: `tests/cmpp/cmpp_test.rs`（import 4-7、19；`impl ClientHandler for ClientState` 117；`impl BusinessHandler for ServerHandler` 273；本地 `start_test_server` 322-331；`ClientBuilder::new` 554、679）

**Interfaces:**
- Consumes：Task 1-4 的全部框架与 example 模式。
- Produces：无（测试终端）。验收：`cmpp-integration` 全绿，证明窄腰服务端 + 客户端真实端到端打通（这是 Task 1/2 新路径的首个真实覆盖）。

- [ ] **Step 1：改 import**

`use rsms_connector::{… ClientBuilder, …}`（4 行附近）保留；第 7 行 `use rsms_connector::client::{ClientContext, ClientConfig, ClientHandler};` 去掉 `ClientContext`/`ClientHandler`、保留 `ClientConfig`，并追加 `NoopClientHandler`；加 `use rsms_business::{MessageContext, MessageHandler};`。最终该组 import 形如：
```rust
use rsms_connector::client::ClientConfig;
use rsms_connector::NoopClientHandler;
use rsms_business::{MessageContext, MessageHandler};
```

- [ ] **Step 2：迁移 `ServerHandler`（服务端业务处理器）**

把 `impl rsms_business::BusinessHandler for ServerHandler`（273 行起）改为 `impl MessageHandler`：方法签名换成 `async fn on_message(&self, ctx: &MessageContext, msg: &UnifiedMessage) -> Result<()>`；删去函数体内对 `CmppAdapter.decode(frame)` 的调用，直接 `match msg`；所有「构造 SubmitResp/DeliverResp 字节 + `ctx.conn.write_frame`」改为 `ctx.reply(UnifiedMessage::SubmitResp(...))` / `ctx.reply(UnifiedMessage::DeliverResp)`。`submit`/`deliver` 改按引用取字段（owned 字段 `.clone()`）。模式与 Task 3 完全一致。

- [ ] **Step 3：迁移 `ClientState`（客户端处理器）**

把 `impl ClientHandler for ClientState`（117 行起）改为 `impl MessageHandler`：`on_inbound(ctx,frame)` → `on_message(ctx,msg)`；删内部 decode、直接 `match msg`；回执用 `ctx.reply`。模式与 Task 4 一致。

- [ ] **Step 4：本地 `start_test_server` 用 `message_handlers`**

把 322-331 行本地 `start_test_server` 的入参类型改为 `biz_handler: Arc<dyn MessageHandler>`，并把 `ServerBuilder::new(cfg).handlers(vec![biz_handler])` 改为 `.message_handlers(vec![biz_handler])`。

- [ ] **Step 5：两处 `ClientBuilder::new` 用 `with_message_handler`**

把 554 行与 679 行的 `ClientBuilder::new(endpoint, client_state.clone(), CmppDecoder)` 改为：
```rust
ClientBuilder::new(endpoint, Arc::new(NoopClientHandler), CmppDecoder)
    .with_message_handler(client_state.clone())
```
（679 行在 `.clone()` 链式调用上下文中同样插入 `.with_message_handler(client_state.clone())`，`endpoint.clone()` 保持。）

- [ ] **Step 6：跑集成测试**

Run（WSL）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test cmpp-integration 2>&1 | grep -E 'test result|FAILED|error' | tail -20"
```
Expected：`test result: ok`，0 failed。若某断言依赖旧裸帧行为，按窄腰语义（统一消息字段）修正断言，不得放宽零丢失/回执正确性。

- [ ] **Step 7：clippy + commit**

Run：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy -p rsms-tests --test cmpp-integration 2>&1 | grep -E 'warning|error' | tail -10"
```
Expected：无告警。然后（Git Bash）：
```bash
git add tests/cmpp/cmpp_test.rs
git commit -m "test(wp4-1): CMPP 集成测试脚手架迁移到 MessageHandler 并端到端验证"
```

---

## Task 6：CMPP 压测迁移 + 零丢失验收

**Files:**
- Modify: `tests/cmpp/stress_test.rs`、`tests/cmpp/multi_account_stress_test.rs`（各自本地 `impl BusinessHandler`/`impl ClientHandler` + 本地 `start_test_server` + `ClientBuilder::new`）

**Interfaces:**
- Consumes：Task 1-5 的全部模式。
- Produces：无。验收：两个压测零丢失（这是 WP4-1 的最终验收线）。

- [ ] **Step 1：迁移 `stress_test.rs` 的本地 handler 与 builder 调用**

按 Task 5 同一套机械变换处理 `tests/cmpp/stress_test.rs`：
- 本地 `impl BusinessHandler for ServerHandler` → `impl MessageHandler`（`on_inbound`→`on_message`，去 decode，回执用 `ctx.reply`）
- 本地 `impl ClientHandler for ClientState` → `impl MessageHandler`
- 本地 `start_test_server`（约 324 行）`.handlers(vec![…])` → `.message_handlers(vec![…])`，入参类型改 `Arc<dyn MessageHandler>`
- 两处 `ClientBuilder::new(endpoint, client_state, CmppDecoder)`（约 554、679 行）→ `ClientBuilder::new(endpoint, Arc::new(NoopClientHandler), CmppDecoder).with_message_handler(client_state)`
- import 同 Task 5 Step 1
- **不下调日志级别**：`EndpointConfig` 保持 `.with_log_level(WARN)`

- [ ] **Step 2：迁移 `multi_account_stress_test.rs`**

对 `tests/cmpp/multi_account_stress_test.rs` 施加完全相同的变换。

- [ ] **Step 3：跑单账号压测（零丢失）**

Run（WSL）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test cmpp-stress-test -- --nocapture 2>&1 | grep -E 'test result|TPS|丢失|loss|sent|recv|FAILED' | tail -30"
```
Expected：`test result: ok`；输出显示 sent==recv（零丢失）、吞吐为 WARN 级数量级（~万 TPS）。**端口竞争偶发单目标超时是已知 flaky、非回归**（见 [[stress-test-port-flaky]]），单独重跑确认。

- [ ] **Step 4：跑多账号压测（零丢失）**

Run（WSL）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test cmpp-multi-account-stress-test -- --nocapture 2>&1 | grep -E 'test result|TPS|丢失|loss|sent|recv|FAILED' | tail -30"
```
Expected：`test result: ok`、零丢失。

- [ ] **Step 5：全工作区回归 + clippy**

Run（WSL）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test --workspace --lib 2>&1 | grep -E 'test result|error' | tail -20"
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy --workspace 2>&1 | grep -E 'warning|error' | tail -20"
```
Expected：全绿、clippy 零告警。SMGP/SMPP/SGIP 仍走旧路径，应不受影响。

- [ ] **Step 6：commit**

（Git Bash）：
```bash
git add tests/cmpp/stress_test.rs tests/cmpp/multi_account_stress_test.rs
git commit -m "test(wp4-1): CMPP 压测迁移到 MessageHandler 并复验零丢失"
```

---

## Self-Review 检查（写计划后自查结论）

- **Spec/笔记覆盖**：笔记 §6 的 1a→Task 1、1b→Task 2、1c→Task 3+4、1d→Task 5+6 全覆盖；D1/D2/D3 已敲定并标注 WP4-1 落地边界；版本感知(D1a)/心跳收归(D3b)/删并存桥明确划归 WP4-3，不在本计划。
- **关键前置已识别**：客户端 `ClientConnection` 需补 `impl rsms_business::ProtocolConnection`（Task 2 Step 3）；服务端 id_gen 的 Option↔非Option 落差用连接级 fallback 解决（Task 1 Step 3）；`ClientBuilder::new` 强制 `client_handler` 用 `NoopClientHandler` 占位（Task 2 Step 4）。
- **类型一致性**：`message_handlers: Vec<Arc<dyn MessageHandler>>`、`with_message_handler(Arc<dyn MessageHandler>)`、`MessageContext::new(...)` 五参签名、`ctx.reply(UnifiedMessage)` 全程一致。
- **遗留风险（执行时留意）**：① CMPP V2.0 入站/回执在 WP4-1 退化、`cmpp20_test.rs` 暂留旧路径不迁（WP4-3 补）；② 若除 `server.rs:198` 外还有其他 `run_connection` 调用点，`cargo build` 会因参数数不符报错，按同样方式补 `mh`；③ 压测端口 flaky 单独重跑。

## 执行交接

计划已存 `docs/superpowers/plans/2026-06-28-wp4-1-cmpp-vertical-slice.md`。两种执行方式：
1. **Subagent-Driven（推荐）**：每 task 派新 subagent 实现，task 间两段式评审（实现→评审→必要时 fix→提交），动主循环/压测的 task 必跑 `cmpp-integration` + `cmpp-stress-test`/`cmpp-multi-account-stress-test`。
2. **Inline**：本 session 内按 executing-plans 批量执行、检查点评审。

关联记忆：[[onboarding-ergonomics-reshape]]、[[git-remote-via-wsl]]、[[stress-test-port-flaky]]、[[java-interop-stress-4proto]]。
