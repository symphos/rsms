# WP1 · UnifiedMessage::ReportResp 补全 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 给统一模型新增 `UnifiedMessage::ReportResp` 变体，让对接方对「收到的投递报告」能用协议无关的一句话回执，并消除 SGIP 当前的 `Unknown` 兜底。

**Architecture:** `ReportResp` 做成无字段 unit 变体（对标已有的 `DeliverResp`）。四协议各做对的事：SGIP 编/解为真正的独立 `Report_Resp`（command_id `0x80000005`）；CMPP/SMGP/SMPP 因报告经 `Deliver`/`DeliverSm` 承载，故 `ReportResp` 等价各自的 `DeliverResp`。这样业务统一 `reply(UnifiedMessage::ReportResp)` 在四协议都生成正确帧。

**Tech Stack:** Rust（edition 2024，1.85+）、Cargo workspace、`async_trait`、`bytes`。

## Global Constraints

- 全程思考与输出**用中文**（仅代码语法关键词除外）——`AGENTS.md` 规定。
- 公共 API 必须有 `///` / `//!` 文档注释。
- `cargo clippy --workspace` 必须零告警（CONTRIBUTING 要求）。
- **构建/测试一律走 WSL**（Windows 原生无 cargo/rustc）：`wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo ..."`。
- **本地 commit 走 Git Bash**（仓库 `autocrlf=true` + `.gitattributes` 强制 LF；切勿用 WSL git commit，会翻转行尾污染 diff）。
- 工作分支：`feature/onboarding-ergonomics`（已存在，设计文档已在其上）。
- 本 WP 只动 `rsms-model` 与四个 `rsms-codec-*` crate 及 `examples/sgip_*`，不碰 connector 主循环（留待 WP4）。

---

## File Structure

- `crates/rsms-model/src/message.rs` — 新增 `UnifiedMessage::ReportResp` 变体（Task 1）。
- `crates/rsms-codec-sgip/src/adapter.rs` — decode/encode 改用新变体，删 `Unknown` 兜底（Task 2）。
- `crates/rsms-codec-cmpp/src/adapter.rs` — encode 新增 `ReportResp` 分支（Task 3）。
- `crates/rsms-codec-smgp/src/adapter.rs` — 同上（Task 3）。
- `crates/rsms-codec-smpp/src/adapter.rs` — 同上（Task 3）。
- `examples/sgip_client/src/main.rs`、`examples/sgip_server/src/main.rs` — 迁移到新变体（Task 4）。

---

### Task 1: 新增 `UnifiedMessage::ReportResp` 变体

**Files:**
- Modify: `crates/rsms-model/src/message.rs:13`（在 `DeliverResp` 后新增一行）
- Test: `crates/rsms-model/src/message.rs`（文件内 `#[cfg(test)] mod tests`，约 107 行起）

**Interfaces:**
- Produces: `UnifiedMessage::ReportResp`（unit 变体，无字段；语义=对收到的投递报告的响应）。后续 Task 2/3 的 encode 分支依赖此变体存在。

- [ ] **Step 1: 写失败测试**

在 `crates/rsms-model/src/message.rs` 的 `mod tests` 内新增：

```rust
    #[test]
    fn report_resp_variant_exists() {
        // ReportResp 是无字段 unit 变体，可构造、可 match、可比较。
        let m = UnifiedMessage::ReportResp;
        assert!(matches!(m, UnifiedMessage::ReportResp));
        assert_eq!(m, UnifiedMessage::ReportResp);
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-model report_resp_variant_exists"`
Expected: 编译失败 `no variant named ReportResp found for enum UnifiedMessage`。

- [ ] **Step 3: 新增变体**

在 `crates/rsms-model/src/message.rs` 的 `enum UnifiedMessage` 中，`DeliverResp,`（第 13 行）之后、`Report(UnifiedReport),`（第 14 行）之前插入：

```rust
    /// 对「收到的投递报告」的响应。协议无关：SGIP 编为独立 `Report_Resp`；
    /// CMPP/SMGP/SMPP 的报告经 Deliver 承载，故等价各自的 `DeliverResp`。
    ReportResp,
```

- [ ] **Step 4: 运行测试确认通过**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-model report_resp_variant_exists"`
Expected: PASS。（四 adapter 的 encode 均有 `other => Err` 通配分支，新增变体不会破坏其编译。）

- [ ] **Step 5: 提交（Git Bash）**

```bash
git add crates/rsms-model/src/message.rs
git commit -m "feat(model): 新增 UnifiedMessage::ReportResp 变体

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: SGIP encode/decode 改用 `ReportResp`，删 `Unknown` 兜底

**Files:**
- Modify: `crates/rsms-codec-sgip/src/adapter.rs:165-168`（decode）、`:322-325`（encode）
- Test: `crates/rsms-codec-sgip/src/adapter.rs`（文件内 `#[cfg(test)] mod tests`）

