# WP4 设计笔记（主循环切换到 MessageHandler）

> WP4 是「对接易用性重塑」里风险最高的工作包：动 `connection.rs` 服务端主循环 + 客户端读循环 + 两个 builder + 压测链路。本笔记固化两轮勘探结论、未定设计决策、推荐 task 切分，供执行时（含跨 session）无缝恢复。
> 拆分策略（用户 2026-06-28 拍板）：**按协议垂直切片**——先 CMPP 全链路打通验证整套模式，再横向铺 SMGP/SMPP/SGIP，最后心跳收归 + 退役旧 trait。

## 1. 现状：两条独立路径

**服务端**（`crates/rsms-connector/src/connection.rs`）：
- `run_connection(read, conn, handlers: Vec<Arc<dyn BusinessHandler>>, account_pool, account_config_provider, auth_handler, protocol, event_handler, metrics, shutdown)`（签名 `connection.rs:329-340`）。
- 每帧：`协议 handler.handle_frame(&frame, conn)` → `HandleResult`（握手/心跳/关闭在此 `write_frame` 并返回 `Continue`/`Stop`）。
- `unified-shadow` feature 下已有 `adapter_for(protocol).decode(&frame)`（`connection.rs:415-422`），仅打日志——**转正即可**。
- `HandleResult::Continue` → `run_chain(conn.config.clone(), conn as Arc<dyn ProtocolConnection>, &handlers, &frame, id_gen)`（`connection.rs:433-438`）；`id_gen = conn.account_connections().await.map(|ac| ac.id_generator().clone())`。

**客户端**（`crates/rsms-connector/src/client.rs`）：
- `run_client_read_loop(conn, client_handler: Arc<dyn ClientHandler>, decoder, event_handler, metrics)`（`client.rs:783-914`）。
- 构造 `ClientContext { endpoint: &conn.endpoint, conn: &conn }`（`client.rs:883-886`）→ `client_handler.on_inbound(&ctx, &frame)`（裸 Frame）。
- **无 run_chain、无 id_generator、无 adapter** 在 ctx 里。
- `ClientHandler` trait（`client.rs:574-577`）：`name()` + `on_inbound(&self, ctx: &ClientContext, frame: &Frame) -> Result<()>`。

**builder**：
- `ServerBuilder.handlers: Vec<Arc<dyn BusinessHandler>>`（`server.rs:41-50`），`handler()`/`handlers()` 方法（`server.rs:67-76`）；`BoundServer` 存 handlers，`run()` spawn `run_connection` 传入（`server.rs:138-204`）。
- `ClientBuilder<D: FrameDecoder>`（`client.rs:587-614`）：`client_handler: Arc<dyn ClientHandler>` + `decoder: D`；`connect() -> Result<Arc<ClientConnection>>`（`client.rs:646`）。

## 2. resp 现状归属（勘探坐实）

| 消息 | CMPP | SMGP | SMPP | SGIP | 谁回 |
|---|---|---|---|---|---|
| 握手(Connect/Login/Bind) | 框架 | 框架 | 框架 | 框架 | 协议 handler 内 `write_frame` |
| 心跳(ActiveTest/EnquireLink) | **业务回** | 框架✓ | 框架✓ | **业务回** | 混合——需收归 |
| Submit | 业务 | 业务 | 业务 | 业务 | 业务 `reply` |
| Deliver/Report | 业务 | 业务 | 业务 | 业务 | 业务 `reply` |
| 关闭(Terminate/Exit/Unbind) | 框架 | 框架 | 框架 | 框架 | 协议 handler / `close_packet` |

`HandleResult` 枚举（`protocol.rs`）：`Continue` | `Stop`。

## 3. 客户端 id_generator 方案（坐实）

`ClientConnection`（`client.rs:203-228`）无 `id_generator` 字段；`account_connections: Mutex<Option<Arc<AccountConnections>>>` 初始 `None`，客户端从不像服务端那样握手后注入。`AccountConnections.id_generator()` 持 `Arc<dyn IdGenerator>`（`pool.rs:88-118`，初始化 `SimpleIdGenerator::new()`）。`SimpleIdGenerator`（`id_generator.rs`）有 `new()`/`Default`。

**结论**：给 `ClientConnection` 加 `id_generator: Arc<dyn IdGenerator>` 字段，`ClientConnection::new`（`client.rs:231`）里 `Arc::new(SimpleIdGenerator::new())` 初始化。比注入整个 `AccountConnections` 干净（客户端不需要池语义）。`ClientConnection` 已 impl `ProtocolConnection`（`client.rs:507-565`，`authenticated_account()` 返回 `endpoint.id`）。

## 4. 三个未定设计决策（执行前须敲定）

