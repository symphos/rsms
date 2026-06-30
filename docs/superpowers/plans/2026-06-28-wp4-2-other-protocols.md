# WP4-2（横向铺 SMGP / SMPP / SGIP）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: 用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐 task 执行。步骤用 checkbox（`- [ ]`）跟踪。
> **语言要求**：全程思考与输出用中文（仅代码英文关键词除外，见 AGENTS.md / CLAUDE.md）。

**Goal:** 把 WP4-1 已在 CMPP 验证的窄腰迁移模式（`MessageHandler` + `ctx.reply`）横向铺到 SMGP / SMPP / SGIP 三协议的 server/client example、集成测试与压测，使四协议主链路全部走窄腰新路径并以零丢失压测验收。

**Architecture:** **无框架改动**——WP4-1 的并存桥（服务端 `ServerBuilder.message_handlers`/`run_connection`、客户端 `ClientBuilder::with_message_handler` + `NoopClientHandler`）已是协议无关的（按 `crate::adapter_registry::adapter_for(protocol)` 解码），对四协议直接生效。本计划只做 example/test 的机械迁移：把各协议本地的 `BusinessHandler`/`ClientHandler`（自己 `XxxAdapter.decode` + 手动 `encode`+`write_frame`）改为 `MessageHandler`（框架已解码）+ `ctx.reply`。三协议均**单版本/版本透明**，无 CMPP 那样的 V2.0/V3.0 偏差。

**Tech Stack:** Rust edition 2024；`rsms-business`（`MessageHandler`/`MessageContext`/`run_message_chain`）；`rsms-model`（`UnifiedMessage`/`ProtocolAdapter`）；`rsms-codec-{smgp,smpp,sgip}`（各 `XxxAdapter`）；`rsms-connector`（并存桥，已就绪）。

## Global Constraints

- **允许 breaking、无需向后兼容**（项目 0.0.1 未发布）。
- **clippy 零告警**：`cargo clippy` 必须 warning-free。
- **公共 API 必须有中文 doc 注释**（`///`/`//!`）。
- **cargo 一律走 WSL**，前缀 `RUSTFLAGS='--cap-lints allow'`；**commit 走 Git Bash**（见 [[git-remote-via-wsl]]）。
- **压测必须 WARN 日志**：`EndpointConfig` 已配 `.with_log_level(WARN)`，禁止下调。
- **压测验收线 = 零丢失**：sent 仅在 `send_request` 成功后计数。
- **不触碰框架代码**（`crates/rsms-connector` 等）：本计划仅改 `examples/` 与 `tests/` 下文件。若发现需要改框架，停下来上报——那是 WP4-3 范畴。
- **不触碰 `tests/common/`**：三协议测试均用各自文件内的本地 `start_test_server`。

## 范围边界

WP4-2 对每协议只迁与 CMPP WP4-1 对称的集合：**server example + client example + 集成测试（`*-integration`）+ 两个压测（`*-stress-test`、`*-multi-account-stress-test`）**。

其余 test target（`*-longmsg-test`、`*-dynamic-connection-test`、`smgp-unified-pilot-test`）**暂留旧 `BusinessHandler`/`ClientHandler` 路径**——并存桥保护其继续工作，与 CMPP 现状一致（CMPP 的 longmsg/dynamic/soak/fault 也未迁）。这些连同 CMPP 残留、`cmpp20_test.rs`、`stress_test.rs` 服务端侧一并留给 **WP4-3**（届时落地版本感知 decode + 删并存桥 + 退役旧 trait 时统一迁清）。

---

## 统一迁移变换（所有 6 个 task 共用，各 task 引用本节 + 附协议特有 delta）

> 活样板：CMPP 已迁移文件可直接对照——
> - server example 样板：`examples/cmpp_server/src/main.rs`
> - client example 样板：`examples/cmpp_client/src/main.rs`
> - 集成测试样板：`tests/cmpp/cmpp_test.rs`
> - 压测样板：`tests/cmpp/stress_test.rs`、`tests/cmpp/multi_account_stress_test.rs`
> 把 `Cmpp*` 换成目标协议的 `Smgp*`/`Smpp*`/`Sgip*`（adapter、decoder、codec 类型），其余结构一致。

