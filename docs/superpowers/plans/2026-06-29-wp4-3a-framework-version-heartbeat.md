# WP4-3a 框架新路径补全（版本感知内化 + 心跳收归）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: 用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐 task 执行。步骤用 checkbox（`- [ ]`）跟踪。
> **语言要求**：全程思考与输出用中文（仅代码英文关键词除外，见 AGENTS.md / CLAUDE.md）。

**Goal:** 把版本感知 decode（D1a）与 CMPP 心跳应答（D3b）内化进框架统一主路径，并修掉客户端新路径未传 version 的 V2.0 解码 bug——全程保持并存桥不动、双路径都绿。

**Architecture:** `ProtocolAdapter` 新增 `decode_with_version(frame, Option<u8>)` 默认方法（转调 `decode`），仅 CMPP override 走版本化 codec；服务端与客户端的新路径解码调用点改传 `conn.protocol_version()`。CMPP `handle_frame` 补 `ActiveTest` 分支自动回 `ActiveTestResp`（对齐 SMGP/SMPP，并修掉 inbound ActiveTest 命中 catch-all `Stop` 断连的潜在 bug）。不删任何旧 trait / 并存桥。

**Tech Stack:** Rust edition 2024；`rsms-model`（`ProtocolAdapter`）；`rsms-codec-cmpp`（`CmppAdapter`/`decode_message_with_version`/`CmppVersion`）；`rsms-connector`（`connection.rs` 服务端新路径、`client.rs` 客户端新路径、`handlers/cmpp.rs`）；`rsms-tests`（cmpp-integration 验证）。

## Global Constraints

- **允许 breaking、无需向后兼容**（项目 0.0.1 未发布）。本子包**不删旧路径**，只加/改新路径与框架方法。
- **clippy 零告警**：`cargo clippy --workspace` 必须 warning-free。
- **公共 API 必须有中文 doc 注释**（`///`/`//!`）。
- **cargo 一律走 WSL**，前缀 `RUSTFLAGS='--cap-lints allow'`；**commit 走 Git Bash**（见 [[git-remote-via-wsl]]）。
- **压测必须 WARN 日志**、**零丢失为验收线**（sent 仅在 `send_request` 成功后计数）。端口 flaky 单独重跑（[[stress-test-port-flaky]]）。
- **本子包不删并存桥、不退役旧 trait**（那是 WP4-3c）；不迁遗留 target（那是 WP4-3b）。

---

## Task 1：`ProtocolAdapter::decode_with_version` 默认方法 + CMPP override

**Files:**
- Modify: `crates/rsms-model/src/adapter.rs`（trait 定义，现 `decode` 在 `:13`、`sequence_of` 默认方法在 `:22`）
- Modify: `crates/rsms-codec-cmpp/src/adapter.rs`（`impl ProtocolAdapter for CmppAdapter`，`:478-490`；CMPP 已有 inherent `decode_with_version(frame, CmppVersion)` 在 `:495`，保留不动）
- Test: `crates/rsms-codec-cmpp/src/adapter.rs`（既有 `mod tests`，`:517` 起）、`crates/rsms-codec-smgp/src/adapter.rs`（既有 `mod tests`）

**Interfaces:**
- Produces：`ProtocolAdapter::decode_with_version(&self, frame: &Frame, version: Option<u8>) -> Result<UnifiedMessage>`，默认实现 `self.decode(frame)`；CMPP override 调 `decode_message_with_version(frame.data_as_slice(), version)`。Task 2/3 的驱动层消费它。

- [ ] **Step 1：写失败测试（CMPP override 行为）**

在 `crates/rsms-codec-cmpp/src/adapter.rs` 的 `mod tests` 内，仿照既有 `frame_of` 与 V3.0 Submit 构造（参 `:524` `frame_of`、`:528` `decode_submit_v30_billing_fields` 的 fixture 构造方式）新增：

