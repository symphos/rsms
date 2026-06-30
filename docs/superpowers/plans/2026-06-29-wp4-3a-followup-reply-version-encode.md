# WP4-3a-followup（D1b：ctx.reply 版本感知 encode）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: 用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐 task 执行。步骤用 checkbox（`- [ ]`）跟踪。
> **语言要求**：全程思考与输出用中文（仅代码英文关键词除外，见 AGENTS.md / CLAUDE.md）。

**Goal:** 把版本感知编码内化进 `ctx.reply`（D1a 的 encode 镜像），使 V2.0 服务端用 `ctx.reply` 自动回 V2.0 格式应答，消除「手动 `encode_with_version` 回包」特例，为 WP4-3b 统一用 `ctx.reply` 迁遗留 target 铺平。

**Architecture:** `ProtocolAdapter` 新增 `encode_with_version(&self, msg, seq, version: Option<u8>)` 默认方法（转调 `encode`），仅 CMPP override 走 `unified_to_cmpp_with_version`；`MessageContext::reply` 改用它并传 `conn.protocol_version()`。与 WP4-3a 的 `decode_with_version` 完全对称。**不删任何旧 trait/旧路径/并存桥。**

**Tech Stack:** Rust edition 2024；`rsms-model`（`ProtocolAdapter`）；`rsms-codec-cmpp`（`CmppAdapter`/`unified_to_cmpp_with_version`/`CmppVersion`/`encode_message`）；`rsms-business`（`MessageContext::reply`）；`rsms-tests`（四协议 integration + 压测验证）。

## Global Constraints

- **允许 breaking、无需向后兼容**（项目 0.0.1 未发布）。本子包**不删旧路径/并存桥**，只加框架方法 + 改 `ctx.reply` 内部一行。
- **clippy 零告警**：`cargo clippy --workspace` 必须 warning-free。
- **公共 API 必须有中文 doc 注释**（`///`/`//!`）。
- **cargo 一律走 WSL**，前缀 `RUSTFLAGS='--cap-lints allow'`；**commit 走 Git Bash**（见 [[git-remote-via-wsl]]）。
- **压测必须 WARN 日志**、**零丢失为验收线**；端口 flaky 单独重跑（[[stress-test-port-flaky]]）。
- **最大回归风险**：V3.0/单版本协议 `encode_with_version(None/Some(0x30))` 必须**逐字节等于** `encode`——否则 WP4-1/4-2 全部 `ctx.reply` 用例回归。Task 1 单测先钉死等价。

---

## Task 1：`ProtocolAdapter::encode_with_version` 默认方法 + CMPP override

**Files:**
- Modify: `crates/rsms-model/src/adapter.rs`（trait；`encode` 在 `:18`、`decode_with_version` 默认方法在 `:31-33`——本 task 在其后加对称的 `encode_with_version` 默认方法）
- Modify: `crates/rsms-codec-cmpp/src/adapter.rs`（`impl ProtocolAdapter for CmppAdapter`，`encode` 在 `:494`；模块私有 `unified_to_cmpp_with_version(msg, seq, version: CmppVersion)` 在 `:275`；inherent `encode_with_version(_, CmppVersion)` 在 `:514` 保留不动）
- Test: `crates/rsms-codec-cmpp/src/adapter.rs`（既有 `mod tests`）、`crates/rsms-codec-smgp/src/adapter.rs`（既有 `mod tests`）

**Interfaces:**
- Produces：`ProtocolAdapter::encode_with_version(&self, msg: &UnifiedMessage, seq: Sequence, version: Option<u8>) -> Result<Vec<u8>>`，默认实现 `self.encode(msg, seq)`；CMPP override 用 `CmppVersion::from_wire` 把 `Option<u8>` 映射为 `CmppVersion`（None/未知→V30）后调 `unified_to_cmpp_with_version`。Task 2 的 `ctx.reply` 消费它。

- [ ] **Step 1：写失败测试（CMPP override + 等价 + SMGP 默认转发）**

在 `crates/rsms-codec-cmpp/src/adapter.rs` 的 `mod tests` 内新增（复用本模块既有 `frame_of` 与 V2.0/V3.0 fixture 构造方式；构造一个 `UnifiedMessage::SubmitResp` 用于编码——参照本模块既有 encode 测试取得合法 `SubmitResp` 值）：