**变换 T1（import）：**
- server 文件：删 `use rsms_business::BusinessHandler;`（及 `InboundContext`，若有），加 `use rsms_business::{MessageContext, MessageHandler};`。
- client 文件：删 `use rsms_connector::client::{ClientContext, ClientHandler};`（保留同 `use` 里的 `ClientConfig` 等其他项），加 `use rsms_business::{MessageContext, MessageHandler};` 与把 `NoopClientHandler` 并入既有 `use rsms_connector::{...}` 分组。
- 集成/压测文件：同上两条按其内含的 server/client 本地 handler 分别处理。
- 按编译器提示清理因迁移而不再使用的 import（典型：`Frame`、`InboundContext`、`ClientContext`、`ClientHandler`、`BusinessHandler`）。**但** adapter（`XxxAdapter`）、decoder（`XxxDecoder`）、鉴权助手（如 `compute_login_auth`）、出站 `MessageSource` 用到的类型一律**保留**（它们在 main/MessageSource/Bind 构造里仍用）。

**变换 T2（server 端业务处理器）：**
- `impl BusinessHandler for XxxBusinessHandler`（或测试里的 `TestBusinessHandler`/`ServerHandler`）→ `impl MessageHandler`。
- 方法：`async fn on_inbound(&self, ctx: &InboundContext, frame: &Frame) -> Result<()>` → `async fn on_message(&self, ctx: &MessageContext, msg: &UnifiedMessage) -> Result<()>`。
- 删去函数体内的 `let unified = XxxAdapter.decode(frame)?;`（或 match 解码 + 错误返回那段）——框架已解码，直接对入参 `msg` 做 `match msg { ... }`。
- 回执：把「`let bytes = XxxAdapter.encode(&resp, XxxAdapter.sequence_of(frame))?; ctx.conn.write_frame(&bytes).await?;`」整体替换为「`ctx.reply(resp).await?;`」（`resp` 为 `UnifiedMessage::XxxResp{...}`）。
- 借用调整：`msg: &UnifiedMessage`，故 match 出来的负载是借用（`&UnifiedSubmit` 等）。原对 owned 字段的 move 改为借用或 `.clone()`（`Copy` 字段如 `encoding` 无需 clone；`Vec`/`Option<Concat>`/`String`/`MessageId` 等按下游签名 `.clone()`）。`Option<Concat>`：`if let Some(c) = submit.concat` → `if let Some(c) = &submit.concat`。

**变换 T3（client 端处理器）：**
- `impl ClientHandler for XxxClientHandler`（或 `TestClientHandler`/`ClientState`）→ `impl MessageHandler`。
- `on_inbound(ctx: &ClientContext, frame: &Frame)` → `on_message(ctx: &MessageContext, msg: &UnifiedMessage)`；删内部 `XxxAdapter.decode(frame)`、直接 `match msg`。
- 回执（如 DeliverResp/ReportResp）：`XxxAdapter.encode(&UnifiedMessage::XxxResp, XxxAdapter.sequence_of(frame))? + write_frame` → `ctx.reply(UnifiedMessage::XxxResp).await?`。删除迁移后空置的回执辅助函数（如 `reply_deliver_resp`）。
- 借用调整同 T2。

**变换 T4（builder 调用点）：**
- server（example main 与本地 `start_test_server`）：`ServerBuilder::new(cfg).handlers(vec![biz])` → `.message_handlers(vec![biz])`；本地 `start_test_server` 的入参类型 `Arc<dyn BusinessHandler>` → `Arc<dyn MessageHandler>`。
- client（example main 与测试建连处）：`ClientBuilder::new(endpoint, handler, XxxDecoder)` → `ClientBuilder::new(endpoint, Arc::new(NoopClientHandler), XxxDecoder).with_message_handler(handler)`（`handler` 现为 `Arc<dyn MessageHandler>`，由 `Arc::new(XxxClientHandler::new(...))` 构造，类型自动满足）。

**变换 T5（保持不变）：** 各 example 的 `MessageSource`、出站 PDU 编码、鉴权（`compute_login_auth` 等 MD5/明文构造）、Bind 帧的构造与发送方式、`EndpointConfig`（含 `.with_protocol(...)`、`.with_log_level(WARN)`）一律**不改**。

