# RSMS 对接易用性重塑 · 设计文档

- 日期：2026-06-28
- 状态：已评审通过，待转实现计划
- 范围：`rsms-core` / `rsms-connector` / `rsms-business` / `rsms-model` / `examples` / `docs`
- 前提：项目版本 `0.0.1`，尚未对外发布、无外部接入方，**允许 breaking change，无需向后兼容**

## 1. 背景与问题

RSMS 的「窄腰统一模型」（`rsms-model` 的 `UnifiedMessage` + `ProtocolAdapter` trait）在**中间层**已经落地良好：四协议（CMPP / SMGP / SMPP / SGIP）的头长度、MsgId 格式、序列号布局、UDH 标记差异都被各 adapter 吸收，`adapter_registry::adapter_for(protocol)` 可按协议中央取 adapter。手写字节、查协议头偏移的负担基本归零。

但窄腰的两个「喇叭口」仍敞开，对接方的真实成本几乎全部集中在这两端：

| 端 | 现状 | 对接方被迫做的事 |
|---|---|---|
| 入站端 | `BusinessHandler::on_inbound` 仍收裸 `Frame`（`rsms-business/src/lib.rs:25`）| 每次手工 `CmppAdapter.decode(frame)?` + 手工版本感知分支 + match |
| 出站回执端 | 框架不代回业务层 resp（设计如此）| 每条 Submit 手工 `adapter.encode(SubmitResp, adapter.sequence_of(frame))` + `write_frame` 三步 |
| 配置端 | `EndpointConfig` 用运行期 enum（`rsms-core/src/endpoint.rs`）| `with_protocol` / `window_size` / `decoder` 类错误编译期全过、运行期才炸 |

试点方法 `on_message(UnifiedMessage)` 虽存在，但**默认空实现、框架不驱动**（`rsms-business/src/lib.rs`），窄腰好处目前**未传导到对接面**。

### 1.1 量化现状（来自代码勘探）

- 单协议单向最小对接：约 300–850 行；四协议双向：约 3000–8000 行。
- 四协议 example 结构重复率约 88%。
- 协议切换实际改动点 7–10 处（文档曾声称 3 处）：除 `with_protocol` / `Decoder` / codec import 外，还隐含认证方式、版本感知、序列号打包、Report 承载等。
- 一次完整对接需从 7 个 crate import 8–12 个 `use` 块。

### 1.2 高发易错点（编译期均无法拦截，运行期才暴露）

1. `with_protocol` 漏写 → 默认 `Protocol::Cmpp` → SMPP/SGIP 序列号偏移错（8 vs 12 字节）。
2. `window_size` 默认 16 太小 → 高吞吐从 ~12k TPS 崩到 ~2k。
3. 业务层 resp 忘回 → 对端滑动窗口耗尽、连接假死。
4. `MessageSource` 的 key 用错（详见 §4，命名为 `account` 但语义是 `endpoint.id`）。
5. `sequence_id` 须账号级唯一；UCS2 须 UTF-16BE 大端。

## 2. 目标与非目标

### 目标

1. 让对接方**面向 `UnifiedMessage` 编程、协议无关**：入站自动 decode、出站一步 reply。
2. 把高发配置错误**从运行期提前到编译/启动期**。
3. 用 prelude + 内置实现把样板与 import 负担降到最低。
4. 厘清并修正 `MessageSource` key 的误导性命名与双重语义。
5. 配套「5 分钟对接」指南与易错点 checklist。

### 非目标

- 不改变协议 codec 的字节级实现（adapter 内部维持现状）。
- 不引入新的传输层 / TLS（属另一阶段）。
- 不追求向后兼容旧 API（项目未发布）。

### 衡量指标（验收基准）

| 指标 | 现在 | 目标 |
|---|---|---|
| 单协议单向对接代码量 | 300–850 行 | ≤ 200 行 |
| 协议切换改动点 | 7–10 处 | 1–2 处 |
| 入站 decode / 出站 encode | 业务手工 + 版本分支 | 框架包办 |
| protocol/window/decoder 错误暴露时机 | 运行期 | 编译期或 `serve()`/`connect()` 启动期 |
| import | 7 crate / 8–12 `use` 块 | 1 个 prelude |