```rust
#[test]
fn decode_with_version_none_equals_decode() {
    // ActiveTest 版本无关：默认（None）路径必须与 decode 一致。
    let f = frame_of(vec![
        0x00, 0x00, 0x00, 0x0C, // total_len = 12
        0x00, 0x00, 0x00, 0x08, // ActiveTest
        0x00, 0x00, 0x00, 0x01, // seq = 1
    ]);
    let a = CmppAdapter;
    assert_eq!(
        a.decode_with_version(&f, None).unwrap(),
        a.decode(&f).unwrap(),
        "None 版本应等价于基础 decode"
    );
    assert_eq!(a.decode_with_version(&f, None).unwrap(), UnifiedMessage::Ping);
}

#[test]
fn decode_with_version_routes_v20() {
    // V2.0 Submit 仅经 Some(0x20) 正确解出；用基础 decode（V3.0 布局）会字段错位/长度不符。
    // fixture：复用本模块 V2.0 Submit 构造（参既有 V2.0 相关测试），断言 Some(0x20) 解出 Submit。
    let f = v20_submit_frame(); // 见 Step 3 注：若模块无此 helper，按既有 V3.0 fixture 模式加 V2.0 版本
    let a = CmppAdapter;
    let msg = a.decode_with_version(&f, Some(0x20)).expect("V2.0 解码应成功");
    assert!(matches!(msg, UnifiedMessage::Submit(_)), "Some(0x20) 应解出 Submit");
}
```

> 注：`UnifiedMessage` 需 `PartialEq`（既有 adapter 测试已 `assert_eq!` 比较，故已满足）。`v20_submit_frame()` 若模块内无现成 helper，参照 `crates/rsms-codec-cmpp/src/message.rs` 的 `decode_message_with_version` V2.0 测试 fixture（该文件 `:154` 起及其测试）构造一个 V2.0 Submit PDU 的 `frame_of(...)`。

- [ ] **Step 2：跑测试确认失败**

Run（WSL）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-codec-cmpp decode_with_version 2>&1 | grep -E 'error\[|cannot find|test result|FAILED' | tail -20"
```
Expected：编译错误 `no method named decode_with_version found for ... in this scope`（trait 方法尚未加；CMPP 现有 inherent 同名但签名为 `CmppVersion`，`Some(0x20)` 类型不匹配）。

- [ ] **Step 3：实现 trait 默认方法 + CMPP override**

在 `crates/rsms-model/src/adapter.rs` 的 `pub trait ProtocolAdapter` 内、`decode` 之后加默认方法：

```rust
    /// 版本感知解码：默认转调 [`decode`](Self::decode)（适用于单版本/版本透明协议）。
    ///
    /// 版本无法从帧字节判定（CMPP V2.0/V3.0 命令字相同、仅字段布局不同），须由握手协商的
    /// `version`（如 CMPP `0x20`/`0x30`，其余协议传 `None`）决定。框架驱动层按
    /// `conn.protocol_version()` 调用本方法；只有 CMPP 适配器需 override。
    fn decode_with_version(&self, frame: &Frame, _version: Option<u8>) -> Result<UnifiedMessage> {
        self.decode(frame)
    }
```

在 `crates/rsms-codec-cmpp/src/adapter.rs` 的 `impl ProtocolAdapter for CmppAdapter`（`:478`）内，`decode`（`:482-485`）之后加 override：

```rust
    fn decode_with_version(
        &self,
        frame: &Frame,
        version: Option<u8>,
    ) -> Result<UnifiedMessage> {
        let msg = decode_message_with_version(frame.data_as_slice(), version)?;
        Ok(cmpp_to_unified(msg))
    }
```

> 说明：trait 方法 `decode_with_version(_, Option<u8>)` 与 CMPP 既有 inherent `decode_with_version(_, CmppVersion)`（`:495`，example 仍用）同名但签名不同——Rust 允许共存，通过 `&dyn ProtocolAdapter` 调用时解析到 trait 方法、具体类型直调按参数类型消歧。`decode_message_with_version` 已 `use`（adapter.rs 顶部从 codec 引入）。

- [ ] **Step 4：加 SMGP 默认转发测试**

在 `crates/rsms-codec-smgp/src/adapter.rs` 的 `mod tests` 内加（仿该模块既有 `frame_of`/decode 测试）：

```rust
#[test]
fn decode_with_version_defaults_to_decode() {
    // SMGP 单版本：decode_with_version 任意 version 都应等价于 decode（默认转发）。
    let f = /* 复用本模块既有 Submit/ActiveTest fixture 构造一个合法 SMGP 帧 */ smgp_active_test_frame();
    let a = SmgpAdapter;
    assert_eq!(
        a.decode_with_version(&f, Some(0x99)).unwrap(),
        a.decode(&f).unwrap(),
        "SMGP 应忽略 version、默认转发 decode"
    );
}
```

> `smgp_active_test_frame()`：若模块无现成 helper，用该 crate 既有 fixture 模式构造一个最简合法 SMGP 帧（ActiveTest 12B：total_len + ActiveTest command_id + seq）。

- [ ] **Step 5：跑测试确认通过 + clippy**

Run（WSL）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-codec-cmpp -p rsms-codec-smgp decode_with_version 2>&1 | grep -E 'test result|FAILED|error' | tail -20"
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy -p rsms-model -p rsms-codec-cmpp -p rsms-codec-smgp 2>&1 | grep -E 'warning:|error' | tail -10"
```
Expected：`test result: ok`（3 测试全过）、clippy 无告警。