---

## Task 1：SMGP — examples + 集成测试

**Files:**
- Modify: `examples/smgp_server/src/main.rs`（`impl BusinessHandler for SmgpBusinessHandler`，decode 行 294；main `.handlers(...)` 行 575–586）
- Modify: `examples/smgp_client/src/main.rs`（`impl ClientHandler for SmgpClientHandler`，decode 行 304；`ClientBuilder::new` 行 394）
- Modify: `tests/smgp/integration.rs`（本地 `TestBusinessHandler` decode 行 131；`TestClientHandler` decode 行 273；本地 `start_test_server` 行 314–341）

**Interfaces:**
- Consumes：WP4-1 并存桥（`ServerBuilder::message_handlers`、`ClientBuilder::with_message_handler`、`NoopClientHandler`、`MessageContext::reply`）。
- Produces：无（终端 example/test）。

**协议特有 delta（SMGP）：**
- 鉴权 MD5（`compute_login_auth`）在 client main 的 Bind 构造里（行 404 附近）——属 T5，**不动**。
- `SubmitResp` 的 `msg_id` 为 `MessageId::Binary(10B)`（SMGP 自定义 10B MsgId）——回执构造照搬现状字段、只把发送改为 `ctx.reply`。
- 无版本分支、无 `decode_with_version`。

- [ ] **Step 1：迁 `examples/smgp_server/src/main.rs`**

应用 T1（server import）+ T2（`SmgpBusinessHandler` → `MessageHandler`，`SmgpAdapter.decode` 删除、match `msg`、回执 `ctx.reply`）+ T4（main `.handlers` → `.message_handlers`）。对照样板 `examples/cmpp_server/src/main.rs`。

- [ ] **Step 2：迁 `examples/smgp_client/src/main.rs`**

应用 T1（client import + `NoopClientHandler`）+ T3（`SmgpClientHandler` → `MessageHandler`）+ T4（`ClientBuilder::new(...).with_message_handler(handler)`）。对照样板 `examples/cmpp_client/src/main.rs`。

- [ ] **Step 3：编译两个 example**

Run（WSL；包名以 `Cargo.toml` `name` 为准，多为 `smgp-server-example`/`smgp-client-example`）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo build -p smgp-server-example -p smgp-client-example 2>&1 | tail -20"
```
Expected：编译通过、无未用 import 告警（按提示清理）。

- [ ] **Step 4：迁 `tests/smgp/integration.rs`**

对本地 `TestBusinessHandler`（T2）、`TestClientHandler`（T3）、本地 `start_test_server`（T4：`.handlers` → `.message_handlers`、入参类型改 `Arc<dyn MessageHandler>`）、建连处 `ClientBuilder::new`（T4）应用变换。对照样板 `tests/cmpp/cmpp_test.rs`。**若某断言依赖旧裸帧行为，按窄腰统一消息字段做等价修正，不放宽断言强度。**

- [ ] **Step 5：跑 SMGP 集成测试**

Run（WSL）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test smgp-integration 2>&1 | grep -E 'test result|FAILED|error' | tail -20"
```
Expected：`test result: ok`，0 failed。

- [ ] **Step 6：clippy + commit**

