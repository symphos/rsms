# WP4-3 设计：退役并存桥、内化版本感知与心跳、统一主路径

> 「对接易用性重塑」收口工作包。本 spec 固化 WP4-3 的拆分、已拍板设计决策、阶段门禁与验收，供逐子包 writing-plans 落地。
> **语言要求**：全程思考与输出用中文（仅代码英文关键词除外，见 AGENTS.md / CLAUDE.md）。

## 1. 背景与目标

WP4-1（CMPP）/ WP4-2（SMGP/SMPP/SGIP）已把四协议主链路的 example / 集成测试 / 压测迁到窄腰新路径（`MessageHandler` + `ctx.reply`），但**框架仍是双路径并存**：服务端 `run_connection` 按 `message_handlers.is_empty()` 在新旧链间择路，客户端 `ClientBuilder` 仍强制旧 `ClientHandler` 占位参数。一批 test target（`cmpp20_test`、`stress_test` 服务端侧、四协议 `*-longmsg-test`/`*-dynamic-connection-test`、`smgp-unified-pilot`）也仍走旧路径，由并存桥保护工作。

**WP4-3 目标**：把窄腰新路径转为**唯一**路径——内化版本感知与心跳应答到框架，把全部遗留 target 迁到新 trait，最后删并存桥、退役旧 trait，使框架只剩一条协议无关的统一主路径。

**允许 breaking、无需向后兼容**（项目 0.0.1 未发布、无外部接入方）。

## 2. 现状（勘探坐实）

### 2.1 服务端并存桥
- `crates/rsms-connector/src/connection.rs`
  - `run_connection` 签名同时收 `handlers: Vec<Arc<dyn BusinessHandler>>` 与 `message_handlers: Vec<Arc<dyn MessageHandler>>`（~`:335`）。
  - `:442` `if message_handlers.is_empty()` → 旧路径 `run_chain()`（`:443-447`）；否则新路径 `adapter.decode → MessageContext → run_message_chain()`（`:448-475`）。
- `crates/rsms-connector/src/server.rs`：`ServerBuilder`/`BoundServer` 各持 `handlers` + `message_handlers` 两字段；`.handler()/.handlers()`（旧，`:70-79`）与 `.message_handler()/.message_handlers()`（新，`:83-92`）；`run()` 克隆两列表传入（`:198-199`）。

### 2.2 客户端并存桥
- `crates/rsms-connector/src/client.rs`
  - `run_client_read_loop` 收 `client_handler` + `message_handler: Option<Arc<dyn MessageHandler>>`（~`:840`）；`:963` `if let Some(mh)` → 新路径 `adapter.decode → mh.on_message()`（`:964-981`，**当前未传 version**）；否则旧路径 `ClientContext → client_handler.on_inbound()`（`:982-990`）。
  - `ClientBuilder`（`:633-642`）：`new(endpoint, client_handler, decoder)` 强制旧 `ClientHandler` 第二参（`:646-662`）+ `.with_message_handler()`（`:695-697`）。

### 2.3 旧 trait 分布
| 类型 | 定义 | 说明 |
|---|---|---|
| `BusinessHandler` / `InboundContext` / `run_chain` | `rsms-business/src/lib.rs`（~`:31-67` / `:13-24` / `:101-116`） | 旧入站业务链 |
| `ClientHandler` / `ClientContext` / `NoopClientHandler` | `rsms-connector/src/client.rs`（~`:606-609` / `:600-603` / `:613-623`） | 旧客户端入站 + 占位符 |

外部仍 impl 旧 trait 的 target（即仍走旧路径、3b 须迁）：`cmpp20_test.rs`、`stress_test.rs`（`ClientState`，且其 `on_inbound` 内自调 `decode_with_version(V20)`）、四协议 `*-longmsg-test`/`*-dynamic-connection-test`、`smgp-unified-pilot`。已迁新路径的：四协议 `*-integration`、`*-stress-test` 客户端侧（CMPP 除外）、`*-multi-account-stress-test`。