```rust
#[test]
fn encode_with_version_none_equals_encode() {
    // 版本无关默认路径：None 必须逐字节等于 encode（保证 V3.0/ctx.reply 零回归）。
    let a: &dyn ProtocolAdapter = &CmppAdapter;
    let msg = sample_submit_resp(); // 见下注：复用既有 SubmitResp fixture 构造
    let seq = Sequence::Plain(7);
    assert_eq!(
        a.encode_with_version(&msg, seq, None).unwrap(),
        a.encode(&msg, seq).unwrap(),
        "None 版本应逐字节等于 encode"
    );
}

#[test]
fn encode_with_version_some30_equals_encode() {
    // Some(0x30) 与 None 同走 V3.0，必须等于 encode。
    let a: &dyn ProtocolAdapter = &CmppAdapter;
    let msg = sample_submit_resp();
    let seq = Sequence::Plain(7);
    assert_eq!(
        a.encode_with_version(&msg, seq, Some(0x30)).unwrap(),
        a.encode(&msg, seq).unwrap(),
        "Some(0x30) 应逐字节等于 encode"
    );
}

#[test]
fn encode_with_version_v20_differs_from_v30() {
    // Some(0x20) 产出 V2.0 应答（SubmitResp 21B），与 V3.0（24B）不同——证明版本感知生效。
    let a: &dyn ProtocolAdapter = &CmppAdapter;
    let msg = sample_submit_resp();
    let seq = Sequence::Plain(7);
    let v20 = a.encode_with_version(&msg, seq, Some(0x20)).unwrap();
    let v30 = a.encode(&msg, seq).unwrap();
    assert_ne!(v20, v30, "Some(0x20) 应产出与 V3.0 不同的 V2.0 字节");
}
```

> 注：`sample_submit_resp()` 若模块内无现成 helper，按本模块既有 encode 测试里构造 `UnifiedMessage::SubmitResp{...}` 的同款方式自建（一个最简合法 SubmitResp）。`UnifiedMessage`/`Sequence` 已在 `mod tests` 的 `use super::*` 可见。测试经 `&dyn ProtocolAdapter` 调用——因 CMPP 既有 inherent `encode_with_version(_, CmppVersion)` 与 trait `encode_with_version(_, Option<u8>)` 同名，具体类型直调会按实参类型走 inherent；经 trait object 精确走 trait 方法（与 WP4-3a Task1 的 decode 测试同款处理）。

在 `crates/rsms-codec-smgp/src/adapter.rs` 的 `mod tests` 内新增：

```rust
#[test]
fn encode_with_version_defaults_to_encode() {
    // SMGP 单版本：encode_with_version 任意 version 都应等于 encode（默认转发）。
    let a: &dyn ProtocolAdapter = &SmgpAdapter;
    let msg = sample_smgp_submit_resp(); // 复用本模块既有 SubmitResp/encode fixture
    let seq = Sequence::Plain(3);
    assert_eq!(
        a.encode_with_version(&msg, seq, Some(0x99)).unwrap(),
        a.encode(&msg, seq).unwrap(),
        "SMGP 应忽略 version、默认转发 encode"
    );
}
```

- [ ] **Step 2：跑测试确认失败**

Run（WSL）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-codec-cmpp -p rsms-codec-smgp encode_with_version 2>&1 | grep -E 'error\[|cannot find|no method|test result|FAILED' | tail -20"
```
Expected：编译错误 `no method named encode_with_version found for reference &dyn ProtocolAdapter`（trait 方法尚未加）。

- [ ] **Step 3：实现 trait 默认方法 + CMPP override**

在 `crates/rsms-model/src/adapter.rs` 的 `decode_with_version` 默认方法（`:31-33`）之后加：

```rust
    /// 版本感知编码：默认转调 [`encode`](Self::encode)（适用于单版本/版本透明协议）。
    ///
    /// 与 [`decode_with_version`](Self::decode_with_version) 对称：版本由握手协商的 `version`
    /// 决定（CMPP `0x20`/`0x30`，其余协议传 `None`）。框架的 `ctx.reply` 按 `conn.protocol_version()`
    /// 调用本方法；只有 CMPP 适配器需 override，以产出 V2.0 应答（如 SubmitResp 21B）。
    fn encode_with_version(
        &self,
        msg: &UnifiedMessage,
        seq: Sequence,
        _version: Option<u8>,
    ) -> Result<Vec<u8>> {
        self.encode(msg, seq)
    }