## 3. 模块 1 · 延伸窄腰到对接面（核心）

### 3.1 新主 trait `MessageHandler`

以面向 `UnifiedMessage` 的 `MessageHandler` 作为默认主 API，取代裸 `Frame` 的 `BusinessHandler`：

```rust
pub trait MessageHandler: Send + Sync {
    fn name(&self) -> &'static str;
    async fn on_message(&self, ctx: &MessageContext, msg: UnifiedMessage) -> Result<()>;
}
```

框架在连接主循环（`rsms-connector/src/connection.rs` 的 `run_connection`）里：

- 对**业务层消息**（Submit / Deliver / Report / 客户端收到的 BindResp 等）自动 `adapter.decode(frame)`（含版本感知），再驱动 `on_message`。
- **协议层消息**（心跳、服务端握手 BindResp）框架自行消化，**不进** `on_message`。

### 3.2 `MessageContext`（取代 `InboundContext`）

`MessageContext` 内嵌当前连接对应的 adapter 与当前帧的序列，向对接方暴露一步式收发：

```rust
pub struct MessageContext {
    pub endpoint: Arc<EndpointConfig>,
    pub conn: Arc<dyn ProtocolConnection>,
    // 私有：经便捷方法暴露
    // adapter: &'static dyn ProtocolAdapter,
    // frame_sequence: Sequence,
}

impl MessageContext {
    /// 回执：自动 encode（版本感知）+ 套请求帧 sequence + write_frame
    pub async fn reply(&self, msg: UnifiedMessage) -> Result<()>;
    /// 主动下发（非回执场景，自分配 sequence）
    pub async fn send(&self, msg: UnifiedMessage) -> Result<()>;
    /// 账号级 ID 生成器（框架保证存在，不再是 Option）
    pub fn id_generator(&self) -> &Arc<dyn IdGenerator>;
    /// 出站队列 key（见 §4）
    pub fn channel_key(&self) -> &str;
}
```

`reply` 内部等价于：`let bytes = adapter.encode(&msg, self.frame_sequence)?; conn.write_frame(&bytes).await`。

`id_generator` 由 `Option<Arc<dyn IdGenerator>>` 收紧为 `Arc<dyn IdGenerator>`：框架在建连接时总会注入（默认 `SimpleIdGenerator`），消除对接方每次判空。

### 3.3 对接面前后对比（服务端处理 Submit 回 Resp）

```rust
// 现在 —— 业务手工 decode + 版本分支 + 手工 encode/write
let version = ctx.conn.protocol_version().await;
let unified = if version == Some(0x20) { CmppAdapter.decode_with_version(frame, V20)? }
             else { CmppAdapter.decode(frame)? };
match unified {
    UnifiedMessage::Submit(s) => {
        let resp = UnifiedMessage::SubmitResp { msg_id, status: 0 };
        let bytes = CmppAdapter.encode(&resp, CmppAdapter.sequence_of(frame))?;
        ctx.conn.write_frame(&bytes).await?;
    }
    _ => {}
}

// 重塑后 —— 协议无关，零 CmppAdapter 字样，四协议同一份代码
match msg {
    UnifiedMessage::Submit(s) => {
        ctx.reply(UnifiedMessage::SubmitResp { msg_id, status: 0 }).await?;
    }
    _ => {}
}
```

### 3.4 版本感知内化

框架在 decode/reply 时按 `conn.protocol_version()` 自动选择 `decode_with_version` / `encode_with_version`（目前仅 CMPP V2.0/V3.0 有字段宽度差异）。对接方不再写 `if version == 0x20` 分支。

### 3.5 补 `UnifiedMessage::ReportResp`

新增 `UnifiedMessage::ReportResp` 变体，消除 SGIP 当前用 `UnifiedMessage::Unknown { command_id, raw: vec![] }` 兜底独立 Report-Resp 的特例（`examples/sgip_*` 与 SGIP adapter）。各 adapter 的 encode 增加对该变体的处理。

### 3.6 逃生舱口 `RawFrameHandler`

保留低层 trait 供极少数需要碰裸字节的高级场景：

