# WP4-3b（全迁遗留 target 到窄腰新路径）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: 用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐 task 执行。步骤用 checkbox（`- [ ]`）跟踪。
> **语言要求**：全程思考与输出用中文（仅代码英文关键词除外）。

**Goal:** 把所有仍走旧路径（`impl BusinessHandler`/`impl ClientHandler` + `.handlers()`）的 test target 迁到窄腰新路径（`MessageHandler` + `ctx.reply` + `ClientBuilder::with_message_handler`），并清掉孤儿死代码与空 `.handlers()` 调用，使全仓 `tests/` 零残留旧 trait impl 与旧 setter——为 WP4-3c 删并存桥/退役旧 trait 扫清前置。

**Architecture:** 无框架改动（框架 D1a/D1b/D3b 已就绪、decode/encode 双向版本感知 + 心跳自动回）。本计划只做 `tests/` 的机械迁移，复用 WP4-2 已验证的「统一迁移变换 T1–T5」（见下「迁移变换」节）。**CMPP V2.0/V3.0 版本差异现由框架 `ctx.reply` + 新路径 `decode_with_version` 自动处理**——迁移后删去 test 里所有手动 `decode_with_version`/`encode_with_version`/version 分支。

**Tech Stack:** Rust edition 2024；`rsms-business`（`MessageHandler`/`MessageContext`/`ctx.reply`）；`rsms-connector`（`ServerBuilder::message_handlers`、`ClientBuilder::with_message_handler`、`NoopClientHandler`）；各 `rsms-codec-*`。

## Global Constraints

- **允许 breaking、无需向后兼容**（项目 0.0.1 未发布）。
- **不改框架代码**（`crates/`）：本计划仅改 `tests/`。若发现需改框架 → 停下上报（属 3c 或 3a-followup）。
- **clippy 零告警**：`cargo clippy --workspace` warning-free。
- **cargo 走 WSL**（`RUSTFLAGS='--cap-lints allow'`）；**commit 走 Git Bash**（[[git-remote-via-wsl]]）。
- **压测 WARN 日志、零丢失为验收线**；端口 flaky 单独重跑（[[stress-test-port-flaky]]）。
- **断言不放宽**：迁移中若断言依赖旧裸帧行为，按统一消息字段等价修正，不降断言强度。
- **本计划允许并必须触 `tests/common/`**（删孤儿 `common/src/server.rs`）——这与 WP4-2「不碰 common」相反，是 WP4-3b 的明确授权（3c 要删 `.handlers()` setter，孤儿必须先清）。

## 迁移变换（复用 WP4-2 plan 的 T1–T5，所有迁移 task 共用）

> 完整定义见 `docs/superpowers/plans/2026-06-28-wp4-2-other-protocols.md` 的「统一迁移变换」节。活样板：`tests/cmpp/cmpp_test.rs`（已迁新路径，含 server `start_test_server` 用 `.message_handlers`、client 用 `ClientBuilder::new(.., NoopClientHandler, ..).with_message_handler`）。