### D1. 版本感知内化
`ProtocolAdapter` trait 只有 `decode(&Frame) -> UnifiedMessage`，无版本参数。CMPP V2.0/V3.0 字段宽度不同，现 example 手调 `CmppAdapter.decode_with_version(frame, V20)`。要把它内化进框架（spec §3.4），候选：
- (a) `ProtocolAdapter` 加 `decode_with_version(&self, frame, version: Option<u8>)`，默认实现转调 `decode`；框架在驱动层按 `conn.protocol_version()` 调它。
- (b) 框架只调 `decode`，CMPP adapter 内部从 frame 不可得 version → 不可行（version 来自握手协商，不在帧里）。
- **倾向 (a)**：trait 加版本感知方法（默认转 `decode`），只有 CMPP override。WP4-1 可先用基础 `decode`（V3.0 路径），版本感知作为 WP4 内单独一步（避免首切片膨胀）。

### D2. 客户端并存桥形态
服务端 `Vec<handler>` 易并存（加 `message_handlers` 字段）。客户端是**单** `ClientHandler`。候选：
- (a) `ClientBuilder` 加 `with_message_handler(Arc<dyn MessageHandler>)`，与 `client_handler` 二选一；`run_client_read_loop` 按哪个非空择路。
- (b) 直接换 `ClientBuilder::new` 第二参为 `MessageHandler`（无并存，CMPP client example 同步迁、其他客户端 example 暂时编译失败——不可行，违垂直切片）。
- **倾向 (a)**：临时并存，WP4 收尾删旧 `ClientHandler`。

### D3. 心跳 resp 收归位置
CMPP/SGIP 服务端心跳现由业务回。收归框架候选：
- (a) 在协议 `handle_frame` 里对 ActiveTest 直接 `write_frame(adapter.encode(PingResp, seq))` 并返回 `Continue`（对齐 SMGP/SMPP 现状）——改 4 个 `handlers/*.rs`。
- (b) 在新的 decode 驱动层：decode 出 `UnifiedMessage::Ping` 时框架自动 `reply(PingResp)`，不进 `on_message`。
- **倾向 (b)**：与「协议层 resp 框架包办」一致、集中一处、协议无关。放 WP4 心跳收归步（WP4-3）。

## 5. 临时并存桥（垂直切片的脚手架，WP4 收尾删除）

因 `run_connection`/`ServerBuilder`/`ClientBuilder` 四协议共享，「只切 CMPP」需临时并存：
- 服务端：`ServerBuilder` 加 `message_handlers: Vec<Arc<dyn MessageHandler>>` + `message_handler()`/`message_handlers()`；`run_connection` 加 `message_handlers` 参数；`Continue` 分支——`message_handlers` 非空 → `decode + MessageContext + run_message_chain`，否则旧 `run_chain`。
- 客户端：见 D2(a)。
- CMPP example/test 改用新 handler，SMGP/SMPP/SGIP 暂留旧 `BusinessHandler`/`ClientHandler`。验证 CMPP 压测零丢失后逐协议迁，**最后删并存桥 + 旧 trait**。

## 6. 推荐 task 切分

- **WP4-1（CMPP 垂直切片）**
  - 1a 服务端并存桥：`ServerBuilder`/`BoundServer`/`run_connection` 支持 `message_handlers`；`Continue` 分支 decode(基础版,V3.0)+`MessageContext`+`run_message_chain`。验证：既有 `BusinessHandler` 路径不回归 + 新增最小 MessageHandler 集成测试。
  - 1b 客户端切换：`ClientConnection` 加 `id_generator`；`ClientBuilder` 加 `with_message_handler`（D2a）；`run_client_read_loop` 择路构造 `MessageContext` 调 `on_message`。
  - 1c CMPP server+client example 迁移到 `MessageHandler`/`ctx.reply`。
  - 1d CMPP 集成 + 压测迁移并验证零丢失。
- **WP4-2**：横向铺 SMGP/SMPP/SGIP（example+test 迁移，逐协议压测）。
- **WP4-3**：版本感知内化(D1a) + 心跳收归(D3b) + 删并存桥 + 退役 `BusinessHandler`/`InboundContext`/`run_chain`/`ClientHandler`/旧 `on_message` 试点。全量集成+压测。

## 7. 执行注意

- cargo 走 WSL（`RUSTFLAGS='--cap-lints allow'`）；commit 走 Git Bash；压测 WARN 日志、零丢失为验收线。
- 每个 task 走 implementer→task 评审→（必要时 fix）→提交；动主循环/压测的 task 必须实跑 `cmpp-integration` + `cmpp-stress-test`/`cmpp-multi-account-stress-test`。
- 关联记忆 [[onboarding-ergonomics-reshape]]、[[git-remote-via-wsl]]、[[java-interop-stress-4proto]]（压测端口/账号映射）。