```rust
pub trait RawFrameHandler: Send + Sync {
    fn name(&self) -> &'static str;
    async fn on_frame(&self, ctx: &MessageContext, frame: &Frame) -> Result<()>;
}
```

`ServerBuilder` / `ClientBuilder` 同时接受 `MessageHandler`（默认主路径）或 `RawFrameHandler`（逃生舱口）。绝大多数对接只用前者。

## 4. 模块 2 · 配置护栏与 key 语义厘清

### 4.1 `protocol` 提为必填

`EndpointConfig::new` 把 `protocol` 提为必填参数，删除 `Protocol::Cmpp` 默认值。漏写直接编译失败，杜绝「默认 Cmpp → 序列号偏移错」。

### 4.2 删除客户端显式 `Decoder` 参数

`ClientBuilder::new(endpoint, handler, decoder)` 中 `decoder` 与 `protocol` 可能不一致。改为框架内部按 `protocol` 决定 `decoder_for(protocol)`，删掉显式 `decoder` 入参——协议切换少一处改动、少一个不一致出错点。

### 4.3 `window_size` 默认值上调 + 启动期校验

将默认 `window_size` 上调到面向吞吐的合理量级；`serve()` / `connect()` 启动时若检测到 `window_size` 过小且非显式设置，发出 `warn`（或抬升到下限）。

### 4.4 修正 `MessageSource` key 的误导性命名与双重语义

**问题根因**（带证据）：

- 客户端侧 `fetch` 的 key 实为 `endpoint.id`，而非运营商账号：
  - `client.rs:520-522`：客户端 `authenticated_account()` 直接返回 `self.endpoint.id.clone()`。
  - `client.rs:946-948`：`run_outbound_fetcher` 用它作 `fetch` 的 key。
  - `endpoint.rs:7`：`id` 注释为「端点唯一标识」。
- 服务端侧 `authenticated_account()` 返回**真实鉴权账号**（`AuthResult.account`，见 `connection.rs:233-239` 的 `set_authenticated_account`）。

因此同一个 `MessageSource::fetch(account, ...)` 的 `account` 参数：**client 侧 = `endpoint.id`，server 侧 = 真实鉴权账号**，双重语义且命名误导。这会导致「多运营商、账号撞名、IP:port 不同」场景下对接方误用运营商账号作 key，造成拉不到或串号。

> 说明：现有「client 侧 key 用 `endpoint.id`」的设计本身是**正确的**——它让账号撞名的多运营商连接因 id 不同而天然隔离。问题只在命名与语义未显式化。

**整改：**

1. 把 `MessageSource` 的 key 参数从 `account` 改名为 `channel_key`（语义中立），并在 trait 文档写死两侧确切含义。
2. 在类型/文档层面明确：client 侧 `channel_key == endpoint.id`，server 侧 `channel_key == 鉴权账号`。
3. `MessageContext::channel_key()` 直接返回「该用什么 key 入队」，对接方无需猜测；push 与 fetch 两端都以它为准。
4. 评估是否将 client/server 的出站队列概念拆为两个清晰封装（而非共用一个含糊 `&str`）——实现阶段据复杂度决定，至少必须做到命名与文档自解释。

## 5. 模块 3 · prelude + 内置实现

### 5.1 `rsms::prelude`

新增 prelude（落在 `rsms-connector` 或聚合 crate），一次性导出 `ServerBuilder` / `ClientBuilder` / `MessageHandler` / `RawFrameHandler` / `MessageContext` / `UnifiedMessage` 及常用类型（`Protocol` / `Address` / `MessageId` / `Encoding` / `Concat` 等）。把 8–12 个 `use` 块收成 1–2 个。

### 5.2 内置实现

覆盖大多数对接，免去每协议重写：

- `InMemoryMessageSource`：按 `channel_key` 分队列，提供 `push` / `fetch`。
- `PasswordAuthHandler`：基于 `HashMap<account, password>`，**自动按协议算 MD5（CMPP/SMGP）或明文（SMPP/SGIP）**；另提供闭包式 `auth_fn`。
- `DefaultAccountConfigProvider`：返回面向吞吐的合理默认值。

### 5.3 编码助手