- **T1（import）**：删 `use ...::{BusinessHandler, InboundContext}` / `use rsms_connector::client::{ClientContext, ClientHandler}`；加 `use rsms_business::{MessageContext, MessageHandler}`，`NoopClientHandler` 并入既有 `use rsms_connector::{...}`。按编译器提示清不再用的 import（`Frame`/`InboundContext`/`ClientContext`/`ClientHandler`/`BusinessHandler`），但 adapter/decoder/鉴权助手/MessageSource 用到的保留。
- **T2（server handler）**：`impl BusinessHandler for X` → `impl MessageHandler for X`；`async fn on_inbound(&self, ctx: &InboundContext, frame: &Frame)` → `async fn on_message(&self, ctx: &MessageContext, msg: &UnifiedMessage)`；删函数体内 `XxxAdapter.decode(frame)`（框架已解码，直接 `match msg`）；回执「`XxxAdapter.encode(..) + ctx.conn.write_frame`」→「`ctx.reply(resp)`」；借用调整（`msg` 是借用，`match` 出 `&UnifiedSubmit` 等，owned move 改借用/`.clone()`）。
- **T3（client handler）**：`impl ClientHandler for X` → `impl MessageHandler for X`；`on_inbound(ctx: &ClientContext, frame)` → `on_message(ctx: &MessageContext, msg)`；删 `XxxAdapter.decode(frame)`；回执 → `ctx.reply`；删迁移后空置的回执辅助函数。
- **T4（builder 调用点）**：server `ServerBuilder::new(cfg).handlers(vec![biz])` → `.message_handlers(vec![biz])`，本地 `start_test_server` 入参类型 `Arc<dyn BusinessHandler>` → `Arc<dyn MessageHandler>`；client `ClientBuilder::new(endpoint, handler, Decoder)` → `ClientBuilder::new(endpoint, Arc::new(NoopClientHandler), Decoder).with_message_handler(handler)`。
- **T5（保持不变）**：`MessageSource`、出站 PDU 编码、鉴权构造、Bind 发送方式、`EndpointConfig`（含 `.with_protocol(...)`、`.with_log_level(WARN)`）一律不改。
- **★WP4-3b 增量 T6（CMPP 版本去手动化）**：CMPP target 里所有**手动**版本处理——`CmppAdapter.decode_with_version(frame, ...)`、`CmppAdapter.encode_with_version(.., CmppVersion::V20)`、`按 version 的 if/else 分支`——迁移后**全删**：框架新路径 decode 已按 `conn.protocol_version()` 版本感知（WP4-3a Task2/3），`ctx.reply` 已版本感知 encode（D1b）。client endpoint 仍须 `.with_protocol(Protocol::Cmpp)`，V2.0 握手仍发 version=0x20 的 Connect（T5 不动握手构造）。

---

## Task 1：四协议 dynamic-connection（最简，client-only）

**Files:**
- Modify: `tests/cmpp/dynamic_connection_test.rs`（`impl ClientHandler for TestClientHandler` :86；server `.handlers(vec![])` :153）
- Modify: `tests/smgp/dynamic_connection_test.rs`（CH :84；`.handlers(vec![])` :146）
- Modify: `tests/smpp/dynamic_connection_test.rs`（CH :82；`.handlers(vec![])` :138）
- Modify: `tests/sgip/dynamic_connection_test.rs`（CH :84；`.handlers(vec![])` :150）

**Interfaces:** Consumes 并存桥新路径。Produces 无。
**特点**：服务端 `.handlers(vec![])` 空（仅测鉴权/动态连接），无 server 业务 handler；客户端 `TestClientHandler` 逻辑极少（多为统计 SubmitResp）。无版本分支。

- [ ] **Step 1：迁四个文件**

每个文件应用 T1 + T3（`TestClientHandler` → `MessageHandler`）+ T4（client `ClientBuilder::new(endpoint, Arc::new(NoopClientHandler), XxxDecoder).with_message_handler(handler)`；server `.handlers(vec![])` → `.message_handlers(vec![])`）。SMPP 确认 `.with_protocol(Protocol::Smpp)`、SGIP `.with_protocol(Protocol::Sgip)` 保留。对照样板 `tests/cmpp/cmpp_test.rs` 的 client 建连写法。

- [ ] **Step 2：跑四个 dynamic 测试**