- [ ] **Step 6：commit**

（Git Bash）：
```bash
git add crates/rsms-model/src/adapter.rs crates/rsms-codec-cmpp/src/adapter.rs crates/rsms-codec-smgp/src/adapter.rs
git commit -m "feat(wp4-3a): ProtocolAdapter 加 decode_with_version 默认方法 + CMPP override

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2：服务端新路径传 version（修业务解码版本感知）

**Files:**
- Modify: `crates/rsms-connector/src/connection.rs`（新路径业务解码 `:452` `adapter.decode(&frame)`）

**Interfaces:**
- Consumes：Task 1 的 `ProtocolAdapter::decode_with_version`。
- Produces：服务端新路径按 `conn.protocol_version()` 解码业务消息（V2.0 不再被按 V3.0 误解）。

**背景**：服务端 CMPP `handle_frame` 已版本感知（`handlers/cmpp.rs:99` `decode_message_with_version(frame_bytes, conn.protocol_version().await)`），但 `Continue` 后新路径的**业务解码**（`connection.rs:452`）仍用无版本 `adapter.decode` → V2.0 业务消息字段错位。

- [ ] **Step 1：改 connection.rs 新路径解码调用**

在 `crates/rsms-connector/src/connection.rs`，把 `:452` 一行：

```rust
                    match adapter.decode(&frame) {
```
改为：
```rust
                    match adapter.decode_with_version(&frame, conn_arc.protocol_version().await) {
```

> `conn_arc` 是 `Arc<ServerConnection>`，impl 了 `crate::protocol::ProtocolConnection`，其 `async fn protocol_version(&self) -> Option<u8>` 即 `handlers/cmpp.rs:99` 用的同一方法；该 trait 在 connection.rs 已在作用域（`run_connection` 全程使用 `conn`/`conn_arc` 的 ProtocolConnection 方法）。若编译报方法不可见，在文件顶部确认 `use crate::protocol::ProtocolConnection;` 在 scope。

- [ ] **Step 2：编译确认通过**

Run（WSL）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo build -p rsms-connector 2>&1 | grep -E 'error|warning: unused|Finished' | tail -20"
```
Expected：`Finished`，无 error。

- [ ] **Step 3：跑 CMPP 集成测试确认 V3.0 不回归**

Run（WSL）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test cmpp-integration 2>&1 | grep -E 'test result|FAILED|error' | tail -20"
```
Expected：`test result: ok`（V3.0 走 version=Some(0x30)/None 等价 decode，零回归）。

- [ ] **Step 4：commit**

（Git Bash）：
```bash
git add crates/rsms-connector/src/connection.rs
git commit -m "fix(wp4-3a): 服务端新路径业务解码改 decode_with_version 传 protocol_version

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3：客户端新路径传 version（修 V2.0 解码真 bug）

**Files:**
- Modify: `crates/rsms-connector/src/client.rs`（新路径解码 `:967` `adapter.decode(&frame)`）

**Interfaces:**
- Consumes：Task 1 的 `decode_with_version`。
- Produces：客户端新路径按 `conn.protocol_version()` 解码（修掉 §现状中「客户端新路径未传 version → V2.0 按 V3.0 误解」的真 bug）。

- [ ] **Step 1：改 client.rs 新路径解码调用**

在 `crates/rsms-connector/src/client.rs`，把 `:967` 一行：

```rust
                match adapter.decode(&frame) {
```
改为：
```rust
                match adapter.decode_with_version(&frame, conn.protocol_version().await) {
```

> `conn` 是 `Arc<ClientConnection>`，已 impl `protocol_version()`（`client.rs:535` 与 `:594` 两个 ProtocolConnection trait 各一份，返回 `self.ctx.lock().await.protocol_version()`）；此处 `conn` 已在 `BusinessProtocolConnection` 语境（`:971` 已 `as Arc<dyn BusinessProtocolConnection>` 转型），方法可见。

- [ ] **Step 2：编译确认通过**

Run（WSL）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo build -p rsms-connector 2>&1 | grep -E 'error|Finished' | tail -10"
```
Expected：`Finished`，无 error。

- [ ] **Step 3：跑 CMPP 集成测试确认不回归**

Run（WSL）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test cmpp-integration 2>&1 | grep -E 'test result|FAILED|error' | tail -20"
```
Expected：`test result: ok`。

- [ ] **Step 4：commit**

（Git Bash）：
```bash
git add crates/rsms-connector/src/client.rs
git commit -m "fix(wp4-3a): 客户端新路径解码改 decode_with_version 传 protocol_version（修 V2.0 误解）

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 4：CMPP V2.0 新路径端到端集成测试（坐实 Task 2+3）

**Files:**
- Modify: `tests/cmpp/cmpp_test.rs`（已是新 `MessageHandler` 路径；新增一个 V2.0 用例）

**Interfaces:**
- Consumes：Task 2（服务端）+ Task 3（客户端）的版本感知解码；`cmpp_test.rs` 既有 `start_test_server`（`.message_handlers`）与 `ClientBuilder::new(..).with_message_handler(..)` 建连模式。
- Produces：证明 V2.0 连接在**新路径**双向正确解码的回归测试。

**背景**：`cmpp20_test.rs` 仍走旧路径（WP4-3b 才迁），故 3a 的 V2.0 新路径验证须在已迁新路径的 `cmpp_test.rs` 内新增一个 V2.0 用例（握手 version=`0x20`），断言服务端 `MessageHandler` 收到正确解码的 V2.0 Submit、客户端正确收到 V2.0 SubmitResp。

- [ ] **Step 1：写 V2.0 新路径集成测试**

在 `tests/cmpp/cmpp_test.rs` 末尾新增一个 `#[tokio::test]`，结构参照该文件既有 V3.0 端到端用例（`start_test_server` + 新路径 client），唯一区别是客户端以 CMPP **V2.0** 握手（Connect version 字节 `0x20`）。V2.0 握手与发包细节参照 `tests/cmpp/cmpp20_test.rs` 既有 V2.0 客户端构造，但 server 用 `.message_handlers(...)`、client 用 `ClientBuilder::new(endpoint, Arc::new(NoopClientHandler), CmppDecoder).with_message_handler(handler)`。断言要点：

```rust
// 服务端 MessageHandler 收到的 UnifiedMessage::Submit 内容正确（V2.0 字段未错位）
assert_eq!(received_submit.dest_terminals /* 或该文件既有断言的等价字段 */, expected);
// 客户端新路径收到 SubmitResp（msg_id 正确），无解码失败日志
assert!(client_got_submit_resp, "客户端新路径应正确解码 V2.0 SubmitResp");
```

> 断言字段名以 `cmpp_test.rs` 既有 V3.0 用例实际断言的 `UnifiedSubmit`/`UnifiedSubmitResp` 字段为准（保持与该文件同款断言强度，不放宽）。若该文件已有「按 version 参数化」的 helper，直接传 V2.0 复用；否则复制 V3.0 用例改 version。

- [ ] **Step 2：跑测试确认通过（先确认未改前会暴露 V2.0 解码问题）**

> 顺序说明：Task 2/3 已合入，本测试应直接通过。为坐实它真的在测版本感知，可临时把 `connection.rs` 改回 `adapter.decode(&frame)` 跑一次本测试观察 FAIL，再改回——可选验证，不提交临时改动。

Run（WSL）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test cmpp-integration 2>&1 | grep -E 'test result|FAILED|error' | tail -20"
```
Expected：`test result: ok`，含新 V2.0 用例。

- [ ] **Step 3：clippy + commit**

```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy -p rsms-tests --test cmpp-integration 2>&1 | grep -E 'warning:|error' | tail -10"
```
Expected：无告警。然后（Git Bash）：
```bash
git add tests/cmpp/cmpp_test.rs
git commit -m "test(wp4-3a): 新增 CMPP V2.0 新路径端到端集成测试（坐实版本感知解码）

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 5：D3b — CMPP `handle_frame` 心跳自动回（并修 ActiveTest 断连 bug）

**Files:**
- Modify: `crates/rsms-connector/src/handlers/cmpp.rs`（`handle_frame` 的 `match msg`，`:120-193`；现 `_ => return Ok(HandleResult::Stop)` 在 `:192`）

**Interfaces:**
- Consumes：无新依赖（`CmppMessage::ActiveTest`/`ActiveTestResp`、`encode_message` 均已 `use`，见 `cmpp.rs:3-6`）。
- Produces：CMPP inbound ActiveTest → 框架自动回 `ActiveTestResp` 并返回 `Continue`（对齐 SMGP `handlers/smgp.rs:119-134`、SMPP `handlers/smpp.rs:150`），业务无需处理心跳；同时修掉 ActiveTest 现命中 catch-all `Stop` 断连的潜在 bug。

**背景**：CMPP `handle_frame` 的 `match msg` 无 `ActiveTest` 分支，inbound ActiveTest 落到 `_ => Stop` → 断连。SMGP/SMPP 已在各自 handler 自动回。本 task 给 CMPP 补齐、不动其他协议。

- [ ] **Step 1：写失败测试（心跳应答 + 不断连）**

在 `tests/cmpp/cmpp_test.rs` 新增一个 `#[tokio::test]`：客户端建连鉴权后，向服务端发一帧 CMPP ActiveTest（command_id `0x00000008`、12B），断言：(a) 收到 ActiveTestResp（command_id `0x80000008`）；(b) 连接未断、后续仍能正常发 Submit 收 SubmitResp。

```rust
#[tokio::test]
async fn cmpp_server_auto_replies_active_test() {
    // 建连鉴权（复用本文件既有 helper）→ 发 ActiveTest(12B) → 断言收到 ActiveTestResp 且连接存活。
    let active_test = vec![
        0x00, 0x00, 0x00, 0x0C, // total_len = 12
        0x00, 0x00, 0x00, 0x08, // ActiveTest
        0x00, 0x00, 0x00, 0x2A, // seq = 42
    ];
    // ... 发送并读取应答帧 ...
    assert_eq!(resp_command_id, 0x8000_0008, "应收到 ActiveTestResp");
    // ... 再发一条 Submit，断言仍能收到 SubmitResp（连接未被 Stop 断开）...
}
```

> 用本文件既有的建连 / 读写帧 helper（与既有用例同款）。若直接读裸帧不便，可断言「ActiveTest 后再发 Submit 仍得到 SubmitResp」来间接证明未断连。

- [ ] **Step 2：跑测试确认失败**

Run（WSL）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test cmpp-integration cmpp_server_auto_replies_active_test 2>&1 | grep -E 'test result|FAILED|panicked|error' | tail -20"
```
Expected：FAIL——现状 ActiveTest 命中 `_ => Stop`，连接被断、收不到 ActiveTestResp（或后续 Submit 无响应）。

- [ ] **Step 3：实现 CMPP ActiveTest 自动回**

在 `crates/rsms-connector/src/handlers/cmpp.rs` 的 `match msg` 内、`_ => return Ok(HandleResult::Stop)`（`:192`）**之前**插入：

```rust
            CmppMessage::ActiveTest { version, sequence_id } => {
                // 心跳收归框架（D3b）：CMPP ActiveTest 自动回 ActiveTestResp 并 Continue，
                // 对齐 SMGP/SMPP；业务无需处理心跳。亦修掉 ActiveTest 此前命中 catch-all Stop 断连的 bug。
                let resp = CmppMessage::ActiveTestResp { version, sequence_id };
                if let Ok(pdu) = encode_message(&resp) {
                    conn.write_frame(&pdu).await?;
                }
                return Ok(HandleResult::Continue);
            }
```

> `CmppMessage::ActiveTest`/`ActiveTestResp` 字段为 `{ version, sequence_id }`（见 `rsms-codec-cmpp/src/message.rs:72-77`）。`encode_message` 把 `ActiveTestResp` 编为 1B reserved body（`message.rs:324` `ActiveTestResp { reserved: 0 }`），合规 PDU。

- [ ] **Step 4：跑测试确认通过 + CMPP 全集成不回归**

Run（WSL）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test cmpp-integration 2>&1 | grep -E 'test result|FAILED|error' | tail -20"
```
Expected：`test result: ok`，含新心跳用例；既有用例零回归。

- [ ] **Step 5：clippy + commit**

```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy -p rsms-connector -p rsms-tests --test cmpp-integration 2>&1 | grep -E 'warning:|error' | tail -10"
```
Expected：无告警。然后（Git Bash）：
```bash
git add crates/rsms-connector/src/handlers/cmpp.rs tests/cmpp/cmpp_test.rs
git commit -m "feat(wp4-3a): CMPP handle_frame 心跳自动回 ActiveTestResp（D3b，修 ActiveTest 断连 bug）

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 6：WP4-3a 收口——四协议全压测零丢失 + 全工作区回归 + clippy

**Files:** 无（纯验证）。

**Interfaces:** Consumes Task 1–5 全部改动。Produces：3a 验收证据（双路径仍绿、版本感知/心跳改动不回归）。

- [ ] **Step 1：全工作区 lib 回归 + clippy**

Run（WSL）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test --workspace --lib 2>&1 | grep -E 'test result|error' | tail -30"
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy --workspace 2>&1 | grep -E 'warning:|error' | tail -20"
```
Expected：全绿、clippy 零告警。

- [ ] **Step 2：四协议集成测试（含旧路径 cmpp20 不回归）**

Run（WSL，逐条）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test cmpp-integration --test cmpp20-test --test smgp-integration --test smpp-integration --test sgip-integration 2>&1 | grep -E 'test result|FAILED|error' | tail -30"
```
Expected：全部 `test result: ok`。**重点**：`cmpp20-test`（旧路径）须仍绿——证明 Task 5 的 ActiveTest 分支与 Task 1 的新增 trait 方法未破坏旧路径。

- [ ] **Step 3：四协议压测零丢失复验**

Run（WSL，逐条；压测慢、端口 flaky 单独重跑）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test cmpp-multi-account-stress-test -- --nocapture 2>&1 | grep -E 'test result|TPS|丢失|loss|unmatched|FAILED' | tail -20"
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test smgp-multi-account-stress-test -- --nocapture 2>&1 | grep -E 'test result|TPS|丢失|loss|unmatched|FAILED' | tail -20"
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test smpp-multi-account-stress-test -- --nocapture 2>&1 | grep -E 'test result|TPS|丢失|loss|unmatched|FAILED' | tail -20"
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test sgip-multi-account-stress-test -- --nocapture 2>&1 | grep -E 'test result|TPS|丢失|loss|unmatched|FAILED' | tail -20"
```
Expected：四个均 `test result: ok`、零丢失（unmatched=0 / sent==recv）。端口竞争偶发超时单独重跑确认（[[stress-test-port-flaky]]），不放宽断言。

- [ ] **Step 4：更新 ledger（无代码 commit；如需记录则 docs commit）**

在 `.superpowers/sdd/progress.md` 追加 WP4-3a 完成行（Task 1–6 + 验收证据），并按需更新记忆 [[onboarding-ergonomics-reshape]]。

---

## Self-Review（计划自查结论）

- **Spec 覆盖**：WP4-3a spec §4「框架新路径补全」三项全覆盖——D1a 版本感知内化（Task 1 trait 方法 + Task 2/3 两调用点）、D3b 心跳收归（Task 5）、客户端 V2.0 version bug（Task 3，并 Task 4 端到端坐实）。3a「不删旧、双路径全绿」由 Task 6 的 cmpp20（旧路径）+ 四协议压测复验保证。
- **占位符扫描**：测试 fixture 处明确指向「复用既有模块 helper / 既有 V3.0 用例模式」，因这是既有代码库且测试样板成熟；框架改动与 handler 改动均给出逐字代码。无 TBD/TODO。
- **类型一致**：`decode_with_version(&self, frame: &Frame, version: Option<u8>) -> Result<UnifiedMessage>` 在 Task 1 定义、Task 2/3 一致消费；`conn.protocol_version() -> Option<u8>`（async）两端一致；`CmppMessage::ActiveTest{version,sequence_id}` 与 `ActiveTestResp{version,sequence_id}` 字段名与 codec 一致。
- **风险**：Task 2 的 `conn_arc.protocol_version()` 可见性、Task 4/5 测试 fixture 的 V2.0 握手细节是执行时最可能卡点——均已给定位与样板出处（cmpp.rs:99 同款调用 / cmpp20_test.rs V2.0 客户端 / smgp.rs 心跳样板）。

## 执行交接

计划存 `docs/superpowers/plans/2026-06-29-wp4-3a-framework-version-heartbeat.md`。推荐 **Subagent-Driven**：每 task 派新 subagent + 两段式评审；Task 5/6 动框架/压测，必实跑 cmpp-integration + 四协议 multi-account 压测零丢失。关联记忆：[[onboarding-ergonomics-reshape]]、[[git-remote-via-wsl]]、[[stress-test-port-flaky]]、[[smgp-keepalive-close-13b-bug]]。