```

在 `crates/rsms-codec-cmpp/src/adapter.rs` 的 `impl ProtocolAdapter for CmppAdapter` 内，`encode`（`:494`）之后加 override（`CmppVersion`/`unified_to_cmpp_with_version`/`encode_message` 均已在文件作用域）：

```rust
    fn encode_with_version(
        &self,
        msg: &UnifiedMessage,
        seq: Sequence,
        version: Option<u8>,
    ) -> Result<Vec<u8>> {
        // Option<u8> → CmppVersion：复用 codec 的 from_wire（0x20/0x00/0x01→V20，0x30→V30）；
        // None 或未知字节默认 V3.0，与 trait encode 行为一致（保证零回归）。
        let v = match version {
            Some(b) => CmppVersion::from_wire(b).unwrap_or(CmppVersion::V30),
            None => CmppVersion::V30,
        };
        let cmpp = unified_to_cmpp_with_version(msg, seq, v)?;
        encode_message(&cmpp)
    }
```

> 说明：trait 方法与 CMPP 既有 inherent `encode_with_version(_, CmppVersion)`（`:514`，example 仍用）同名不同签名，Rust 允许共存；经 `&dyn ProtocolAdapter` 调用走 trait 版。inherent 版**不删不改**。

- [ ] **Step 4：跑测试确认通过 + clippy**

Run（WSL）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-codec-cmpp -p rsms-codec-smgp encode_with_version 2>&1 | grep -E 'test result|FAILED|error' | tail -20"
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy -p rsms-model -p rsms-codec-cmpp -p rsms-codec-smgp 2>&1 | grep -E 'warning:|error' | tail -10"
```
Expected：`test result: ok`（4 测试全过）、clippy 无告警。

- [ ] **Step 5：commit**

（Git Bash）：
```bash
git add crates/rsms-model/src/adapter.rs crates/rsms-codec-cmpp/src/adapter.rs crates/rsms-codec-smgp/src/adapter.rs
git commit -m "feat(wp4-3a-followup): ProtocolAdapter 加 encode_with_version 默认方法 + CMPP override

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2：`MessageContext::reply` 改用 encode_with_version 传 version

**Files:**
- Modify: `crates/rsms-business/src/message_context.rs`（`reply` 在 `:52-54`：`adapter.encode(&msg, frame_sequence)`；既有 `reply_encodes_with_frame_sequence_then_writes` 测试在 `:116`；mock 的 `protocol_version` 在 `:90`）

**Interfaces:**
- Consumes：Task 1 的 `ProtocolAdapter::encode_with_version`。
- Produces：`ctx.reply` 按 `conn.protocol_version()` 版本感知编码——V2.0 连接回 V2.0 应答，V3.0/单版本逐字节不变。

- [ ] **Step 1：改 reply + 更新既有测试断言**

把 `crates/rsms-business/src/message_context.rs` 的 `reply`（`:53`）：

```rust
        let bytes = self.adapter.encode(&msg, self.frame_sequence)?;
```
改为：
```rust
        let bytes = self
            .adapter
            .encode_with_version(&msg, self.frame_sequence, self.conn.protocol_version().await)?;
```

> `self.conn` 已 impl `ProtocolConnection`（含 `async fn protocol_version(&self) -> Option<u8>`，与既有 `reply` 同处可见）。doc 注释（`:50-51`）把「等价于手工 `adapter.encode(...)`」更新为「等价于 `adapter.encode_with_version(..., conn.protocol_version())`」。

更新既有测试 `reply_encodes_with_frame_sequence_then_writes`（`:116`）：核对 mock 的 `protocol_version()`（`:90`）返回值——
- 若返回 `None`：mock adapter 的 `encode_with_version(None)` 默认转 `encode`，原断言 `reply 字节 == adapter.encode(msg, frame_sequence)` 仍成立，**无需改断言**（仅确认通过）。
- 若返回 `Some(v)` 且 mock adapter 未 override `encode_with_version`：默认仍转 `encode`，断言成立。
- 若返回 `Some(v)` 且 mock adapter override 了：把断言右侧改为 `adapter.encode_with_version(&msg, frame_sequence, Some(v))` 以保持语义一致、不放宽。

按 mock 实际情况择一处理，保证测试真验证「reply == encode_with_version(msg, frame_sequence, conn.protocol_version())」。

- [ ] **Step 2：跑 business 单测 + 编译**

Run（WSL）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-business 2>&1 | grep -E 'test result|FAILED|error' | tail -20"
```
Expected：`test result: ok`。