Run：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy -p smgp-server-example -p smgp-client-example 2>&1 | grep -E 'warning|error' | tail -10"
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy -p rsms-tests --test smgp-integration 2>&1 | grep -E 'warning|error' | tail -10"
```
Expected：无告警。然后（Git Bash）：
```bash
git add examples/smgp_server/src/main.rs examples/smgp_client/src/main.rs tests/smgp/integration.rs
git commit -m "refactor(wp4-2): SMGP example + 集成测试迁移到 MessageHandler/ctx.reply"
```

---

## Task 2：SMGP — 压测（零丢失验收）

**Files:**
- Modify: `tests/smgp/stress_test.rs`（本地 `ServerHandler`/`ClientState`（decode 行 160）+ 本地 `start_test_server` 行 350–375）
- Modify: `tests/smgp/multi_account_stress_test.rs`（同构本地 handler + start_test_server）

**Interfaces:** Consumes Task 1 全部模式。Produces 无。验收：两压测零丢失。

**协议特有 delta（SMGP）：** 无多版本用例；`SubmitResp.msg_id` Binary(10B)；WARN 日志保持。

- [ ] **Step 1：迁 `tests/smgp/stress_test.rs`**

对本地 `ServerHandler`（T2）、`ClientState`（T3）、本地 `start_test_server`（T4）应用变换。对照样板 `tests/cmpp/stress_test.rs`（注意：CMPP 样板因 V2.0 留了旧路径，SMGP **无此问题**，服务端直接全迁 `.message_handlers`）。`.with_log_level(WARN)` 保持不动。

- [ ] **Step 2：迁 `tests/smgp/multi_account_stress_test.rs`**

同 Step 1，对照样板 `tests/cmpp/multi_account_stress_test.rs`。

- [ ] **Step 3：跑单连接/多连接压测（零丢失）**

Run（WSL，压测较慢，超时放宽）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test smgp-stress-test -- --nocapture 2>&1 | grep -E 'test result|TPS|丢失|loss|sent|recv|FAILED' | tail -30"
```
Expected：`test result: ok`、sent==recv（零丢失）。端口竞争偶发超时是已知 flaky（[[stress-test-port-flaky]]），单独重跑确认，不放宽断言。

- [ ] **Step 4：跑多账号压测（零丢失）**

Run（WSL）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test smgp-multi-account-stress-test -- --nocapture 2>&1 | grep -E 'test result|TPS|丢失|loss|sent|recv|FAILED' | tail -30"
```
Expected：`test result: ok`、零丢失。

- [ ] **Step 5：clippy + commit**

Run：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy -p rsms-tests --test smgp-stress-test --test smgp-multi-account-stress-test 2>&1 | grep -E 'warning|error' | tail -10"
```
Expected：无告警。然后（Git Bash）：
```bash
git add tests/smgp/stress_test.rs tests/smgp/multi_account_stress_test.rs
git commit -m "test(wp4-2): SMGP 压测迁移到 MessageHandler 并复验零丢失"
```

---

## Task 3：SMPP — examples + 集成测试

**Files:**
- Modify: `examples/smpp_server/src/main.rs`（`impl BusinessHandler for SmppBusinessHandler`，decode 行 344；main `.handlers(...)` 行 582–593）
- Modify: `examples/smpp_client/src/main.rs`（`impl ClientHandler for SmppClientHandler`，decode 行 253；`ClientBuilder::new` 行 332）
- Modify: `tests/smpp/integration.rs`（`TestBusinessHandler` decode 行 117；`TestClientHandler` decode 行 259；本地 `start_test_server` 行 295–322）

**Interfaces:** Consumes 并存桥 + Task 1 模式。Produces 无。

**协议特有 delta（SMPP）：**
- 客户端鉴权明文（`authenticator = PASSWORD.as_bytes().to_vec()`），`mode: BindMode::Transceiver`——属 T5，**不动**。
- `EndpointConfig` 须 `.with_protocol(Protocol::Smpp)`（**已设置**，迁移时确认别误删）。
- 集成测试覆盖 BindTransmitter + BindTransceiver 两个用例——两者都走统一模型，迁移方式相同。
- 无版本分支。

- [ ] **Step 1：迁 `examples/smpp_server/src/main.rs`**

应用 T1+T2+T4，对照 `examples/cmpp_server/src/main.rs`。确认 `.with_protocol(Protocol::Smpp)` 保留。

- [ ] **Step 2：迁 `examples/smpp_client/src/main.rs`**

应用 T1+T3+T4，对照 `examples/cmpp_client/src/main.rs`。确认明文鉴权与 `.with_protocol(Protocol::Smpp)` 保留。

- [ ] **Step 3：编译两个 example**

