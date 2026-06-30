# WP4-3c（删并存桥 + 退役旧 trait + 修缺口#2）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: 用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐 task 执行。步骤用 checkbox（`- [ ]`）跟踪。
> **语言要求**：全程思考与输出用中文（仅代码英文关键词除外）。

**Goal:** 删掉并存桥与旧 trait（`BusinessHandler`/`InboundContext`/`run_chain`/`ClientHandler`/`ClientContext`/`NoopClientHandler`/`NoopBusiness`），使框架只剩单一窄腰主路径；`ClientBuilder::new` 第二参收敛为 `Arc<dyn MessageHandler>`；删 `unified-shadow` feature 与零用的 `ClientPool`；并以 A 方案修框架缺口#2（客户端配置层声明版本、`connect()` 自动设），完成「对接易用性重塑」WP4 全部收口。

**Architecture:** 这是触框架最深的收尾包。删除顺序严格按依赖（先增 EndpointConfig 字段 → 删 ClientPool → 改 ClientBuilder 签名+全调用点 → 删服务端桥 → 删孤立 trait 定义 → 删 unified-shadow → 换测试 workaround → 收口）。**`ClientBuilder::new` 签名 breaking，76 处调用点必须与签名改动同一提交全改，半改即编译断。**

**Tech Stack:** Rust edition 2024；`rsms-core`（`EndpointConfig`）；`rsms-connector`（`connection.rs`/`server.rs`/`client.rs`/`client_pool.rs`）；`rsms-business`（trait 定义）；全 `examples/` + `tests/`（调用点）。

## Global Constraints

- **允许 breaking、无需向后兼容**（项目 0.0.1 未发布）。
- **clippy 零告警**：`cargo clippy --workspace` warning-free。
- **公共 API 中文 doc 注释**（`///`/`//!`）。
- **cargo 走 WSL**（`RUSTFLAGS='--cap-lints allow'`）；**commit 走 Git Bash**（[[git-remote-via-wsl]]）。
- **压测 WARN 日志、零丢失为验收线**；端口 flaky 单独重跑（[[stress-test-port-flaky]]）。压测时长 `STRESS_TEST_DURATION_SECS`（标准 300s；快速验证可临时改 30s 跑完还原、不提交——经用户多次确认 30s 足够）。
- **断言不放宽**；删除前 grep 确认零引用再删（孤立定义）。
- 这是 WP4 最后一个子包，完成后 WP4 整体（3a+3b+3c）走全分支最终评审 + 合并/PR。

## 已拍板决策（2026-06-30）

- **缺口#2 走 A 方案（配置层声明）**：`EndpointConfig` 加 `protocol_version: Option<u8>` + `.with_protocol_version(u8)`；`connect()`/`connect_with_pool` 建连后若 `Some(v)` 则 `conn.set_protocol_version(v)`。测试手动 `set_protocol_version(0x20)` workaround 换成配置层声明。
- **删除 `ClientPool`**：`ClientPool`/`connect_with_pool`/`ConnectionReadyCallback` 强依赖即将退役的 `ClientHandler`、examples/tests 零使用——整体删除（不迁移死代码）。

---

## Task 1：缺口#2 A 方案——EndpointConfig 加版本 + connect() 自动设

**Files:**
- Modify: `crates/rsms-core/src/endpoint.rs`（结构体字段 `:5-25`；`new()` `:28-47`；builder 区 `:49-72`，仿 `with_protocol` `:54-57`）
- Modify: `crates/rsms-connector/src/client.rs`（`connect()` 建连点 `:731`、读循环 spawn `:748`；`connect_with_pool` 建连 `:797`、spawn `:815`）
- Test: `crates/rsms-core/src/endpoint.rs`（`mod tests` 若有，否则加）

**Interfaces:**
- Produces：`EndpointConfig.protocol_version: Option<u8>`（默认 None）+ `EndpointConfig::with_protocol_version(self, v: u8) -> Self`；`connect()` 据此自动 `set_protocol_version`。Task 7 的测试 workaround 替换消费它。

- [ ] **Step 1：写失败测试（EndpointConfig builder）**

在 `crates/rsms-core/src/endpoint.rs` 的 `mod tests`（无则新建 `#[cfg(test)] mod tests { use super::*; ... }`）加：