### 2.4 版本感知 decode
- `ProtocolAdapter` 仅 `decode(&Frame) -> UnifiedMessage`，无版本参数。
- `CmppAdapter::decode_with_version(frame, CmppVersion)` 已存在（`crates/rsms-codec-cmpp/src/adapter.rs:495`）；底层 `decode_message_with_version()` 按版本分叉字段宽度。SMGP/SMPP/SGIP 单版本或版本透明，无版本感知 decode。
- 握手版本由 `conn.protocol_version() -> Option<u8>`（connection.rs:99 / client.rs:128）提供（CMPP 2.0=`0x20`、3.0=`0x30`）。
- **服务端** CMPP `handle_frame` 已用 `decode_message_with_version(version)`；**客户端新路径** `adapter.decode()` 未传 version → V2.0 会按 V3.0 误解（真 bug）。

### 2.5 心跳应答
- `UnifiedMessage::Ping` / `PingResp` 已存在（`rsms-model/src/message.rs:22-23`）。
- 服务端 `handle_frame`：SMGP（`handlers/smgp.rs:119`）/ SMPP（`handlers/smpp.rs:150`）**已框架自动回** ActiveTestResp/EnquireLinkResp 返回 `Continue`；CMPP ActiveTest 无特殊处理 → 落业务链；SGIP 无心跳。
- SMGP/SMPP 服务端心跳已经过真机联调验证（见 [[smgp-keepalive-close-13b-bug]] 等）。

## 3. 已拍板设计决策（2026-06-29）

- **D-切分**：拆 3 子包 3a/3b/3c，门禁串联（§4）。每子包独立 spec→plan→评审，延续 WP4-1/4-2 节奏。
- **D1a 版本感知内化**：`ProtocolAdapter` 加 `decode_with_version(&self, frame, version: Option<u8>) -> Result<UnifiedMessage>`，**默认实现转调 `decode`**；仅 CMPP override 转 `CmppAdapter::decode_with_version`。服务端新路径与客户端新路径的解码调用点均改传 `conn.protocol_version()`。
- **D1b 版本感知编码内化（WP4-3a-followup，2026-06-29 追加）**：D1a 的 encode 镜像。`ProtocolAdapter` 加 `encode_with_version(&self, msg, seq, version: Option<u8>) -> Result<Vec<u8>>`，**默认转调 `encode`**，仅 CMPP override（`CmppVersion::from_wire` 映射 + `unified_to_cmpp_with_version`）；`MessageContext::reply` 改传 `conn.protocol_version()`。使 V2.0 服务端 `ctx.reply` 自动回 V2.0 应答，消除手动 `encode_with_version` 特例。WP4-3a scope 从「仅 decode 版本感知」扩为「decode+encode 对称」。**前置于 WP4-3b**：3b 迁 cmpp20_test/stress 可统一用 `ctx.reply`、不留手动 encode 特例。零回归命脉=V3.0/单版本 `encode_with_version(None/Some(0x30))` 逐字节等于 `encode`。
- **D3b 心跳收归（handle_frame 统一，偏离原设计笔记的「统一 decode 层」倾向）**：心跳与握手/关闭同属协议层 resp，保持在各协议 `handle_frame` 里框架自动回。仅给 CMPP `handlers/cmpp.rs` 补 ActiveTest 自动回（对齐 SMGP/SMPP），**不动**已验证的 SMGP/SMPP，SGIP 无心跳不动。理由：与握手同层架构一致 + 不扰动已真机验证路径 + 改动面最小。
- **D-遗留全迁**：3b **全迁**所有遗留 target，**不留任何旧路径样本**。逐 target 判断是否有触裸帧需求 → 用 `RawFrameHandler` 逃生舱口，否则 `MessageHandler`。
- **D-客户端签名**：3c 删 `NoopClientHandler` 占位，`ClientBuilder::new(endpoint, handler, decoder)` 第二参直接收 `Arc<dyn MessageHandler>`（breaking）。

## 4. 子包拆分与门禁

### WP4-3a — 框架新路径补全（动框架，双路径并存、全程全绿）
**范围**：D1a 版本感知内化 + D3b 心跳收归（CMPP 补 ActiveTest）+ 修客户端新路径 V2.0 version 传递 bug。
**不删任何旧东西**：并存桥、旧 trait 全部保留，旧路径 target 不回归。
**验收**：四协议 `*-integration` + `*-stress-test` + `*-multi-account-stress-test` 全绿、零丢失；新增/打通 CMPP V2.0 新路径解码的最小验证；clippy 净。