Run（WSL）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo build -p smpp-server-example -p smpp-client-example 2>&1 | tail -20"
```
Expected：编译通过、无未用 import 告警。

- [ ] **Step 4：迁 `tests/smpp/integration.rs`**

对 `TestBusinessHandler`（T2）、`TestClientHandler`（T3）、本地 `start_test_server`（T4）、建连处（T4）应用变换。对照 `tests/cmpp/cmpp_test.rs`。断言等价修正、不放宽。

- [ ] **Step 5：跑 SMPP 集成测试**

Run（WSL）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test smpp-integration 2>&1 | grep -E 'test result|FAILED|error' | tail -20"
```
Expected：`test result: ok`，0 failed（含 BindTransmitter + BindTransceiver 两场景）。

- [ ] **Step 6：clippy + commit**

Run：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy -p smpp-server-example -p smpp-client-example 2>&1 | grep -E 'warning|error' | tail -10"
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy -p rsms-tests --test smpp-integration 2>&1 | grep -E 'warning|error' | tail -10"
```
Expected：无告警。然后（Git Bash）：
```bash
git add examples/smpp_server/src/main.rs examples/smpp_client/src/main.rs tests/smpp/integration.rs
git commit -m "refactor(wp4-2): SMPP example + 集成测试迁移到 MessageHandler/ctx.reply"
```

---

## Task 4：SMPP — 压测（零丢失验收）

**Files:**
- Modify: `tests/smpp/stress_test.rs`（本地 `BusinessHandler`/`ClientHandler` + 本地 `start_test_server`）
- Modify: `tests/smpp/multi_account_stress_test.rs`

**Interfaces:** Consumes Task 3 模式。Produces 无。验收：两压测零丢失。

**协议特有 delta（SMPP）：** 无多版本；`.with_protocol(Protocol::Smpp)` + WARN 日志保持。

- [ ] **Step 1：迁 `tests/smpp/stress_test.rs`**

T2+T3+T4，对照 `tests/cmpp/stress_test.rs`（SMPP 无 V2.0 问题，服务端直接全迁）。`.with_protocol(Protocol::Smpp)`、`.with_log_level(WARN)` 保持。

- [ ] **Step 2：迁 `tests/smpp/multi_account_stress_test.rs`**

同 Step 1，对照 `tests/cmpp/multi_account_stress_test.rs`。

- [ ] **Step 3：跑压测（零丢失）**

Run（WSL）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test smpp-stress-test -- --nocapture 2>&1 | grep -E 'test result|TPS|丢失|loss|sent|recv|FAILED' | tail -30"
```
Expected：`test result: ok`、零丢失。

- [ ] **Step 4：跑多账号压测（零丢失）**

Run（WSL）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test smpp-multi-account-stress-test -- --nocapture 2>&1 | grep -E 'test result|TPS|丢失|loss|sent|recv|FAILED' | tail -30"
```
Expected：`test result: ok`、零丢失。

- [ ] **Step 5：clippy + commit**

Run：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy -p rsms-tests --test smpp-stress-test --test smpp-multi-account-stress-test 2>&1 | grep -E 'warning|error' | tail -10"
```
Expected：无告警。然后（Git Bash）：
```bash
git add tests/smpp/stress_test.rs tests/smpp/multi_account_stress_test.rs
git commit -m "test(wp4-2): SMPP 压测迁移到 MessageHandler 并复验零丢失"
```

---

## Task 5：SGIP — examples + 集成测试（含 4 个协议固有点）

**Files:**
- Modify: `examples/sgip_server/src/main.rs`（`impl BusinessHandler for SgipBusinessHandler`，decode 行 347；回执/`sequence_of` 行 408–412；main `.handlers(...)` 行 629–640）
- Modify: `examples/sgip_client/src/main.rs`（`impl ClientHandler for SgipClientHandler`，decode 行 330；`ClientBuilder::new` 行 444；Bind 用 `write_frame` 行 471 附近）
- Modify: `tests/sgip/integration.rs`（`TestBusinessHandler` decode 行 133；`TestClientHandler` decode 行 276；本地 `start_test_server` 行 313–340；send_bind 用 `write_frame` 行 445）

**Interfaces:** Consumes 并存桥 + 前序模式。Produces 无。