Run（WSL，逐条或合并）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test cmpp-dynamic-connection-test --test smgp-dynamic-connection-test --test smpp-dynamic-connection-test --test sgip-dynamic-connection-test 2>&1 | grep -E 'test result|FAILED|error' | tail -20"
```
Expected：四个均 `test result: ok`。

- [ ] **Step 3：clippy + commit**

```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy -p rsms-tests --test cmpp-dynamic-connection-test --test smgp-dynamic-connection-test --test smpp-dynamic-connection-test --test sgip-dynamic-connection-test 2>&1 | grep -E 'warning:|error' | tail -10"
```
Expected：无告警。然后（Git Bash）：
```bash
git add tests/cmpp/dynamic_connection_test.rs tests/smgp/dynamic_connection_test.rs tests/smpp/dynamic_connection_test.rs tests/sgip/dynamic_connection_test.rs
git commit -m "refactor(wp4-3b): 四协议 dynamic-connection 测试迁移到 MessageHandler"
```

---

## Task 2：SMGP/SMPP/SGIP longmsg（无版本，server+client）

**Files:**
- Modify: `tests/smgp/smgp_longmsg_test.rs`（BusinessHandler :99；ClientHandler :198；`.handlers` :238）
- Modify: `tests/smpp/smpp_longmsg_test.rs`（BH :123；CH :227；`.handlers` :335）
- Modify: `tests/sgip/sgip_longmsg_test.rs`（BH :118；CH :223；`.handlers` :324）

**Interfaces:** Consumes 并存桥。Produces 无。单版本/版本透明，无 version 分支。SGIP 注意保持 ReportResp/复合序列/Bind write_frame 语义（与 WP4-2 Task5 一致——`ctx.reply` 自动回显复合序列）。

- [ ] **Step 1：迁三个文件**

每文件应用 T1 + T2（server `LongMsgBizHandler` → MessageHandler、回执 `ctx.reply`）+ T3（client `LongMsgClientHandler` → MessageHandler）+ T4。长短信 split/merge（`LongMessageSplitter`/`Merger` + concat 模型）属 T5 不动。SGIP 收 `UnifiedMessage::Report` 回 `ctx.reply(UnifiedMessage::ReportResp)`、收 Deliver 回 DeliverResp（保持现状语义）。

- [ ] **Step 2：跑三个 longmsg 测试**

```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test smgp-longmsg-test --test smpp-longmsg-test --test sgip-longmsg-test -- --nocapture 2>&1 | grep -E 'test result|FAILED|error' | tail -20"
```
Expected：三个均 `test result: ok`（长短信合包零丢字）。

- [ ] **Step 3：clippy + commit**

```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy -p rsms-tests --test smgp-longmsg-test --test smpp-longmsg-test --test sgip-longmsg-test 2>&1 | grep -E 'warning:|error' | tail -10"
```
Expected：无告警。然后（Git Bash）：
```bash
git add tests/smgp/smgp_longmsg_test.rs tests/smpp/smpp_longmsg_test.rs tests/sgip/sgip_longmsg_test.rs
git commit -m "refactor(wp4-3b): SMGP/SMPP/SGIP longmsg 测试迁移到 MessageHandler"
```

---

## Task 3：CMPP longmsg（含 V2.0/V3.0，版本去手动化）

**Files:**
- Modify: `tests/cmpp/cmpp_longmsg_test.rs`（BusinessHandler :103；server on_inbound 按 `handler.version` 分支 V2.0 `decode_message_with_version(.., Some(0x20))` / V3.0 `decode_message()`；ClientHandler :239；client on_inbound 用 `CmppAdapter.decode(frame)` 无版本；`.handlers` :296）

**Interfaces:** Consumes 并存桥 + 框架版本感知（D1a/D1b）。Produces 无。**同时测 V2.0 和 V3.0**。

- [ ] **Step 1：迁 cmpp_longmsg_test.rs**

应用 T1 + T2 + T3 + T4 + **T6（版本去手动化）**：
- server `LongMsgBizHandler`：`impl MessageHandler`，删 `按 version 的 decode_message_with_version/decode_message` 分支——框架新路径已按 `conn.protocol_version()` 解码，直接 `match msg`；回执 `ctx.reply`（V2.0 连接自动回 V2.0，D1b）。
- client `LongMsgClientHandler`：`impl MessageHandler`，删 `CmppAdapter.decode(frame)`，`match msg`；回执 `ctx.reply`。
- T5 保持：客户端 V2.0 握手仍发 version=0x20 的 Connect（`build_connect_pdu` 的 `version: self.version` 不动）；`.with_protocol(Protocol::Cmpp)` 保留。
- **若 handler 结构体仍需 `version` 字段**（用于决定握手/构造 Submit 的版本）保留；仅删「用 version 选 decode/encode 路径」的逻辑。

- [ ] **Step 2：跑 cmpp-longmsg 测试（V2.0+V3.0）**

```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test cmpp-longmsg-test -- --nocapture 2>&1 | grep -E 'test result|FAILED|error' | tail -20"
```
Expected：`test result: ok`，V2.0 与 V3.0 长短信用例全过（框架版本感知 + ctx.reply 正确处理两版本）。

- [ ] **Step 3：clippy + commit**

```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy -p rsms-tests --test cmpp-longmsg-test 2>&1 | grep -E 'warning:|error' | tail -10"
```
Expected：无告警（尤其确认 `decode_with_version`/`encode_with_version` 手动调用与 `CmppVersion` 等 import 若不再用已删）。然后（Git Bash）：
```bash
git add tests/cmpp/cmpp_longmsg_test.rs
git commit -m "refactor(wp4-3b): CMPP longmsg 测试迁移到 MessageHandler（V2.0/V3.0 版本去手动化）"
```

---

## Task 4：CMPP transaction（server+client，纯 V3.0）

**Files:**
- Modify: `tests/cmpp/transaction_integration_test.rs`（ClientHandler `TransactionTestHandler` :50；BusinessHandler `TestBusinessHandler` :151，手写 `ctx.conn.write_frame(&resp_bytes)` :165；`.handlers(vec![biz_handler])` :223）

**Interfaces:** Consumes 并存桥。Produces 无。纯 V3.0，无版本分支；客户端走 `TransactionManager`（事务匹配 seq↔msg_id）——TM 逻辑属业务流程，**只迁 handler 的解码/回执方式，不动 TM 调用语义**。

- [ ] **Step 1：迁 transaction_integration_test.rs**

应用 T1 + T2（`TestBusinessHandler` → MessageHandler，手写 write_frame → `ctx.reply`）+ T3（`TransactionTestHandler` → MessageHandler）+ T4。客户端若在 `on_inbound` 里驱动 `TransactionManager`（如 `tm.on_response(...)`），把驱动逻辑搬到 `on_message` 对应 `match msg` 分支、用 `msg` 而非手动 decode 的结果。**断言（事务匹配数、msg_id 关联）不放宽**。

- [ ] **Step 2：跑 transaction 测试**

```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test cmpp-transaction-test 2>&1 | grep -E 'test result|FAILED|error' | tail -20"
```
Expected：`test result: ok`。

- [ ] **Step 3：clippy + commit**

```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy -p rsms-tests --test cmpp-transaction-test 2>&1 | grep -E 'warning:|error' | tail -10"
```
Expected：无告警。然后（Git Bash）：
```bash
git add tests/cmpp/transaction_integration_test.rs
git commit -m "refactor(wp4-3b): CMPP transaction 测试迁移到 MessageHandler"
```

---

## Task 5：cmpp20（纯 V2.0，client-only）——并修复 2 个预存失败

**Files:**
- Modify: `tests/cmpp/cmpp20_test.rs`（ClientHandler `TestClientHandler` :161，on_inbound 用 `CmppAdapter.decode(frame)` 无版本；server `.handlers(vec![])` :250）

**Interfaces:** Consumes 并存桥 + 框架 V2.0 版本感知（D1a/D1b）。Produces 无。
**★背景**：cmpp20-test 现有 2 个 V2.0 用例（`test_connect_v20_version`/`test_submit_v20_after_connect`）**预存失败**——根因正是旧 ClientHandler 路径客户端 `CmppAdapter.decode`（无版本）误按 V3.0 解 V2.0 应答。迁到新路径后，框架按 `conn.protocol_version()=Some(0x20)` 解码（WP4-3a Task3），**这 2 个用例应转为 PASS**。这是本 task 的核心验收信号。

- [ ] **Step 1：迁 cmpp20_test.rs**

应用 T1 + T3（`TestClientHandler` → MessageHandler，删 `CmppAdapter.decode(frame)`、`match msg`，回执 `ctx.reply`）+ T4（client `ClientBuilder::new(endpoint, Arc::new(NoopClientHandler), CmppDecoder).with_message_handler(handler)`；server `.handlers(vec![])` → `.message_handlers(vec![])`）+ **T6**（删任何手动 `decode_with_version`）。T5 保持：V2.0 握手发 version=0x20 的 Connect、`.with_protocol(Protocol::Cmpp)`。`TestClientHandler` 的 `version`/`next_seq` 等用于构造发包的字段保留。

- [ ] **Step 2：跑 cmpp20 测试（重点：2 个预存失败转 PASS）**

```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test cmpp20-test 2>&1 | grep -E 'test result|FAILED|test_connect_v20|test_submit_v20|error' | tail -20"
```
Expected：`test result: ok`，**8 passed; 0 failed**（此前是 6 passed/2 failed；`test_connect_v20_version` 与 `test_submit_v20_after_connect` 现应通过）。若仍失败，说明迁移未真正走上版本感知新路径，须排查（不是放宽断言）。

- [ ] **Step 3：clippy + commit**

```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy -p rsms-tests --test cmpp20-test 2>&1 | grep -E 'warning:|error' | tail -10"
```
Expected：无告警。然后（Git Bash）：
```bash
git add tests/cmpp/cmpp20_test.rs
git commit -m "refactor(wp4-3b): cmpp20 测试迁移到 MessageHandler（修 2 个 V2.0 预存失败——新路径版本感知）"
```

---

## Task 6：cmpp-stress（V2.0/V3.0 双 impl 收敛，最复杂）

**Files:**
- Modify: `tests/cmpp/stress_test.rs`（ClientHandler `ClientState` :118 + MessageHandler `ClientState` :234 双 impl；server `ServerHandler` impl BusinessHandler ~:334、手写 write_frame :325、`.handlers(vec![biz_handler])` :393；`run_stress_test` 版本分支 :617——V3.0 已新路径 :618、V2.0 旧路径 :626）

**Interfaces:** Consumes 并存桥 + 框架 V2.0/V3.0 版本感知。Produces 无。验收：cmpp-stress 零丢失。

- [ ] **Step 1：收敛 ClientState 为单一 MessageHandler**

删 `impl ClientHandler for ClientState`（:118-~183，含其 `CmppVersion::from_wire + decode_with_version` 版本分支）；保留/统一 `impl MessageHandler for ClientState`（:234），其 `on_message` 直接 `match msg`（框架已按版本解码）。`ClientState` 的 `version` 字段若仅用于旧 ClientHandler 的解码分支则删；若还用于构造发包/握手则保留。

- [ ] **Step 2：迁 server ServerHandler + 统一建连分支**

`ServerHandler` `impl BusinessHandler` → `impl MessageHandler`，手写 write_frame → `ctx.reply`（V2.0 自动回 V2.0，D1b）；`start_test_server` 的 `.handlers(vec![biz_handler])`（:393）→ `.message_handlers(...)`、入参类型改 `Arc<dyn MessageHandler>`。`run_stress_test` 的版本分支（:617-626）统一为**单一新路径**：V2.0 与 V3.0 都用 `ClientBuilder::new(endpoint, Arc::new(NoopClientHandler), CmppDecoder).with_message_handler(client_state)`，仅 endpoint 的握手版本不同（T5）。删 V2.0 的旧 `ClientBuilder::new(endpoint, client_state, ..)`（:626）。

- [ ] **Step 3：跑 cmpp-stress（V2.0+V3.0 零丢失）**

```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test cmpp-stress-test -- --nocapture 2>&1 | grep -E 'test result|TPS|unmatched|丢失|loss|FAILED' | tail -30"
```
Expected：`test result: ok`、零丢失（V2.0 与 V3.0 压测用例均 sent==recv）。端口 flaky 单独重跑。

- [ ] **Step 4：clippy + commit**

```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy -p rsms-tests --test cmpp-stress-test 2>&1 | grep -E 'warning:|error' | tail -10"
```
Expected：无告警（确认 `impl ClientHandler` 已删、`decode_with_version`/`CmppVersion`/`ClientHandler`/`BusinessHandler` import 若不再用已清）。然后（Git Bash）：
```bash
git add tests/cmpp/stress_test.rs
git commit -m "refactor(wp4-3b): cmpp-stress V2.0/V3.0 双 impl 收敛为单一 MessageHandler 并复验零丢失"
```

---

## Task 7：清孤儿死代码 + 空 handler 换新 setter

**Files:**
- Delete: `tests/cmpp/cmpp_test_server.rs`（孤儿 `impl BusinessHandler for TestBusinessHandler` :156，经确认无 target include/use）
- Delete: `tests/common/src/server.rs`（孤儿 `start_test_server` 用 `.handlers()` :24，经确认无 target use）
- Modify: `tests/cmpp/soak_test.rs`（`.handlers(vec![])` :120）
- Modify: `tests/cmpp/fault_injection_test.rs`（`.handlers(vec![])` :93）
- Modify: `tests/cmpp/soak_dynamic_test.rs`（`.handlers(vec![])` :115）
- Modify: `tests/cmpp/network_fault_test.rs`（`.handlers(vec![])` :176）
- Modify: `tests/common/src/lib.rs`（删 `pub mod server;` 之类对 server.rs 的声明，若有）

**Interfaces:** Consumes 无。Produces 无。

- [ ] **Step 1：确认孤儿零引用再删**

Run（Git Bash）：
```bash
grep -rn "cmpp_test_server\|mod cmpp_test_server\|include!.*cmpp_test_server" tests/ || echo "cmpp_test_server 零引用"
grep -rn "rsms_test_common::start_test_server\|common::server\|server::start_test_server" tests/ || echo "common server 零引用"
grep -rn "pub mod server\|mod server" tests/common/src/lib.rs || echo "lib.rs 无 server 声明"
```
Expected：确认两孤儿零引用。若 `tests/common/src/lib.rs` 有 `pub mod server;` 则需一并删该行。删除两文件：
```bash
git rm tests/cmpp/cmpp_test_server.rs tests/common/src/server.rs
```
> 若 grep 发现**有**引用（与勘探相反），**停止删除、改为按 T2/T4 迁移该文件**并上报。

- [ ] **Step 2：四个空 handler target 换 setter**

`soak_test.rs:120`、`fault_injection_test.rs:93`、`soak_dynamic_test.rs:115`、`network_fault_test.rs:176` 的 `.handlers(vec![])` → `.message_handlers(vec![])`（这些是连接/压力/故障注入测试、无业务 handler，换 setter 仅为去除将被 3c 删除的旧 API 调用）。

- [ ] **Step 3：编译 + 跑这四个 target**

```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test cmpp-soak-test --test cmpp-fault-injection-test --test cmpp-soak-dynamic-test --test cmpp-network-fault-test 2>&1 | grep -E 'test result|FAILED|error' | tail -20"
```
Expected：四个均 `test result: ok`（删孤儿 + 换 setter 后编译通过、行为不变）。
> 注：soak/soak-dynamic 可能耗时较长，若超时可单独跑或确认其本身设计运行时长。

- [ ] **Step 4：clippy + commit**

```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy -p rsms-tests 2>&1 | grep -E 'warning:|error' | tail -10"
```
Expected：无告警。然后（Git Bash）：
```bash
git add -A tests/
git commit -m "chore(wp4-3b): 删孤儿 cmpp_test_server/common server + 四 target 空 .handlers→.message_handlers"
```

---

## Task 8：WP4-3b 收口——零残留门禁 + 全量回归 + 四协议压测零丢失

**Files:** 无（纯验证）。

**Interfaces:** Consumes Task 1–7。Produces：进 WP4-3c 的门禁证据。

- [ ] **Step 1：★零残留门禁 grep**

Run（Git Bash）：
```bash
echo "=== impl BusinessHandler/ClientHandler 残留（期望仅可能的 common 定义或零） ==="
grep -rn "impl .*BusinessHandler\|impl .*ClientHandler" tests/ --include="*.rs"
echo "=== .handlers( 残留（期望零，除 message_handlers） ==="
grep -rn "\.handlers(" tests/ --include="*.rs" | grep -v "message_handlers"
```
Expected：**两条均零输出**（全 `tests/` 无 `impl BusinessHandler`/`impl ClientHandler`、无旧 `.handlers()` 调用）。若有残留，回对应 task 补迁。

- [ ] **Step 2：全工作区 lib + clippy + 全 integration**

```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test --workspace --lib 2>&1 | grep -E 'test result|error' | tail -20"
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy --workspace 2>&1 | grep -E 'warning:|error' | tail -20"
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test cmpp-integration --test cmpp20-test --test smgp-integration --test smpp-integration --test sgip-integration --test cmpp-longmsg-test --test smgp-longmsg-test --test smpp-longmsg-test --test sgip-longmsg-test --test cmpp-transaction-test 2>&1 | grep -E 'test result|FAILED|error' | tail -30"
```
Expected：全绿（**cmpp20-test 现 8/8**）、clippy 净。

- [ ] **Step 3：四协议 multi-account 压测零丢失**

```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && echo '===cmpp===' && cargo test -p rsms-tests --test cmpp-multi-account-stress-test -- --nocapture 2>&1 | grep -E 'test result|unmatched|FAILED' | tail -6 && echo '===smgp===' && cargo test -p rsms-tests --test smgp-multi-account-stress-test -- --nocapture 2>&1 | grep -E 'test result|unmatched|FAILED' | tail -6 && echo '===smpp===' && cargo test -p rsms-tests --test smpp-multi-account-stress-test -- --nocapture 2>&1 | grep -E 'test result|unmatched|FAILED' | tail -6 && echo '===sgip===' && cargo test -p rsms-tests --test sgip-multi-account-stress-test -- --nocapture 2>&1 | grep -E 'test result|unmatched|FAILED' | tail -6"
```
> 注：用字面 target 名链式、勿用 `for` 循环变量（嵌套 wsl 引号下 `${t}` 不展开）。压测时长由各文件 `STRESS_TEST_DURATION_SECS` 决定（标准 300s；如需快速可临时改小后还原、不提交）。
Expected：四协议均 `unmatched: 0` 零丢失。

- [ ] **Step 4：更新 ledger**

在 `.superpowers/sdd/progress.md` 追加 WP4-3b 完成行 + 零残留门禁证据；更新记忆 [[onboarding-ergonomics-reshape]]（3b 完成、cmpp20 预存失败已修、下一步 3c）。

---

## Self-Review（计划自查结论）

- **范围覆盖**：权威 grep 清出的全部旧路径残留——4 dynamic（T1）+ 3 非 CMPP longmsg（T2）+ CMPP longmsg（T3）+ transaction（T4）+ cmpp20（T5）+ cmpp-stress（T6）+ 2 孤儿 + 4 空 handler（T7）——Task 8 零残留 grep 兜底。smgp-unified-pilot 经勘探已是新路径、不在范围。
- **占位符**：迁移逻辑复用 WP4-2 已验证的 T1–T5（文档化变换 + 活样板 cmpp_test.rs）+ 精确 file:line + WP4-3b 增量 T6（CMPP 版本去手动化）。删除项先 grep 确认零引用（T7 Step1 有兜底「若有引用则改迁移」）。
- **类型一致**：四协议 `ClientBuilder::new(endpoint, Arc::new(NoopClientHandler), XxxDecoder).with_message_handler(handler)` 形式统一；`ctx.reply(UnifiedMessage::XxxResp)` 一致。
- **关键风险**：① cmpp20 的 2 预存失败转 PASS 是 Task5 硬验收信号（非放宽）；② cmpp-stress 双 impl 收敛是最复杂点（Task6 单列、必跑零丢失）；③ 孤儿删除前 grep 兜底防误删。
- **门禁**：Task8 的「grep 零 impl + 零 .handlers()」是进 WP4-3c 的硬前置。

## 执行交接

计划存 `docs/superpowers/plans/2026-06-29-wp4-3b-migrate-legacy-targets.md`。推荐 **Subagent-Driven**：每 task 派新 subagent + 两段式评审；动压测的 Task6/8 必实跑对应压测零丢失；Task5 必确认 cmpp20 转 8/8。关联记忆：[[onboarding-ergonomics-reshape]]、[[git-remote-via-wsl]]、[[stress-test-port-flaky]]、[[java-interop-stress-4proto]]。