- [ ] **Step 3：四协议集成测试零回归（关键：V3.0/单版本 ctx.reply 必须逐字节不变）**

Run（WSL）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test cmpp-integration --test smgp-integration --test smpp-integration --test sgip-integration 2>&1 | grep -E 'test result|FAILED|error' | tail -30"
```
Expected：全部 `test result: ok`。WP4-1/4-2 的 `ctx.reply` 用例全走 V3.0/单版本→`encode_with_version` 默认转 `encode`→零回归。

- [ ] **Step 4：clippy + commit**

```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy -p rsms-business 2>&1 | grep -E 'warning:|error' | tail -10"
```
Expected：无告警。然后（Git Bash）：
```bash
git add crates/rsms-business/src/message_context.rs
git commit -m "feat(wp4-3a-followup): ctx.reply 改用 encode_with_version 传 protocol_version（版本感知应答）

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3：V2.0 端到端改用 ctx.reply（证明 D1b + 消除手动 encode 特例）

**Files:**
- Modify: `tests/cmpp/cmpp_test.rs`（`V20AwareBusinessHandler`——WP4-3a Task4 引入，现用 `CmppAdapter.encode_with_version(.., CmppVersion::V20)` + 手动 `write_frame` 回 V2.0 SubmitResp；`test_cmpp_v20_new_path_e2e` 用例）

**Interfaces:**
- Consumes：Task 2 的版本感知 `ctx.reply`。
- Produces：证明 V2.0 连接下 `ctx.reply(SubmitResp)` 自动产出 V2.0（21B）应答的端到端回归测试，并移除手动 encode 特例。

- [ ] **Step 1：把 V20AwareBusinessHandler 改用 ctx.reply**

在 `tests/cmpp/cmpp_test.rs` 的 `V20AwareBusinessHandler::on_message`：删去「读 `ctx.conn.protocol_version()` + `CmppAdapter.encode_with_version(.., CmppVersion::V20)` + `ctx.conn.write_frame`」整段手动回包，替换为一行 `ctx.reply(UnifiedMessage::SubmitResp(<原相同字段>)).await?;`（SubmitResp 的字段值保持与原手动构造一致）。删除因此不再使用的 import（如 `CmppVersion`、`CmppAdapter`，按编译器提示，若该文件其他用例仍用则保留）。

> 关键：`test_cmpp_v20_new_path_e2e` 的客户端仍以 V2.0 握手，故服务端 `MessageContext.conn.protocol_version()==Some(0x20)`，新版 `ctx.reply` 会自动经 `encode_with_version(Some(0x20))` 产出 V2.0（21B）SubmitResp。第三段断言 `get_submit_status()==Some(0)` 不变——证明客户端仍收到正确 V2.0 应答。

- [ ] **Step 2：跑 cmpp-integration 确认通过**

Run（WSL）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test cmpp-integration 2>&1 | grep -E 'test result|FAILED|error' | tail -20"
```
Expected：`test result: ok`（含 `test_cmpp_v20_new_path_e2e`）——`ctx.reply` 自动回 V2.0，断言不放宽仍通过。

> 可选必要性自检：若想坐实 D1b 真生效，临时把 Task 2 的 `reply` 改回 `adapter.encode(...)` 跑本测试，应观察第三段断言 FAIL（客户端 V2.0 解码器读到 24B V3.0 应答→丢帧→`None`），再改回。**不提交临时改动**。

- [ ] **Step 3：clippy + commit**

```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy -p rsms-tests --test cmpp-integration 2>&1 | grep -E 'warning:|error' | tail -10"
```
Expected：无告警。然后（Git Bash）：
```bash
git add tests/cmpp/cmpp_test.rs
git commit -m "test(wp4-3a-followup): V2.0 端到端改用 ctx.reply 自动回 V2.0，消除手动 encode 特例

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 4：收口——四协议压测零丢失 + 全工作区回归 + spec 增补 D1b

**Files:**
- Modify: `docs/superpowers/specs/2026-06-29-wp4-3-retire-bridge-design.md`（增补 D1b）

**Interfaces:** Consumes Task 1–3 全部改动。Produces：3a-followup 验收证据 + spec 记录 D1b。

- [ ] **Step 1：全工作区 lib 回归 + clippy**