**协议特有 delta（SGIP，逐条必须保持）：**
1. **独立 Report 命令回 `ReportResp`（非 DeliverResp）**：server/client handler 收到 `UnifiedMessage::Report` 时回 `ctx.reply(UnifiedMessage::ReportResp).await?`（`ReportResp` 是 WP1 已加的统一变体，`SgipAdapter` 编码为独立 Report_Resp）。
2. **复合序列透传已验证 OK**：`ctx.reply` 内部 `adapter.encode(&msg, frame_sequence)`，而 `MessageContext` 构造时 `frame_sequence = SgipAdapter.sequence_of(frame)` 返回 `Sequence::Sgip{node_id,timestamp,number}`；`SgipAdapter.encode` 已正确处理 `Sequence::Sgip`（`crates/rsms-codec-sgip/src/adapter.rs:202-204` + 测试 `:441-455`）。故 SGIP 回执经 `ctx.reply` 自动回显复合序列，**无需特殊处理**。
3. **客户端 Bind 仍用 `conn.write_frame`（非 `send_request`）**：因 SGIP 复合序列偏移与 CMPP 不同——这在 main / 测试的 Bind 发送处，属 T5，**不改**。
4. **`SubmitResp` 的 `msg_id` 置空**：`MessageId::Text(String::new())`——回执构造照搬现状字段、只把发送改 `ctx.reply`。
5. **集成测试里"状态报告"用 `Deliver` 文本承载（历史约定，非独立 Report 命令）**：迁移时**保持现有 match 语义**（该 handler 分支收 `UnifiedMessage::Deliver` 处理报告文本、回 `ctx.reply(UnifiedMessage::DeliverResp)`），不要强行改成 Report/ReportResp。

- [ ] **Step 1：迁 `examples/sgip_server/src/main.rs`**

应用 T1+T2+T4，对照 `examples/cmpp_server/src/main.rs`。落实 delta-1（Report→`ctx.reply(ReportResp)`）、delta-4（SubmitResp msg_id 空）。确认 delta-3 的 Bind 发送方式与 `.with_protocol(Protocol::Sgip)` 保留。

- [ ] **Step 2：迁 `examples/sgip_client/src/main.rs`**

应用 T1+T3+T4，对照 `examples/cmpp_client/src/main.rs`。落实 delta-1（收 Report 回 `ctx.reply(ReportResp)`）。delta-3：client Bind 仍 `write_frame`（**不动**那段）。

- [ ] **Step 3：编译两个 example**

Run（WSL）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo build -p sgip-server-example -p sgip-client-example 2>&1 | tail -20"
```
Expected：编译通过、无未用 import 告警。

- [ ] **Step 4：迁 `tests/sgip/integration.rs`**

对 `TestBusinessHandler`（T2）、`TestClientHandler`（T3）、本地 `start_test_server`（T4）、建连处（T4）应用变换。**落实 delta-5**：集成测试的"状态报告"走 `Deliver` 文本承载，保持收 `Deliver`、回 `DeliverResp` 的现有语义，不改成 Report。send_bind 仍 `write_frame`（不动）。断言等价修正、不放宽。

- [ ] **Step 5：跑 SGIP 集成测试**

Run（WSL）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test sgip-integration 2>&1 | grep -E 'test result|FAILED|error' | tail -20"
```
Expected：`test result: ok`，0 failed。

- [ ] **Step 6：clippy + commit**