```rust
#[test]
fn with_protocol_version_sets_field() {
    let cfg = EndpointConfig::new("c", "127.0.0.1", 7890, 8, 30)
        .with_protocol_version(0x20);
    assert_eq!(cfg.protocol_version, Some(0x20));
}

#[test]
fn protocol_version_defaults_none() {
    let cfg = EndpointConfig::new("c", "127.0.0.1", 7890, 8, 30);
    assert_eq!(cfg.protocol_version, None);
}
```

- [ ] **Step 2：跑确认失败**

```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-core protocol_version 2>&1 | grep -E 'error\[|no field|no method|test result|FAILED' | tail -10"
```
Expected：编译错误（`no field protocol_version` / `no method with_protocol_version`）。

- [ ] **Step 3：实现 EndpointConfig 字段 + builder + connect 自动设**

`endpoint.rs` 结构体在 `log_level` 字段后加：
```rust
    /// 客户端协议版本（仅 CMPP 区分 V2.0=0x20/V3.0=0x30）。`Some(v)` 时 `connect()` 建连后
    /// 自动 `set_protocol_version(v)`，使客户端首帧响应按正确版本解码（解决服务端自动协商、
    /// 客户端不自动设版本的不对称）。`None`（默认）走协议默认（CMPP 即 V3.0）。
    pub protocol_version: Option<u8>,
```
`new()` 在 `log_level: None` 旁加 `protocol_version: None,`。builder 区仿 `with_protocol` 加：
```rust
    /// 声明客户端协议版本（CMPP V2.0 传 `0x20`）。见字段 [`protocol_version`](Self::protocol_version)。
    pub fn with_protocol_version(mut self, version: u8) -> Self {
        self.protocol_version = Some(version);
        self
    }
```
`client.rs` `connect()` 在 `ClientConnection::new(...)` 返回 `conn`（`:731`）之后、读循环 spawn（`:748`）之前插入：
```rust
    // 缺口#2 修复：客户端按配置声明的版本自动预设（服务端收 Connect 自动协商，客户端侧无此入站时机）。
    if let Some(v) = endpoint.protocol_version {
        conn.set_protocol_version(v).await;
    }
```
`connect_with_pool` 在建连（`:797`）之后、spawn（`:815`）之前插同样逻辑（注意该函数 `endpoint` 变量名以实际为准）。

> 注：`conn.set_protocol_version` 是 `ProtocolConnection` trait 方法（`client.rs:539`），写入 `ctx`；`connect()` 里 `conn` 已是 `Arc<ClientConnection>`，trait 在作用域。

- [ ] **Step 4：跑确认通过 + 编译 connector**

```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-core protocol_version 2>&1 | grep -E 'test result|FAILED' | tail -5"
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo build -p rsms-connector 2>&1 | grep -E 'error|Finished' | tail -5"
```
Expected：2 测试过、connector 编译通过。

- [ ] **Step 5：clippy + commit**