Run（WSL）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test --workspace --lib 2>&1 | grep -E 'test result|error' | tail -30"
wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy --workspace 2>&1 | grep -E 'warning:|error' | tail -20"
```
Expected：全绿、clippy 零告警。

- [ ] **Step 2：四协议 multi-account 压测零丢失（证明 ctx.reply 改动不回归）**

Run（WSL，逐条；压测慢、端口 flaky 单独重跑）：
```bash
wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && echo '===cmpp===' && cargo test -p rsms-tests --test cmpp-multi-account-stress-test -- --nocapture 2>&1 | grep -E 'test result|unmatched|loss|FAILED' | tail -8 && echo '===smgp===' && cargo test -p rsms-tests --test smgp-multi-account-stress-test -- --nocapture 2>&1 | grep -E 'test result|unmatched|loss|FAILED' | tail -8 && echo '===smpp===' && cargo test -p rsms-tests --test smpp-multi-account-stress-test -- --nocapture 2>&1 | grep -E 'test result|unmatched|loss|FAILED' | tail -8 && echo '===sgip===' && cargo test -p rsms-tests --test sgip-multi-account-stress-test -- --nocapture 2>&1 | grep -E 'test result|unmatched|loss|FAILED' | tail -8"
```
Expected：四协议均 `test result: ok`、`unmatched: 0`（零丢失）。
> 注：用字面 target 名链式、勿用 `for` 循环变量（嵌套 wsl 引号下 `${t}` 不展开）。

- [ ] **Step 3：spec 增补 D1b**

在 `docs/superpowers/specs/2026-06-29-wp4-3-retire-bridge-design.md` 的 §3（已拍板设计决策）加一条，并在 §4 WP4-3a 范围注明 followup：

```markdown
- **D1b 版本感知编码内化（WP4-3a-followup，2026-06-29 追加）**：`ProtocolAdapter` 加 `encode_with_version(&self, msg, seq, version: Option<u8>)` 默认转 `encode`，仅 CMPP override；`MessageContext::reply` 改传 `conn.protocol_version()`。使 V2.0 服务端 `ctx.reply` 自动回 V2.0 应答，消除手动 `encode_with_version` 特例。WP4-3a scope 从「仅 decode 版本感知」扩为「decode+encode 对称」。**前置于 WP4-3b**：3b 迁 cmpp20_test/stress 可统一用 `ctx.reply`、不留手动 encode 特例。
```

- [ ] **Step 4：commit**

（Git Bash）：
```bash
git add docs/superpowers/specs/2026-06-29-wp4-3-retire-bridge-design.md
git commit -m "docs(wp4-3a-followup): spec 增补 D1b 版本感知编码内化

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

- [ ] **Step 5：更新 ledger**

在 `.superpowers/sdd/progress.md` 追加 WP4-3a-followup 完成行（Task 1–4 + 验收证据），按需更新记忆 [[onboarding-ergonomics-reshape]]。

---

## Self-Review（计划自查结论）

- **覆盖**：D1b 三处改动全覆盖——trait `encode_with_version`（Task1）+ `ctx.reply` 接 version（Task2）+ V2.0 端到端用 `ctx.reply`（Task3）+ 收口/spec（Task4）。
- **占位符**：测试 fixture（`sample_submit_resp`/`sample_smgp_submit_resp`）明确指向「复用既有模块 encode 测试 fixture」，因既有代码库测试样板成熟；框架/codec/ctx.reply 改动均给逐字代码与精确签名（`unified_to_cmpp_with_version(msg, seq, CmppVersion)`、`CmppVersion::from_wire`）。
- **类型一致**：`encode_with_version(&self, msg: &UnifiedMessage, seq: Sequence, version: Option<u8>) -> Result<Vec<u8>>` 在 Task1 定义、Task2（ctx.reply）一致消费；`conn.protocol_version() -> Option<u8>`（async）一致。
- **最大风险已前置**：V3.0/单版本零回归由 Task1 等价单测 + Task2 四协议 integration + Task4 四压测三层钉死；CMPP inherent 同名方法遮蔽问题沿用 WP4-3a Task1 的 `&dyn` 测试手法。

## 执行交接

计划存 `docs/superpowers/plans/2026-06-29-wp4-3a-followup-reply-version-encode.md`。推荐 **Subagent-Driven**：每 task 派新 subagent + 两段式评审；Task4 动压测必实跑四协议 multi-account 零丢失。关联记忆：[[onboarding-ergonomics-reshape]]、[[git-remote-via-wsl]]、[[stress-test-port-flaky]]。