Run：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy -p sgip-server-example -p sgip-client-example 2>&1 | grep -E 'warning|error' | tail -10"
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy -p rsms-tests --test sgip-integration 2>&1 | grep -E 'warning|error' | tail -10"
```
Expected：无告警。然后（Git Bash）：
```bash
git add examples/sgip_server/src/main.rs examples/sgip_client/src/main.rs tests/sgip/integration.rs
git commit -m "refactor(wp4-2): SGIP example + 集成测试迁移到 MessageHandler/ctx.reply（保持 ReportResp/复合序列/Bind write_frame 语义）"
```

---

## Task 6：SGIP — 压测（零丢失验收）

**Files:**
- Modify: `tests/sgip/stress_test.rs`（本地 `BusinessHandler`/`ClientHandler` + 本地 `start_test_server`）
- Modify: `tests/sgip/multi_account_stress_test.rs`

**Interfaces:** Consumes Task 5 模式与 SGIP delta。Produces 无。验收：两压测零丢失。

**协议特有 delta（SGIP）：** 同 Task 5 的 delta-1/2/3/5 按压测 handler 实际收发的消息类型落实（压测若只发 Submit/收 SubmitResp + 报告，按其现状 match 分支等价迁移，报告应答类型与现状保持一致）；无多版本；WARN 日志保持。

- [ ] **Step 1：迁 `tests/sgip/stress_test.rs`**

T2+T3+T4，对照 `tests/cmpp/stress_test.rs`（SGIP 无 V2.0 问题，服务端直接全迁）。按该文件 handler 现状收发的消息类型落实 SGIP delta（回执类型与现状一致、复合序列经 `ctx.reply` 自动回显）。`.with_protocol(Protocol::Sgip)`、`.with_log_level(WARN)` 保持。

- [ ] **Step 2：迁 `tests/sgip/multi_account_stress_test.rs`**

同 Step 1，对照 `tests/cmpp/multi_account_stress_test.rs`。

- [ ] **Step 3：跑压测（零丢失）**

Run（WSL）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test sgip-stress-test -- --nocapture 2>&1 | grep -E 'test result|TPS|丢失|loss|sent|recv|FAILED' | tail -30"
```
Expected：`test result: ok`、零丢失。

- [ ] **Step 4：跑多账号压测（零丢失）**

Run（WSL）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test sgip-multi-account-stress-test -- --nocapture 2>&1 | grep -E 'test result|TPS|丢失|loss|sent|recv|FAILED' | tail -30"
```
Expected：`test result: ok`、零丢失。

- [ ] **Step 5：全工作区回归 + clippy（WP4-2 收口）**

Run（WSL）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test --workspace --lib 2>&1 | grep -E 'test result|error' | tail -20"
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy --workspace 2>&1 | grep -E 'warning|error' | tail -20"
```
Expected：全绿、clippy 零告警。未迁移的 longmsg/dynamic 等 target 仍走旧路径、应不受影响。

- [ ] **Step 6：commit**

（Git Bash）：
```bash
git add tests/sgip/stress_test.rs tests/sgip/multi_account_stress_test.rs
git commit -m "test(wp4-2): SGIP 压测迁移到 MessageHandler 并复验零丢失"
```

---

## Self-Review 检查（写计划后自查结论）

- **范围覆盖**：三协议 × (server example + client example + integration + 2 stress) 全覆盖（Task 1–6）；与 CMPP WP4-1 对称。longmsg/dynamic/unified-pilot 明确留 WP4-3。
- **统一变换 vs 重复代码**：本计划用「统一迁移变换 T1–T5 + 协议特有 delta + 精确文件/行号 + CMPP 活样板」替代逐文件复制代码——因这是「修改既有文件、有已合入样板」的机械迁移，变换规则确定无歧义，比抄一遍 12 份代码更可靠（样板不会过时）。
- **关键风险已前置核实**：SGIP 复合序列经 `ctx.reply` 透传——已实地核实 `SgipAdapter.encode` 支持 `Sequence::Sgip`（adapter.rs:202-204 + 测试），非阻塞。三协议均无版本偏差（grep 确认零 `decode_with_version`/`encode_with_version`）。
- **不碰框架/不碰 common**：本计划仅改 `examples/`、`tests/{smgp,smpp,sgip}/`；若 implementer 发现需改框架须停下上报（WP4-3 范畴）。
- **遗留风险（执行时留意）**：① example 包名以各 `Cargo.toml` `name` 为准（多带 `-example` 后缀）；② 压测端口 flaky 单独重跑；③ SGIP delta-5（integration 报告走 Deliver）易被误改成 Report，Task 5 已显式标注。

## 执行交接

计划已存 `docs/superpowers/plans/2026-06-28-wp4-2-other-protocols.md`。两种执行方式：
1. **Subagent-Driven（推荐）**：每 task 派新 subagent + 两段式评审；动压测的 task 必实跑对应 `*-stress-test`/`*-multi-account-stress-test`。
2. **Inline**：本 session 内按 executing-plans 批量执行 + 检查点评审。

关联记忆：[[onboarding-ergonomics-reshape]]、[[git-remote-via-wsl]]、[[stress-test-port-flaky]]、[[java-interop-stress-4proto]]。