把 examples 中重复的 `to_wire_bytes` / `decode_text`（含 UCS2 = UTF-16BE 大端转换）收进公开助手（`rsms-model` 或 `rsms-longmsg`），避免每个对接方重写并踩 UTF-16BE 雷。

## 6. 模块 4 · 对接者指南

`docs/guides/` 新增「5 分钟对接」：

- 用新 API 的最小 server / client 模板（目标 ≤ 200 行）。
- 易错点 checklist：`protocol` 必填、`window_size`、`channel_key` 语义、UCS2 编码。
- 四协议切换差异表（重塑后应缩到 1–2 处）。

## 7. Breaking 变更清单

1. `BusinessHandler::on_inbound(&Frame)` → `MessageHandler::on_message(UnifiedMessage)`（裸帧路径迁至 `RawFrameHandler`）。
2. `InboundContext` → `MessageContext`（字段与方法变化；`id_generator` 去 `Option`）。
3. `EndpointConfig::new` 增加必填 `protocol` 参数，删除 `Protocol::Cmpp` 默认值。
4. `ClientBuilder::new` 删除 `decoder` 入参。
5. `MessageSource::fetch` 参数 `account` → `channel_key`。
6. 新增 `UnifiedMessage::ReportResp` 变体（match 穷尽性受影响）。
7. `window_size` 默认值变化。

所有 `examples/*` 与 `tests/*`（`rsms-tests`）随之迁移。

## 8. 实施顺序（分阶段，每阶段可独立编译验证）

构建/测试一律走 WSL（`wsl bash -lc "cd /mnt/g/RustProjects/rsms && cargo ..."`，`RUSTFLAGS='--cap-lints allow'`）；本地 commit 走 Git Bash（避免 CRLF 翻转）。

- **阶段 1（基石）**：模块 1 —— `MessageHandler` / `MessageContext` / `reply`/`send` / 版本感知内化 / `ReportResp` / `RawFrameHandler`；框架主循环改为自动 decode 驱动。迁移一个协议（CMPP）的 server+client example 验证编译与既有集成测试。
- **阶段 2（护栏）**：模块 2 —— `protocol` 必填、删 decoder 入参、window 默认值与启动校验、`channel_key` 改名与语义厘清。
- **阶段 3（减样板）**：模块 3 —— prelude、`InMemoryMessageSource`、`PasswordAuthHandler`、`DefaultAccountConfigProvider`、编码助手；用内置实现重写四协议 example。
- **阶段 4（文档）**：模块 4 —— 「5 分钟对接」指南 + checklist + 切换差异表；同步更新 `CLAUDE.md` / `AGENTS.md` / `README` 中过时的对接描述。

每阶段结束：`cargo clippy --workspace` 零告警 + 相关集成/压测通过（压测 `WARN` 日志级别）。

## 9. 已拍板的决策点

1. **API 演进策略**：重塑主路径，允许 breaking，不保留旧 API 兼容层。
2. **自动回执边界**：协议层 resp（心跳两向 + 服务端握手 BindResp）框架自动回；业务层 resp（SubmitResp/DeliverResp/ReportResp）由对接方 `ctx.reply` 控制。
3. **Bind 成功感知**：服务端走 `on_authenticated` / `on_disconnected` 生命周期回调；客户端由 `connect().await` 直接返回握手结果（成功返回就绪连接、失败返回 `Err`），不在 handler 里捞。
4. **裸帧路径**：降级为 `RawFrameHandler` 逃生舱口，保留而非删除。
5. **`channel_key`**：修正 `account` 误导命名，显式化 client/server 双侧语义。

## 10. 风险与缓解

- **风险：自动 decode 对每帧增加开销。** 缓解：`adapter.decode` 已在 `unified-shadow` feature 下验证过正确性与可行性；主循环只做一次 decode，不重复。
- **风险：版本感知内化遗漏非 CMPP 的未来差异。** 缓解：版本选择集中在框架一处，新增差异只改一点。
- **风险：`channel_key` 双侧语义仍可能被误解。** 缓解：`MessageContext::channel_key()` 让对接方无需理解差异即可拿到正确 key；文档配示例。
- **风险：breaking 面大、example/test 迁移量大。** 缓解：分阶段、每阶段独立编译验证，先以 CMPP 打通再铺开四协议。