**Interfaces:**
- Consumes: `UnifiedMessage::ReportResp`（Task 1）、既有 `ReportResp { result: u32 }`（`crates/rsms-codec-sgip/src/datatypes/deliver.rs`，`to_pdu_bytes(node, ts, num)`）、`CommandId::ReportResp = 0x80000005`。
- Produces: SGIP `decode` 对 `Report_Resp` 帧返回 `UnifiedMessage::ReportResp`；`encode` 对该变体产出合规 `Report_Resp` PDU（header 20B + body 9B = 29B）。

- [ ] **Step 1: 写失败测试**

在 `crates/rsms-codec-sgip/src/adapter.rs` 的 `mod tests` 内新增（文件顶部若缺 `use` 则在 `mod tests` 内 `use super::*;` 基础上补 `use rsms_core::{Frame, RawPdu}; use rsms_model::{UnifiedMessage, types::Sequence};`）：

```rust
    #[test]
    fn report_resp_roundtrip() {
        // SGIP 有独立 Report_Resp 命令：encode→帧字节→decode 应无损回到 ReportResp。
        let seq = Sequence::Sgip { node_id: 1, timestamp: 2, number: 3 };
        let bytes = SgipAdapter.encode(&UnifiedMessage::ReportResp, seq).unwrap();
        let frame = Frame::new(0, 0, RawPdu::new(bytes::Bytes::copy_from_slice(&bytes)));
        assert_eq!(SgipAdapter.decode(&frame).unwrap(), UnifiedMessage::ReportResp);
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-codec-sgip report_resp_roundtrip"`
Expected: FAIL —— decode 现把 `Report_Resp` 退化为 `Unknown{command_id}`，断言 `Unknown != ReportResp` 失败。

- [ ] **Step 3a: 改 decode**

`crates/rsms-codec-sgip/src/adapter.rs:165-168`，把：

```rust
        SgipMessage::ReportResp(_) => UnifiedMessage::Unknown {
            command_id: CommandId::ReportResp as u32,
            raw: vec![],
        },
```

替换为：

```rust
        // 独立 Report 命令的响应：统一模型已有 ReportResp 变体，直接映射，不再退化为 Unknown。
        SgipMessage::ReportResp(_) => UnifiedMessage::ReportResp,
```

- [ ] **Step 3b: 改 encode**

`crates/rsms-codec-sgip/src/adapter.rs:322-325`，把：

```rust
        UnifiedMessage::Unknown { command_id, .. }
            if *command_id == CommandId::ReportResp as u32 =>
        {
            ReportResp { result: 0 }.to_pdu_bytes(node, ts, num)
        }
```

替换为：

```rust
        // SGIP 收到独立 Report 必须回 Report_Resp（result=0 表示成功接收）。
        UnifiedMessage::ReportResp => ReportResp { result: 0 }.to_pdu_bytes(node, ts, num),
```

- [ ] **Step 4: 运行测试确认通过**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-codec-sgip"`
Expected: `report_resp_roundtrip` PASS，其余既有测试不回归。

- [ ] **Step 5: 提交（Git Bash）**

```bash
git add crates/rsms-codec-sgip/src/adapter.rs
git commit -m "feat(sgip): Report_Resp 映射到 UnifiedMessage::ReportResp，删 Unknown 兜底

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: CMPP/SMGP/SMPP encode 支持 `ReportResp`（等价 `DeliverResp`）

**Files:**
- Modify: `crates/rsms-codec-cmpp/src/adapter.rs:360`（`DeliverResp` 分支后）
- Modify: `crates/rsms-codec-smgp/src/adapter.rs:310`（`DeliverResp` 分支后）
- Modify: `crates/rsms-codec-smpp/src/adapter.rs:295`（`DeliverResp` 分支后）
- Test: 三个 adapter.rs 各自的 `mod tests`

**Interfaces:**
- Consumes: `UnifiedMessage::ReportResp`（Task 1）、各协议既有 `DeliverResp` 编码路径。
- Produces: 三协议 `encode(&UnifiedMessage::ReportResp, seq)` 字节 == `encode(&UnifiedMessage::DeliverResp, seq)`。

- [ ] **Step 1: 写失败测试（三协议各一个）**

CMPP —— `crates/rsms-codec-cmpp/src/adapter.rs` 的 `mod tests` 内：

```rust
    #[test]
    fn report_resp_equals_deliver_resp() {
        // CMPP 无独立 Report_Resp：报告经 Deliver 承载，故对报告的响应即 DeliverResp。
        let seq = Sequence::Plain(42);
        let a = CmppAdapter.encode(&UnifiedMessage::ReportResp, seq).unwrap();
        let b = CmppAdapter.encode(&UnifiedMessage::DeliverResp, seq).unwrap();
        assert_eq!(a, b);
    }