### WP4-3b — 全迁遗留 target（不动框架）
**范围**：把 §2.3 列出的全部遗留 target 迁到 `MessageHandler`/`RawFrameHandler`：
- `cmpp20_test.rs`（依赖 3a 的 V2.0 版本感知打通）
- `stress_test.rs` 服务端侧 `start_test_server` 全迁 `.message_handlers`；`ClientState` 双 impl（ClientHandler V2.0 + MessageHandler V3.0）收敛为单 `MessageHandler`
- 四协议 `*-longmsg-test`、`*-dynamic-connection-test`、`smgp-unified-pilot`
**规则**：同 WP4-2 的统一迁移变换 T1–T5；逐 target 判断裸帧需求。**不碰框架、不碰 `tests/common/`**（若发现需改框架 → 停下，属 3c 或回 3a）。
**门禁（进 3c 前必须满足）**：四协议**全部** test target（integration + 全压测 + longmsg + dynamic + unified-pilot）全绿、压测零丢失；全仓 `grep 'impl .*BusinessHandler\|impl .*ClientHandler'` 在 `examples/` 与 `tests/` 下**零命中**。

### WP4-3c — 删并存桥 + 退役旧 trait + 收尾（动框架，破坏性）
**范围**：
- 服务端：删 connection.rs:442 并存分支 → 单一新路径；删 `ServerBuilder.handler()/handlers()` 与 `handlers` 字段；删 `run_chain`。
- 客户端：删 `ClientHandler`/`ClientContext`/`NoopClientHandler`；`ClientBuilder::new` 改签名（第二参 `Arc<dyn MessageHandler>`）；删 `run_client_read_loop` 旧分支与 `message_handler: Option` 的择路（转必填）。
- rsms-business：删 `BusinessHandler`/`InboundContext`。
- 删 `unified-shadow` shadow feature 及相关条件编译。
**验收**：四协议全 target + 全压测零丢失 + 全工作区 lib 绿 + clippy 净；`cargo build --workspace` 无未用代码告警。

## 5. 测试与验证策略

- 每个动框架/动压测的子包必须实跑对应协议的 `*-integration` + `*-stress-test` + `*-multi-account-stress-test`（WSL，压测 `--nocapture` + WARN 日志，零丢失为验收线）。
- 3a：四协议全压测复验零丢失（证明版本感知/心跳改动不回归）。
- 3b：每迁一个 target 即跑该 target；进 3c 门禁做四协议全 target 全绿 + grep 零残留。
- 3c：全量回归 + clippy `--workspace`。
- cargo 走 WSL（`RUSTFLAGS='--cap-lints allow'`）、commit 走 Git Bash（见 [[git-remote-via-wsl]]）；端口 flaky 单独重跑（见 [[stress-test-port-flaky]]）。

## 6. 风险与缓解

| 风险 | 缓解 |
|---|---|
| 客户端新路径 V2.0 未传 version（真 bug，3a 修） | 3a 修复后 cmpp20 端到端新路径解码验证；3b 迁 cmpp20_test 时复核 |
| 3c 破坏性删除遗漏残留 impl | 3b 门禁强制全仓 grep 零残留 + 四协议全绿后才进 3c |
| 心跳改动扰动已验证 SMGP/SMPP | D3b 选 handle_frame 统一、只补 CMPP、不动 SMGP/SMPP/SGIP |
| 压测端口 flaky 误判回归 | 单独重跑确认，不放宽断言（[[stress-test-port-flaky]]） |
| stress_test.rs `ClientState` 双 impl 收敛出错 | 3b 单独 task，迁后实跑 cmpp-stress + multi-account 零丢失 |

## 7. 子包交接

逐子包走 brainstorm（本 spec 已覆盖，子包可直接 writing-plans）→ writing-plans → subagent-driven-development（每 task 单独评审，动压测 task 必实跑）。子包 ledger 接续 `.superpowers/sdd/progress.md`。分支 `feature/onboarding-ergonomics` 保留，WP4 整体（3a+3b+3c）完成后统一最终评审 + 合并/PR。

关联记忆：[[onboarding-ergonomics-reshape]]、[[git-remote-via-wsl]]、[[stress-test-port-flaky]]、[[smgp-keepalive-close-13b-bug]]、[[java-interop-stress-4proto]]。