```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy -p rsms-core -p rsms-connector 2>&1 | grep -E 'warning:|error' | tail -10"
```
Expected：无告警。然后（Git Bash）：
```bash
git add crates/rsms-core/src/endpoint.rs crates/rsms-connector/src/client.rs
git commit -m "feat(wp4-3c): 缺口#2 修复——EndpointConfig 加 protocol_version + connect 自动设（A 方案）

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2：删除 ClientPool / connect_with_pool / ConnectionReadyCallback

**Files:**
- Delete/Modify: `crates/rsms-connector/src/client_pool.rs`（`ClientPool` 定义 `:86`、`ClientPool::new` `:99`、import `:12`）——整文件删除（若文件只含 ClientPool）或删 ClientPool 相关
- Modify: `crates/rsms-connector/src/client.rs`（`connect_with_pool` `:784-838`——删整函数）
- Modify: `crates/rsms-connector/src/lib.rs`（导出 `ClientPool`/`ConnectionReadyCallback` `:25` 附近——删）
- Modify: `crates/rsms-connector/src/*.rs`（`mod client_pool;` 声明——删）

**Interfaces:** Consumes 无。Produces：移除一个强依赖 `ClientHandler` 的零用路径，为 Task 4 退役 `ClientHandler` 扫清。

- [ ] **Step 1：grep 确认 ClientPool 零外部使用**

```bash
grep -rn "ClientPool\|connect_with_pool\|ConnectionReadyCallback" examples/ tests/ || echo "[ClientPool 零外部使用]"
grep -rn "ClientPool\|connect_with_pool\|ConnectionReadyCallback\|mod client_pool" crates/ | grep -v "client_pool.rs"
```
Expected：examples/tests 零命中；crates 内仅 lib.rs 导出 + mod 声明 + client.rs connect_with_pool 定义。**若 examples/tests 有命中（与勘探相反），停止、BLOCKED 上报。**

- [ ] **Step 2：删除 ClientPool 相关**

删 `client_pool.rs`（`git rm`，若该文件仅 ClientPool）；删 `client.rs` 的 `connect_with_pool` 函数（`:784-838`，含其内部 `client_handler`/TM 手动驱动）；删 `lib.rs` 对 `ClientPool`/`ConnectionReadyCallback`/`mod client_pool` 的导出与声明。

- [ ] **Step 3：编译 connector**

```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo build -p rsms-connector 2>&1 | grep -E 'error|warning: unused|Finished' | tail -15"
```
Expected：`Finished`（删后无悬空引用；若报 `ClientHandler` 仍被别处用是预期——Task 3/4 再删）。按提示清理因删 ClientPool 而未用的 import。

- [ ] **Step 4：commit**

```bash
git add -A crates/rsms-connector/
git commit -m "chore(wp4-3c): 删除零用的 ClientPool/connect_with_pool/ConnectionReadyCallback（强依赖即将退役的 ClientHandler）

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3：★ClientBuilder::new 签名收敛 + 删客户端桥 + 改全部 76 调用点

**Files:**
- Modify: `crates/rsms-connector/src/client.rs`（`ClientBuilder` `client_handler` 字段 `:635`、`message_handler` 字段 `:642`；`new` 签名 `:646-650`；`.with_message_handler` `:695-698`；`run_client_read_loop` 择路 `:963-990` + 签名 `:842-843`；`connect()` 用 client_handler `:704/:738/:748`）
- Modify: **全部 76 处 `ClientBuilder::new` 调用点**（26 文件：`examples/*_client/src/main.rs` 各 1 + tests 重灾区：`tests/cmpp/cmpp_test.rs` 20、`tests/smgp/integration.rs` 9、`tests/smpp/integration.rs` 9、`tests/sgip/integration.rs` 8、`tests/cmpp/cmpp20_test.rs` 5、`tests/cmpp/transaction_integration_test.rs` 3、`tests/cmpp/stress_test.rs` 2、各 longmsg/dynamic 等）+ 框架自测 `client.rs:1208`
- Modify: 各文件 `use` 列表删 `NoopClientHandler`（105 处引用含 import）

**Interfaces:**
- Consumes 无。Produces：`ClientBuilder::new(endpoint, handler: Arc<dyn MessageHandler>, decoder)`——单一新路径客户端构造。

**★这是 breaking 大改：签名改动与全部调用点改动必须在同一 commit，半改即编译断。** 建议 implementer：先改框架（client.rs），再 `cargo build` 驱动，按编译错误逐文件批量改调用点，直到全绿。

- [ ] **Step 1：改框架 client.rs**

- `ClientBuilder` 删 `client_handler: Arc<dyn ClientHandler>` 字段（`:635`）；`message_handler: Option<Arc<dyn MessageHandler>>`（`:642`）改为 `handler: Arc<dyn MessageHandler>`（必填）。
- `ClientBuilder::new(endpoint, client_handler, decoder)`（`:646-650`）第二参改 `handler: Arc<dyn MessageHandler>`，存入 `handler` 字段；删 `.with_message_handler()`（`:695-698`）。
- `run_client_read_loop`：删 else 旧路径（`:982-990`，`ClientContext` + `client_handler.on_inbound`）、把 `if let Some(mh)` 新路径（`:963-981`）解为无条件主路径；签名 `client_handler` 参（`:842`）删、`message_handler: Option`（`:843`）转必填 `Arc<dyn MessageHandler>`。
- `connect()`：destructure（`:704`）删 `client_handler`；`client_handler_clone`（`:738`）删；传读循环（`:748-749`）只传 `handler`。
- **暂不删** `ClientHandler`/`ClientContext`/`NoopClientHandler` 定义本身（Task 5 删，避免本 task 编译中间态太碎）——但 `NoopClientHandler` 不再被构造。

- [ ] **Step 2：cargo build 驱动改全部调用点**

每处 `ClientBuilder::new(endpoint, Arc::new(NoopClientHandler), XxxDecoder).with_message_handler(handler)` → `ClientBuilder::new(endpoint, handler, XxxDecoder)`；删该文件 `use` 里的 `NoopClientHandler`。反复 `cargo build -p rsms-tests` / 各 example 直到无 `ClientBuilder::new`/`NoopClientHandler` 相关错误。

```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo build --workspace --tests 2>&1 | grep -E 'error\[|error:|Finished' | tail -30"
```
Expected：迭代至 `Finished`。中途用 `grep -rn 'NoopClientHandler\|with_message_handler' examples/ tests/` 自查残留。

- [ ] **Step 3：全 integration + 关键测试回归**

```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test cmpp-integration --test cmpp20-test --test smgp-integration --test smpp-integration --test sgip-integration --test cmpp-longmsg-test --test cmpp-transaction-test --test cmpp-dynamic-connection-test 2>&1 | grep -E 'test result|FAILED|error' | tail -20"
```
Expected：全 `test result: ok`（cmpp20 仍 8/0——注意此时 V2.0 仍靠 Task 7 前的手动 set_protocol_version，本 task 不动测试的 set_protocol_version 语义、只改 builder 构造）。

- [ ] **Step 4：clippy + commit**

```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy --workspace --tests 2>&1 | grep -E 'warning:|error' | tail -15"
```
Expected：无告警。然后（Git Bash）：
```bash
git add -A crates/ examples/ tests/
git commit -m "refactor(wp4-3c)!: ClientBuilder::new 第二参收敛为 Arc<dyn MessageHandler> + 删客户端桥（76 调用点同步改）

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 4：删服务端并存桥（connection.rs + server.rs + run_chain）

**Files:**
- Modify: `crates/rsms-connector/src/connection.rs`（择路 `:441-476`、`run_connection` `handlers` 参 `:335`、import `:3`、注释 `:370/:443`）
- Modify: `crates/rsms-connector/src/server.rs`（`BoundServer.handlers` `:19`、`ServerBuilder.handlers` `:44`、`.handler()` `:70-73`、`.handlers()` `:76-79`、`serve()` 透传 `:139`、`run()` `:198/:216`、import `:7`）
- Modify: `crates/rsms-business/src/lib.rs`（`run_chain` `:101-116`——删）

**Interfaces:** Consumes 无。Produces：服务端单一新路径（`message_handlers` 转必填驱动）。

- [ ] **Step 1：删 connection.rs 旧择路 + handlers 参**

`run_connection`：删 `:442-447` 旧 `run_chain` 分支，把 `:448` `else {` 解为无条件主路径（`message_handlers` 非空假设——本就全新路径）；删签名 `handlers: Vec<Arc<dyn BusinessHandler>>`（`:335`）；删 import 的 `run_chain`/`BusinessHandler`（`:3`）；清 `:370/:443` 残留注释。

- [ ] **Step 2：删 server.rs handlers 字段/方法/透传**

删 `BoundServer.handlers`（`:19`）、`ServerBuilder.handlers`（`:44`）、`.handler()`（`:70-73`）、`.handlers()`（`:76-79`）；`serve()` 删 `handlers: self.handlers`（`:139`）；`run()` 删 `let h = self.handlers.clone()`（`:198`）与 `run_connection(..., h, mh, ...)` 的 `h` 实参（`:216`）；import 删 `BusinessHandler`（`:7`）。

- [ ] **Step 3：删 run_chain**

删 `rsms-business/src/lib.rs:101-116` 的 `run_chain`（其唯一调用点已随 Step1 删）。

- [ ] **Step 4：编译 + 四协议 integration 回归**

```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo build -p rsms-connector -p rsms-business 2>&1 | grep -E 'error|Finished' | tail -10"
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test cmpp-integration --test smgp-integration --test smpp-integration --test sgip-integration 2>&1 | grep -E 'test result|FAILED|error' | tail -15"
```
Expected：编译通过、四 integration 全绿。

- [ ] **Step 5：clippy + commit**

```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy -p rsms-connector -p rsms-business 2>&1 | grep -E 'warning:|error' | tail -10"
```
Expected：无告警。然后（Git Bash）：
```bash
git add crates/rsms-connector/src/connection.rs crates/rsms-connector/src/server.rs crates/rsms-business/src/lib.rs
git commit -m "refactor(wp4-3c)!: 删服务端并存桥（connection.rs is_empty 择路 + ServerBuilder.handlers + run_chain）

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 5：退役旧 trait 定义（BusinessHandler/InboundContext/ClientHandler/ClientContext/Noop*）

**Files:**
- Modify: `crates/rsms-business/src/lib.rs`（`InboundContext` `:13-24`、`BusinessHandler` `:31-67`、`NoopBusiness` `:69-81`——删；同步删其在 lib.rs 的 `pub` 导出）
- Modify: `crates/rsms-connector/src/client.rs`（`ClientContext` `:600-603`、`ClientHandler` `:606-609`、`NoopClientHandler` `:613-623`——删；框架自测 `:1208` 若引用同步改）
- Modify: `crates/rsms-connector/src/lib.rs`（`:21` 删 re-export `ClientHandler, ClientContext, NoopClientHandler`）

**Interfaces:** Consumes 无（前序 task 已删全部引用）。Produces：旧 trait 彻底退役。

- [ ] **Step 1：grep 确认零引用再删**

```bash
grep -rn "BusinessHandler\|InboundContext\|NoopBusiness" crates/ examples/ tests/ | grep -v "MessageHandler\|message" 
grep -rn "\bClientHandler\b\|ClientContext\|NoopClientHandler" crates/ examples/ tests/
```
Expected：除各自定义行 + lib.rs 导出行外，**零业务引用**（命名带 "BusinessHandler" 的结构体 impl MessageHandler 不算）。若有残留引用，回对应 task 补清、勿强删。

- [ ] **Step 2：删定义 + 导出**

删 rsms-business/lib.rs 的 `InboundContext`/`BusinessHandler`/`NoopBusiness` 及导出；删 client.rs 的 `ClientContext`/`ClientHandler`/`NoopClientHandler`；删 rsms-connector/lib.rs:21 的 re-export。框架自测 `client.rs:1208`（`wp4_client_tests`）若用 NoopClientHandler 同步改为新构造。

- [ ] **Step 3：全工作区编译 + lib 测试**

```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo build --workspace --tests 2>&1 | grep -E 'error|Finished' | tail -15"
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test --workspace --lib 2>&1 | grep -E 'test result|error' | tail -15"
```
Expected：编译通过、lib 全绿。

- [ ] **Step 4：clippy + commit**

```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy --workspace 2>&1 | grep -E 'warning:|error' | tail -10"
```
Expected：无告警。然后（Git Bash）：
```bash
git add crates/rsms-business/src/lib.rs crates/rsms-connector/src/client.rs crates/rsms-connector/src/lib.rs
git commit -m "refactor(wp4-3c)!: 退役旧 trait（BusinessHandler/InboundContext/ClientHandler/ClientContext/Noop*）

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 6：删除 unified-shadow feature

**Files:**
- Modify: `crates/rsms-connector/src/connection.rs`（shadow 块 `:423-430`）
- Modify: `crates/rsms-connector/src/client.rs`（shadow 块 `:953-961`）
- Modify: `crates/rsms-connector/Cargo.toml`（`unified-shadow = []` `:9`）
- Modify: `tests/Cargo.toml`（`unified-shadow = [...]` `:123`）
- Modify: `examples/{cmpp,smpp,smgp,sgip}_client/Cargo.toml`（各 `:8` 透传行）

**Interfaces:** Consumes 无。Produces：删去影子比对 feature（纯日志、已无价值）。

- [ ] **Step 1：删两处 cfg 块 + 6 个 Cargo.toml 条目**

删 `connection.rs:423-430`、`client.rs:953-961` 两段 `#[cfg(feature = "unified-shadow")]` 块；删各 Cargo.toml 的 `unified-shadow` feature 定义/透传（connector + tests + 4 example client）。

- [ ] **Step 2：编译确认（默认 + 显式无 feature）**

```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo build --workspace --tests 2>&1 | grep -E 'error|unknown feature|Finished' | tail -10"
```
Expected：`Finished`、无 `unknown feature unified-shadow` 残留引用。

- [ ] **Step 3：clippy + commit**

```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy --workspace 2>&1 | grep -E 'warning:|error' | tail -10"
```
Expected：无告警。然后（Git Bash）：
```bash
git add crates/rsms-connector/src/connection.rs crates/rsms-connector/src/client.rs crates/rsms-connector/Cargo.toml tests/Cargo.toml examples/cmpp_client/Cargo.toml examples/smpp_client/Cargo.toml examples/smgp_client/Cargo.toml examples/sgip_client/Cargo.toml
git commit -m "chore(wp4-3c): 删除 unified-shadow 影子比对 feature（纯日志、已无价值）

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 7：测试 V2.0 workaround 换配置层 with_protocol_version

**Files:**
- Modify: `tests/cmpp/stress_test.rs`（`:499` `set_protocol_version(0x20)`）
- Modify: `tests/cmpp/cmpp_test.rs`（`:1562`）
- Modify: `tests/cmpp/cmpp20_test.rs`（`:288`、`:380`）
- Modify: `tests/cmpp/cmpp_longmsg_test.rs`（`:299`，参数化 `set_protocol_version(version)`）

**Interfaces:** Consumes Task 1 的 `EndpointConfig::with_protocol_version`。Produces：测试 V2.0 走配置层声明，删手动 set_protocol_version——证明缺口#2 修复端到端生效。

- [ ] **Step 1：每处把 V2.0 endpoint 加 .with_protocol_version(0x20)、删手动 set_protocol_version**

对每个用例：建 V2.0 client 的 `EndpointConfig::new(...)...` 链上加 `.with_protocol_version(0x20)`（CMPP V2.0），删 `connect()` 后的 `conn.set_protocol_version(0x20).await`。`cmpp_longmsg_test.rs:299` 参数化的：仅 V2.0 用例加 `.with_protocol_version(version)`（version=0x20 时），V3.0 用例无需（默认）。

> 服务端侧 `handlers/cmpp.rs:131`、`handlers/smpp.rs:107` 是入站协商写入，**不是 workaround，不动**。

- [ ] **Step 2：跑相关 target 确认仍绿（尤其 cmpp20 8/0、cmpp-stress 零丢失）**

```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test cmpp20-test --test cmpp-longmsg-test 2>&1 | grep -E 'test result|FAILED|test_connect_v20|test_submit_v20' | tail -15"
```
Expected：`cmpp20-test` 仍 **8 passed; 0 failed**（V2.0 改走配置层后仍通过）；cmpp-longmsg 全绿。cmpp-stress 留 Task 8 随压测一起复验。

- [ ] **Step 3：grep 确认手动 workaround 已清 + commit**

```bash
grep -rn "set_protocol_version(0x20)\|set_protocol_version(version)" tests/ || echo "[V2.0 workaround 已清]"
```
Expected：零命中（或仅剩确有理由保留的）。然后 clippy + commit（Git Bash）：
```bash
git add tests/cmpp/stress_test.rs tests/cmpp/cmpp_test.rs tests/cmpp/cmpp20_test.rs tests/cmpp/cmpp_longmsg_test.rs
git commit -m "refactor(wp4-3c): 测试 V2.0 手动 set_protocol_version 换配置层 with_protocol_version（缺口#2 端到端）"
```

---

## Task 8：WP4-3c 收口——零残留门禁 + 全量回归 + 四协议压测零丢失

**Files:** 无（纯验证 + spec 增补）。

**Interfaces:** Consumes Task 1–7。Produces：WP4 全收口证据。

- [ ] **Step 1：★零残留门禁 grep（旧 trait/桥彻底消失）**

```bash
echo "=== 旧 trait 定义/引用（期望零，命名结构体 impl MessageHandler 不算）==="
grep -rnE "\b(BusinessHandler|ClientHandler|ClientContext|InboundContext|NoopClientHandler|NoopBusiness|run_chain|ClientPool|with_message_handler)\b" crates/ examples/ tests/ | grep -vE "MessageHandler|message_handler"
echo "=== unified-shadow 残留（期望零）==="
grep -rn "unified-shadow\|unified_shadow" crates/ examples/ tests/
echo "=== .handlers( 残留（期望零）==="
grep -rn "\.handlers(" crates/ examples/ tests/ | grep -v "message_handlers"
```
Expected：三条均零（或仅剩文档/注释的历史提及）。

- [ ] **Step 2：全工作区 lib + clippy + 全 integration**

```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test --workspace --lib 2>&1 | grep -E 'test result|error' | tail -25"
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy --workspace --tests 2>&1 | grep -E 'warning:|error' | tail -20"
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test cmpp-integration --test cmpp20-test --test smgp-integration --test smpp-integration --test sgip-integration --test cmpp-longmsg-test --test smgp-longmsg-test --test smpp-longmsg-test --test sgip-longmsg-test --test cmpp-transaction-test --test cmpp-dynamic-connection-test --test smgp-dynamic-connection-test --test smpp-dynamic-connection-test --test sgip-dynamic-connection-test 2>&1 | grep -E 'test result|FAILED|error' | tail -30"
```
Expected：全绿（cmpp20 8/0）、clippy 净。

- [ ] **Step 3：四协议 multi-account 压测零丢失**

```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && echo '===cmpp===' && cargo test -p rsms-tests --test cmpp-multi-account-stress-test -- --nocapture 2>&1 | grep -E 'test result|unmatched|loss|FAILED' | tail -6 && echo '===smgp===' && cargo test -p rsms-tests --test smgp-multi-account-stress-test -- --nocapture 2>&1 | grep -E 'test result|unmatched|loss|FAILED' | tail -6 && echo '===smpp===' && cargo test -p rsms-tests --test smpp-multi-account-stress-test -- --nocapture 2>&1 | grep -E 'test result|unmatched|loss|FAILED' | tail -6 && echo '===sgip===' && cargo test -p rsms-tests --test sgip-multi-account-stress-test -- --nocapture 2>&1 | grep -E 'test result|unmatched|loss|FAILED' | tail -6"
```
> 字面 target 名链式、勿用 `for` 循环变量。时长由 `STRESS_TEST_DURATION_SECS` 决定（30s 快速即可，标准 300s 留整分支合并前）。
Expected：四协议均 `unmatched: 0` 零丢失。

- [ ] **Step 4：spec 增补 + 更新 ledger/记忆**

`docs/superpowers/specs/2026-06-29-wp4-3-retire-bridge-design.md` §3 增补缺口#2 A 方案（D1c）+ ClientPool 删除决策。在 `.superpowers/sdd/progress.md` 追加 WP4-3c 完成行；更新记忆 [[onboarding-ergonomics-reshape]]（WP4-3c 完成、WP4 整体收口、下一步全分支最终评审 + 合并/PR）。

---

## Self-Review（计划自查结论）

- **范围覆盖**：spec §4 WP4-3c 全部（删服务端桥 Task4 + 客户端桥 Task3 + 退役旧 trait Task5 + ClientBuilder 签名 Task3 + unified-shadow Task6）+ 两拍板决策（缺口#2 A 方案 Task1/7、ClientPool 删除 Task2）+ 收口门禁 Task8。
- **删除顺序依赖**：严格按 Explore 建议——EndpointConfig 增字段（Task1，不破坏）→ 删 ClientPool（Task2，减 ClientHandler 依赖）→ ClientBuilder 签名+76 调用点（Task3，breaking 原子）→ 服务端桥（Task4）→ 删孤立定义（Task5，引用已清）→ unified-shadow（Task6）→ workaround 换配置层（Task7）→ 收口（Task8）。
- **占位符**：每 task 给精确 file:line（源自 Explore 测绘）+ 删除/改动逐条；Task3 的 76 调用点用「cargo build 驱动逐文件批改」+ grep 自查残留兜底。
- **最大风险**：Task3（76 调用点 breaking 原子改）——必须同一提交、cargo build 迭代至全绿；Task2 ClientPool 删除前 grep 兜底（有引用则停）。
- **门禁**：Task8 零残留 grep（旧 trait/桥/unified-shadow/.handlers 全零）是 WP4 收口硬证据。

## 执行交接

计划存 `docs/superpowers/plans/2026-06-30-wp4-3c-retire-bridge-traits.md`。推荐 **Subagent-Driven**：每 task 派新 subagent + 两段式评审；Task3（最大机械面）+ Task8（压测）尤需实测。WP4-3c 完成后 WP4 整体（3a+3b+3c）走全分支最终评审 + finishing-a-development-branch（合并/PR）。关联记忆：[[onboarding-ergonomics-reshape]]、[[git-remote-via-wsl]]、[[stress-test-port-flaky]]。