```

SMGP —— `crates/rsms-codec-smgp/src/adapter.rs` 的 `mod tests` 内（同上，把 `CmppAdapter` 换 `SmgpAdapter`）：

```rust
    #[test]
    fn report_resp_equals_deliver_resp() {
        let seq = Sequence::Plain(42);
        let a = SmgpAdapter.encode(&UnifiedMessage::ReportResp, seq).unwrap();
        let b = SmgpAdapter.encode(&UnifiedMessage::DeliverResp, seq).unwrap();
        assert_eq!(a, b);
    }
```

SMPP —— `crates/rsms-codec-smpp/src/adapter.rs` 的 `mod tests` 内（换 `SmppAdapter`）：

```rust
    #[test]
    fn report_resp_equals_deliver_resp() {
        let seq = Sequence::Plain(42);
        let a = SmppAdapter.encode(&UnifiedMessage::ReportResp, seq).unwrap();
        let b = SmppAdapter.encode(&UnifiedMessage::DeliverResp, seq).unwrap();
        assert_eq!(a, b);
    }
```

> 各 `mod tests` 若缺 `use`，补 `use rsms_model::{UnifiedMessage, types::Sequence};`（`use super::*;` 通常已引入 adapter 类型）。

- [ ] **Step 2: 运行测试确认失败**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-codec-cmpp -p rsms-codec-smgp -p rsms-codec-smpp report_resp_equals_deliver_resp"`
Expected: FAIL —— `ReportResp` 当前落到 `other => Err(...)`，`encode(...).unwrap()` panic。

- [ ] **Step 3a: CMPP 加分支**

`crates/rsms-codec-cmpp/src/adapter.rs:360`，在 `UnifiedMessage::DeliverResp => CmppMessage::DeliverResp { ... },`（355-360 行）之后插入：

```rust
        // CMPP 报告经 Deliver(registered_delivery=1) 承载，对报告的响应即 DeliverResp。
        UnifiedMessage::ReportResp => CmppMessage::DeliverResp {
            version,
            sequence_id: seq,
            resp: DeliverResp { msg_id: [0u8; 8], result: 0 },
        },
```

- [ ] **Step 3b: SMGP 加分支**

`crates/rsms-codec-smgp/src/adapter.rs:310`，在 `UnifiedMessage::DeliverResp => SmgpMessage::DeliverResp { ... },`（306-310 行）之后插入：

```rust
        // SMGP 报告经 Deliver(is_report=1) 承载，对报告的响应即 DeliverResp。
        UnifiedMessage::ReportResp => SmgpMessage::DeliverResp {
            sequence_id: seq,
            resp: DeliverResp { msg_id: SmgpMsgId::default(), status: 0 },
        },
```

> 注：照抄同文件 `DeliverResp` 分支（306-310 行）的确切右值，保持 `DeliverResp`/`SmgpMsgId` 的 import 与字段名与该分支一致。

- [ ] **Step 3c: SMPP 加分支**

`crates/rsms-codec-smpp/src/adapter.rs:295`，在 `UnifiedMessage::DeliverResp => ...`（第 295 行，产 `DeliverSmResp { message_id: String::new() }`）之后插入：

```rust
        // SMPP 报告经 DeliverSm(esm_class=0x04) 承载，对报告的响应即 DeliverSmResp。
        UnifiedMessage::ReportResp => Pdu::from(DeliverSmResp { message_id: String::new() }),
```

> 注：照抄同文件 `DeliverResp` 分支（第 295 行）的确切右值与 `Pdu::from(...)` 形态，保持一致。

- [ ] **Step 4: 运行测试确认通过**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-codec-cmpp -p rsms-codec-smgp -p rsms-codec-smpp report_resp_equals_deliver_resp"`
Expected: 三个 PASS。

- [ ] **Step 5: clippy + 提交（Git Bash）**

先 `wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy -p rsms-codec-cmpp -p rsms-codec-smgp -p rsms-codec-smpp"` 确认零告警，再：

```bash
git add crates/rsms-codec-cmpp/src/adapter.rs crates/rsms-codec-smgp/src/adapter.rs crates/rsms-codec-smpp/src/adapter.rs
git commit -m "feat(cmpp,smgp,smpp): encode 支持 ReportResp（等价 DeliverResp）

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: 迁移 SGIP examples 到新变体 + 全工作区校验

**Files:**
- Modify: `examples/sgip_client/src/main.rs`、`examples/sgip_server/src/main.rs`

**Interfaces:**
- Consumes: `UnifiedMessage::ReportResp`（Task 1-3 后四协议全支持）。

- [ ] **Step 1: 定位旧兜底用法**

Run（Grep 工具或 WSL）：搜 `Unknown` + `ReportResp` 的构造点与接收 match：
`wsl bash -lc "cd /mnt/g/RustProjects/rsms && grep -rn 'CommandId::ReportResp\|Unknown {' examples/sgip_client/src/main.rs examples/sgip_server/src/main.rs"`
Expected: 命中「回复 Report_Resp 时构造 `UnifiedMessage::Unknown { command_id: CommandId::ReportResp as u32, raw: vec![] }`」以及可能的「接收端 `match` 对 `Unknown`/`Report` 的处理」。

- [ ] **Step 2: 替换构造点**

把每处用于「回复报告」的：

```rust
let resp = UnifiedMessage::Unknown {
    command_id: rsms_codec_sgip::CommandId::ReportResp as u32,
    raw: vec![],
};
let bytes = SgipAdapter.encode(&resp, SgipAdapter.sequence_of(frame))?;
```

替换为：

```rust
let bytes = SgipAdapter.encode(&UnifiedMessage::ReportResp, SgipAdapter.sequence_of(frame))?;
```

接收端若有 `UnifiedMessage::Report(report) => { ... reply_report_resp(...) }`，逻辑不变（仍收 `Report`，回 `ReportResp`）；若存在对 `Unknown { command_id == ReportResp }` 的特判分支，删除或改为 `UnifiedMessage::ReportResp`。删除因此不再用到的 `use rsms_codec_sgip::CommandId;`（若仅此处使用）。

- [ ] **Step 3: 全工作区编译 + 测试**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo build --workspace && RUSTFLAGS='--cap-lints allow' cargo test --workspace --lib"`
Expected: 编译通过、库测试全绿。

- [ ] **Step 4: SGIP 集成测试**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo test -p rsms-tests --test sgip-integration"`
Expected: PASS（报告链路在新变体下不回归）。

- [ ] **Step 5: clippy + 提交（Git Bash）**

先 `wsl bash -lc "cd /mnt/g/RustProjects/rsms && RUSTFLAGS='--cap-lints allow' cargo clippy --workspace"` 零告警，再：

```bash
git add examples/sgip_client/src/main.rs examples/sgip_server/src/main.rs
git commit -m "refactor(sgip-examples): 改用 UnifiedMessage::ReportResp，去掉 Unknown 兜底

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

- **Spec 覆盖**：本 WP 实现 spec §3.5「补 `UnifiedMessage::ReportResp`，消除 SGIP `Unknown` 兜底」。其余 §3.1-3.4 / §4 / §5 / §6 属 WP2-5。
- **占位符扫描**：无 TBD/TODO；每个改动给出确切前后代码与行号。Task 4 Step 2 的「接收端若有…」是条件分支说明，非占位符——给了三种情形的确切处理。
- **类型一致**：`UnifiedMessage::ReportResp`（unit 变体）在 Task 1 定义，Task 2/3/4 一致引用；SGIP `ReportResp { result: u32 }` 与 `to_pdu_bytes(node, ts, num)` 对齐 `adapter.rs:325` 既有用法；CMPP/SMGP/SMPP 各 `DeliverResp` 右值照抄同文件既有分支。

---

## 阶段 1 剩余工作包路线图（WP1 落地后逐个出精确计划）

| WP | 内容 | 依赖 | 关键改动点（已勘探）|
|---|---|---|---|
| **WP2** | `MessageContext`（`reply`/`send`/`channel_key`/`id_generator` 去 `Option`）| WP1 + `adapter_registry::adapter_for` | 新文件或扩 `rsms-business/src/lib.rs`；reply 内部 `adapter.encode(msg, frame_seq)` + `conn.write_frame` |
| **WP3** | `MessageHandler` + `RawFrameHandler` trait | WP2 | 替换 `BusinessHandler`（`rsms-business/src/lib.rs:25`）；builder 接受两类 handler |
| **WP4** | 主循环自动 decode 驱动 + 版本感知内化 + 心跳 resp 收归 | WP1-3 | `connection.rs:415-422` 的 `unified-shadow` decode 转正；`:433-438` 的 `run_chain` 改为 decode 后驱动 `on_message`；版本经 `conn.protocol_version()` 内化 |
| **WP5** | CMPP example 迁移 + 既有集成/压测验证 | WP1-4 | `examples/cmpp_*` 改用新 API，目标 ≤200 行；跑 `cmpp-integration` / `cmpp-stress-test`（WARN 日志）|
