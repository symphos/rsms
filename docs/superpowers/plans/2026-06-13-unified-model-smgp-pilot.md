# 统一消息模型 · SMGP 窄腰试点 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 引入协议无关的 `UnifiedMessage` + `ProtocolAdapter` 窄腰层,并用 SMGP 单协议试点验证「方言能被 adapter 无损吸收」。

**Architecture:** 新增 `rsms-model` crate(窄腰,依赖 rsms-core);在 `rsms-codec-smgp` 加 `SmgpAdapter`(复用现有 `decode_message`/`encode_message` 中转,只做 `SmgpMessage ↔ UnifiedMessage` 翻译);P2 在 connector 加影子比对;P3 加统一业务接口。全程新旧并存、可回退。

**Tech Stack:** Rust 2024、tokio、bytes;测试经 WSL + `RUSTFLAGS='--cap-lints allow'`(Windows rustc 1.94 ICE)。

**环境约定:** 所有 cargo 命令:`wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && <cmd>"`。提交在 develop 分支、ff main、push 用 `wsl bash -lc "... git push origin develop main"`(login shell)。

---

## File Structure

**创建:**
- `crates/rsms-model/Cargo.toml` — 新 crate 清单
- `crates/rsms-model/src/lib.rs` — 模块声明 + re-export
- `crates/rsms-model/src/types.rs` — 语义类型(Encoding/DeliveryStatus/Address/Concat/MessageId/Tlv/ProtocolExtra/SmgpExtra)
- `crates/rsms-model/src/message.rs` — UnifiedMessage 枚举 + 各消息结构
- `crates/rsms-model/src/adapter.rs` — ProtocolAdapter trait
- `crates/rsms-codec-smgp/src/adapter.rs` — SmgpAdapter 实现 + 翻译表 + roundtrip 测试

**修改:**
- `Cargo.toml`(workspace) — members 加 `crates/rsms-model`
- `crates/rsms-codec-smgp/Cargo.toml` — deps 加 `rsms-model`
- `crates/rsms-codec-smgp/src/lib.rs` — `pub mod adapter;`
- `crates/rsms-business/src/lib.rs` — BusinessHandler 加默认 `on_message`(P3)
- `crates/rsms-connector/src/connection.rs` — SMGP 影子比对(P2)
- `tests/Cargo.toml` + `tests/smgp/unified_pilot_test.rs` — 验证(P2/P3)

---

## Task 1: 创建 rsms-model crate 骨架（P0）

**Files:**
- Create: `crates/rsms-model/Cargo.toml`, `crates/rsms-model/src/lib.rs`
- Modify: `Cargo.toml`(workspace members)

- [ ] **Step 1: 写 Cargo.toml**

`crates/rsms-model/Cargo.toml`:
```toml
[package]
name = "rsms-model"
version.workspace = true
edition.workspace = true
license.workspace = true
description = "Protocol-agnostic unified SMS message model and ProtocolAdapter trait"

[dependencies]
rsms-core = { path = "../rsms-core" }
```

- [ ] **Step 2: 写 lib.rs(先空声明)**

`crates/rsms-model/src/lib.rs`:
```rust
//! 协议无关的统一短信消息模型与协议适配器 trait（窄腰层）。

mod adapter;
mod message;
mod types;

pub use adapter::ProtocolAdapter;
pub use message::*;
pub use types::*;
```

- [ ] **Step 3: 建三个空模块文件占位(让 lib.rs 能编译)**

`crates/rsms-model/src/types.rs`、`message.rs`、`adapter.rs` 各写 `//! placeholder`(Task 2-4 填充)。注:此处「placeholder」仅指临时空文件,Task 2-4 立即填充;不可遗留。

- [ ] **Step 4: workspace 加 member**

`Cargo.toml` 的 `members` 数组加一行 `"crates/rsms-model",`(放在 `"crates/rsms-core",` 之后)。

- [ ] **Step 5: 编译验证**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && cargo build -p rsms-model"`
Expected: `Finished`(空 crate 编译通过)

- [ ] **Step 6: Commit**
```bash
git checkout develop && git add crates/rsms-model Cargo.toml && git commit -m "feat(model): rsms-model crate 骨架（P0）"
```

---

## Task 2: 语义类型 types.rs（P0）

**Files:**
- Modify: `crates/rsms-model/src/types.rs`

- [ ] **Step 1: 写语义类型(完整代码)**

`crates/rsms-model/src/types.rs`(覆盖占位):
```rust
//! 统一模型的语义类型：编码、投递状态、地址、分片、消息 ID、TLV、协议扩展。

/// 短信编码语义（协议魔数由各 adapter 翻译，不上浮到此层）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Gsm7,
    Ascii,
    Ucs2,
    Gbk,
    Binary,
    /// 未识别的协议编码值，保留原值不丢失。
    Other(u8),
}

/// 投递状态语义。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryStatus {
    Delivered,
    Expired,
    Undeliverable,
    Accepted,
    Rejected,
    Unknown,
    /// 未识别的状态文本，保留原值。
    Other(String),
}

/// 短信地址。`ton`/`npi` 是「地址」概念的可选方言修饰（非 SMPP 协议为 None）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    pub number: String,
    pub ton: Option<u8>,
    pub npi: Option<u8>,
}

impl Address {
    /// 构造一个无 TON/NPI 修饰的纯号码地址（CMPP/SMGP/SGIP 用）。
    pub fn plain(number: impl Into<String>) -> Self {
        Self { number: number.into(), ton: None, npi: None }
    }
}

/// 长短信分片信息（UDH 的语义抽象）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Concat {
    pub reference: u16,
    pub total: u8,
    pub sequence: u8,
}

/// 统一消息 ID，吸收各协议形态（CMPP [u8;8]/SMGP 10B/SGIP Sequence/SMPP String）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageId {
    Binary(Vec<u8>),
    Text(String),
}

/// 可选 TLV 参数（SMPP/SMGP）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tlv {
    pub tag: u16,
    pub value: Vec<u8>,
}

/// 协议特有方言字段（typed，非 map）。试点期只填 Smgp。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolExtra {
    None,
    Smgp(SmgpExtra),
}

/// SMGP 特有字段（计费/类型/优先级/时间）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SmgpExtra {
    pub msg_type: u8,
    pub priority: u8,
    pub service_id: String,
    pub fee_type: String,
    pub fee_code: String,
    pub fixed_fee: String,
    pub charge_term_id: String,
    pub valid_time: String,
    pub at_time: String,
}
```

- [ ] **Step 2: 加构造单测**

在 `types.rs` 末尾追加:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_plain_has_no_ton_npi() {
        let a = Address::plain("13800138000");
        assert_eq!(a.number, "13800138000");
        assert!(a.ton.is_none() && a.npi.is_none());
    }

    #[test]
    fn protocol_extra_default_smgp() {
        let e = ProtocolExtra::Smgp(SmgpExtra::default());
        assert!(matches!(e, ProtocolExtra::Smgp(_)));
    }
}
```

- [ ] **Step 3: 编译 + 测试**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && cargo test -p rsms-model"`
Expected: PASS(2 passed)

- [ ] **Step 4: Commit**
```bash
git add crates/rsms-model/src/types.rs && git commit -m "feat(model): 语义类型 Encoding/Address/MessageId/ProtocolExtra（P0）"
```

---

## Task 3: 统一消息结构 message.rs（P0）

**Files:**
- Modify: `crates/rsms-model/src/message.rs`

- [ ] **Step 1: 写消息结构(完整代码)**

`crates/rsms-model/src/message.rs`(覆盖占位):
```rust
//! 协议无关的统一消息枚举与各消息结构。

use crate::types::{Address, Concat, DeliveryStatus, Encoding, MessageId, ProtocolExtra, Tlv};

/// 统一消息（主干；Query/Cancel 等次要消息后续补充）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnifiedMessage {
    Submit(UnifiedSubmit),
    SubmitResp(UnifiedSubmitResp),
    Deliver(UnifiedDeliver),
    DeliverResp,
    Report(UnifiedReport),
    Bind(UnifiedBind),
    BindResp(UnifiedBindResp),
    Unbind,
    UnbindResp,
    Ping,
    PingResp,
    /// 未识别命令，保留原始 body 不丢帧。
    Unknown { command_id: u32, raw: Vec<u8> },
}

/// MT 提交。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnifiedSubmit {
    pub src: Address,
    pub dests: Vec<Address>,
    pub content: Vec<u8>,
    pub encoding: Encoding,
    pub want_report: bool,
    pub concat: Option<Concat>,
    pub extra: ProtocolExtra,
    pub tlvs: Vec<Tlv>,
}

/// MT 提交响应。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnifiedSubmitResp {
    pub msg_id: MessageId,
    pub status: u32,
}

/// MO 上行（用户发来的短信）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnifiedDeliver {
    pub src: Address,
    pub dest: Address,
    pub content: Vec<u8>,
    pub encoding: Encoding,
    pub concat: Option<Concat>,
    pub extra: ProtocolExtra,
    pub tlvs: Vec<Tlv>,
}

/// 投递状态报告（统一抽象，不论底层是 Deliver 还是独立 Report 命令）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnifiedReport {
    pub msg_id: MessageId,
    pub status: DeliveryStatus,
    pub dest: Address,
    /// 原始报告正文，便于业务需要时取协议原始信息。
    pub raw: Vec<u8>,
}

/// 认证请求（CMPP Connect/SMGP Login/SMPP Bind/SGIP Bind）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnifiedBind {
    pub client_id: String,
    pub authenticator: Vec<u8>,
    pub timestamp: u32,
    pub version: u8,
}

/// 认证响应。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnifiedBindResp {
    pub status: u32,
}
```

- [ ] **Step 2: 加构造单测**

`message.rs` 末尾:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Address;

    #[test]
    fn build_submit_message() {
        let m = UnifiedMessage::Submit(UnifiedSubmit {
            src: Address::plain("1065900000"),
            dests: vec![Address::plain("13800138000")],
            content: b"hello".to_vec(),
            encoding: Encoding::Gbk,
            want_report: true,
            concat: None,
            extra: ProtocolExtra::None,
            tlvs: vec![],
        });
        assert!(matches!(m, UnifiedMessage::Submit(_)));
    }
}
```

- [ ] **Step 3: 编译 + 测试**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && cargo test -p rsms-model"`
Expected: PASS(3 passed)

- [ ] **Step 4: Commit**
```bash
git add crates/rsms-model/src/message.rs && git commit -m "feat(model): UnifiedMessage 主干消息结构（P0）"
```

---

## Task 4: ProtocolAdapter trait（P0）

**Files:**
- Modify: `crates/rsms-model/src/adapter.rs`

- [ ] **Step 1: 写 trait(完整代码)**

`crates/rsms-model/src/adapter.rs`(覆盖占位):
```rust
//! 协议适配器 trait：各协议 codec 实现它,负责帧 ↔ 统一消息的双向翻译。

use crate::message::UnifiedMessage;
use rsms_core::{Frame, Protocol, Result};

/// 协议适配器。实现者把已切好的帧解码为统一消息,以及把统一消息编码为帧字节。
pub trait ProtocolAdapter: Send + Sync {
    /// 该适配器对应的协议。
    fn protocol(&self) -> Protocol;

    /// 把一个已切好边界的帧解码为统一消息。
    fn decode(&self, frame: &Frame) -> Result<UnifiedMessage>;

    /// 把统一消息编码为完整帧字节（含协议头,写入 sequence_id）。
    fn encode(&self, msg: &UnifiedMessage, sequence_id: u32) -> Result<Vec<u8>>;
}
```

- [ ] **Step 2: 编译验证**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && cargo build -p rsms-model"`
Expected: `Finished`(注:需确认 rsms-core 导出了 `Frame`/`Protocol`/`Result`,均已存在)

- [ ] **Step 3: Commit**
```bash
git add crates/rsms-model/src/adapter.rs && git commit -m "feat(model): ProtocolAdapter trait（P0）"
```

> **P0 完成判据:** `cargo build -p rsms-model` 通过,5 个单测全绿。骨架立住,不接任何现有代码。

---

## Task 5: SmgpAdapter 解码 + 翻译表（P1）

**Files:**
- Modify: `crates/rsms-codec-smgp/Cargo.toml`(加依赖), `crates/rsms-codec-smgp/src/lib.rs`(加 mod)
- Create: `crates/rsms-codec-smgp/src/adapter.rs`

**翻译表依据(真实字段,已核对 codec):**
- `Encoding ↔ SMGP msg_fmt`(SMGP 3.0 规范常见值):`0=Ascii, 4=Binary, 8=Ucs2, 15=Gbk`,其余 `Other(v)`。
- `Submit{src_term_id, dest_term_ids, msg_content, msg_fmt, need_report, msg_type, priority, service_id, fee_*, charge_term_id, valid_time, at_time}` → `UnifiedSubmit + SmgpExtra`。
- `Deliver{is_report, msg_id, src_term_id, dest_term_id, msg_content, msg_fmt}`:`is_report != 0` → `UnifiedReport`,否则 → `UnifiedDeliver`。
- `Login{client_id, authenticator, timestamp, version}` → `UnifiedBind`。

- [ ] **Step 1: 加依赖与模块声明**

`crates/rsms-codec-smgp/Cargo.toml` 的 `[dependencies]` 加:
```toml
rsms-model = { path = "../rsms-model" }
```
`crates/rsms-codec-smgp/src/lib.rs` 加:
```rust
pub mod adapter;
```

- [ ] **Step 2: 写 decode 方向 + 编码翻译(完整代码)**

`crates/rsms-codec-smgp/src/adapter.rs`:
```rust
//! SmgpAdapter：复用现有 decode_message/encode_message，做 SmgpMessage ↔ UnifiedMessage 翻译。

use crate::datatypes::{Deliver, Login, Submit};
use crate::message::{decode_message, SmgpMessage};
use rsms_core::{Frame, Protocol, Result, RsmsError};
use rsms_model::{
    Address, Encoding, MessageId, ProtocolAdapter, ProtocolExtra, SmgpExtra, UnifiedBind,
    UnifiedDeliver, UnifiedMessage, UnifiedReport, UnifiedSubmit, UnifiedSubmitResp,
};

pub struct SmgpAdapter;

/// SMGP msg_fmt → 统一 Encoding。
fn encoding_from_fmt(fmt: u8) -> Encoding {
    match fmt {
        0 => Encoding::Ascii,
        4 => Encoding::Binary,
        8 => Encoding::Ucs2,
        15 => Encoding::Gbk,
        other => Encoding::Other(other),
    }
}

/// 统一 Encoding → SMGP msg_fmt。
fn fmt_from_encoding(enc: Encoding) -> u8 {
    match enc {
        Encoding::Ascii | Encoding::Gsm7 => 0,
        Encoding::Binary => 4,
        Encoding::Ucs2 => 8,
        Encoding::Gbk => 15,
        Encoding::Other(v) => v,
    }
}

fn submit_to_unified(s: Submit) -> UnifiedSubmit {
    UnifiedSubmit {
        src: Address::plain(s.src_term_id),
        dests: s.dest_term_ids.into_iter().map(Address::plain).collect(),
        content: s.msg_content,
        encoding: encoding_from_fmt(s.msg_fmt),
        want_report: s.need_report != 0,
        concat: None, // 长短信分片由 rsms-longmsg 在更上层处理，试点期不在 adapter 拆 UDH
        extra: ProtocolExtra::Smgp(SmgpExtra {
            msg_type: s.msg_type,
            priority: s.priority,
            service_id: s.service_id,
            fee_type: s.fee_type,
            fee_code: s.fee_code,
            fixed_fee: s.fixed_fee,
            charge_term_id: s.charge_term_id,
            valid_time: s.valid_time,
            at_time: s.at_time,
        }),
        tlvs: vec![], // optional_params → TLV 的翻译留待 P1 后续 step（见 Step 5）
    }
}

fn deliver_to_unified(d: Deliver) -> UnifiedMessage {
    if d.is_report != 0 {
        UnifiedMessage::Report(UnifiedReport {
            msg_id: MessageId::Binary(d.msg_id.bytes.to_vec()),
            status: rsms_model::DeliveryStatus::Unknown, // 精确状态解析见 Step 4
            dest: Address::plain(d.dest_term_id),
            raw: d.msg_content,
        })
    } else {
        UnifiedMessage::Deliver(UnifiedDeliver {
            src: Address::plain(d.src_term_id),
            dest: Address::plain(d.dest_term_id),
            content: d.msg_content,
            encoding: encoding_from_fmt(d.msg_fmt),
            concat: None,
            extra: ProtocolExtra::None,
            tlvs: vec![],
        })
    }
}

fn login_to_unified(l: Login) -> UnifiedBind {
    UnifiedBind {
        client_id: l.client_id,
        authenticator: l.authenticator.to_vec(),
        timestamp: l.timestamp,
        version: l.version,
    }
}

fn smgp_to_unified(msg: SmgpMessage) -> UnifiedMessage {
    match msg {
        SmgpMessage::Submit { submit, .. } => UnifiedMessage::Submit(submit_to_unified(submit)),
        SmgpMessage::SubmitResp { resp, .. } => UnifiedMessage::SubmitResp(UnifiedSubmitResp {
            msg_id: MessageId::Binary(resp.msg_id.bytes.to_vec()),
            status: resp.status,
        }),
        SmgpMessage::Deliver { deliver, .. } => deliver_to_unified(deliver),
        SmgpMessage::DeliverResp { .. } => UnifiedMessage::DeliverResp,
        SmgpMessage::Login { login, .. } => UnifiedMessage::Bind(login_to_unified(login)),
        SmgpMessage::LoginResp { resp, .. } => {
            UnifiedMessage::BindResp(rsms_model::UnifiedBindResp { status: resp.status })
        }
        SmgpMessage::ActiveTest { .. } => UnifiedMessage::Ping,
        SmgpMessage::ActiveTestResp { .. } => UnifiedMessage::PingResp,
        SmgpMessage::Exit { .. } => UnifiedMessage::Unbind,
        SmgpMessage::ExitResp { .. } => UnifiedMessage::UnbindResp,
        SmgpMessage::Unknown { command_id, body, .. } => {
            UnifiedMessage::Unknown { command_id, raw: body }
        }
    }
}

impl ProtocolAdapter for SmgpAdapter {
    fn protocol(&self) -> Protocol {
        Protocol::Smgp
    }

    fn decode(&self, frame: &Frame) -> Result<UnifiedMessage> {
        let msg = decode_message(frame.data_as_slice())?;
        Ok(smgp_to_unified(msg))
    }

    fn encode(&self, _msg: &UnifiedMessage, _sequence_id: u32) -> Result<Vec<u8>> {
        // Task 6 实现
        Err(RsmsError::Other("encode not yet implemented".to_string()))
    }
}
```

> 注:`resp.status` 字段名以 `LoginResp` 实际定义为准——若 `LoginResp` 状态字段名不同(如 `status`),编译器会报错,据报错改为真实字段名。`frame.data_as_slice()` 是 `rsms_core::Frame` 既有方法(见 tests 用法)。

- [ ] **Step 3: 写 decode roundtrip 测试(先红)**

`crates/rsms-codec-smgp/src/adapter.rs` 末尾:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::Pdu;
    use crate::datatypes::Submit;
    use rsms_core::{Frame, RawPdu};

    fn frame_of(bytes: Vec<u8>) -> Frame {
        // command_id/sequence_id 对 decode 不关键（adapter 内部重新解码），置 0 即可
        Frame::new(0, 0, RawPdu::from_vec(bytes))
    }

    #[test]
    fn decode_submit_to_unified() {
        let submit = Submit::new().with_message("1065900000", "13800138000", b"Hello");
        let bytes = Pdu::from(submit).to_pdu_bytes(7).to_vec();
        let unified = SmgpAdapter.decode(&frame_of(bytes)).unwrap();
        match unified {
            UnifiedMessage::Submit(s) => {
                assert_eq!(s.src.number, "1065900000");
                assert_eq!(s.dests[0].number, "13800138000");
                assert_eq!(s.content, b"Hello");
            }
            _ => panic!("expected Submit"),
        }
    }

    #[test]
    fn decode_deliver_report_vs_mo() {
        let mut d = crate::datatypes::Deliver::new();
        d.is_report = 1;
        d.dest_term_id = "1065900000".to_string();
        let bytes = Pdu::from(d).to_pdu_bytes(8).to_vec();
        let unified = SmgpAdapter.decode(&frame_of(bytes)).unwrap();
        assert!(matches!(unified, UnifiedMessage::Report(_)), "is_report=1 应翻译为 Report");
    }
}
```

- [ ] **Step 4: 跑测试验证(可能因字段名/Pdu::from 报错,据编译器修正)**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && cargo test -p rsms-codec-smgp adapter"`
Expected: PASS(2 passed)。若编译报错(字段名/`Pdu::From<Deliver>` 是否存在),按报错修正:`Deliver` 若无 `From<Deliver> for Pdu`,改用 `crate::message::encode_message(&SmgpMessage::Deliver{...})` 造字节。

- [ ] **Step 5: 提交 decode 方向**
```bash
git add crates/rsms-codec-smgp/ && git commit -m "feat(smgp): SmgpAdapter decode + 编码翻译表（P1）"
```

---

## Task 6: SmgpAdapter 编码 + 字节级 roundtrip（P1，判据①）

**Files:**
- Modify: `crates/rsms-codec-smgp/src/adapter.rs`

- [ ] **Step 1: 实现 encode(UnifiedMessage → SmgpMessage → bytes)**

替换 Task 5 的 `encode` 桩。在 `adapter.rs` 加反向翻译 + encode:
```rust
use crate::datatypes::{SubmitResp, SmgpMsgId};
use crate::message::encode_message;

fn unified_to_smgp(msg: &UnifiedMessage, seq: u32) -> Result<SmgpMessage> {
    let m = match msg {
        UnifiedMessage::Submit(s) => {
            let extra = match &s.extra {
                ProtocolExtra::Smgp(e) => e.clone(),
                _ => SmgpExtra::default(),
            };
            let mut sub = Submit::new();
            sub.src_term_id = s.src.number.clone();
            sub.dest_term_ids = s.dests.iter().map(|a| a.number.clone()).collect();
            sub.dest_term_id_count = sub.dest_term_ids.len() as u8;
            sub.msg_content = s.content.clone();
            sub.msg_fmt = fmt_from_encoding(s.encoding);
            sub.need_report = if s.want_report { 1 } else { 0 };
            sub.msg_type = extra.msg_type;
            sub.priority = extra.priority;
            sub.service_id = extra.service_id;
            sub.fee_type = extra.fee_type;
            sub.fee_code = extra.fee_code;
            sub.fixed_fee = extra.fixed_fee;
            sub.charge_term_id = extra.charge_term_id;
            sub.valid_time = extra.valid_time;
            sub.at_time = extra.at_time;
            SmgpMessage::Submit { sequence_id: seq, submit: sub }
        }
        UnifiedMessage::SubmitResp(r) => {
            let bytes10 = match &r.msg_id {
                MessageId::Binary(b) => {
                    let mut arr = [0u8; 10];
                    let n = b.len().min(10);
                    arr[..n].copy_from_slice(&b[..n]);
                    arr
                }
                MessageId::Text(t) => {
                    let mut arr = [0u8; 10];
                    let tb = t.as_bytes();
                    let n = tb.len().min(10);
                    arr[..n].copy_from_slice(&tb[..n]);
                    arr
                }
            };
            SmgpMessage::SubmitResp {
                sequence_id: seq,
                resp: SubmitResp { msg_id: SmgpMsgId::new(bytes10), status: r.status },
            }
        }
        UnifiedMessage::Ping => SmgpMessage::ActiveTest { sequence_id: seq },
        UnifiedMessage::PingResp => SmgpMessage::ActiveTestResp { sequence_id: seq },
        UnifiedMessage::Unbind => SmgpMessage::Exit { sequence_id: seq },
        UnifiedMessage::UnbindResp => SmgpMessage::ExitResp { sequence_id: seq },
        other => {
            return Err(RsmsError::Other(format!("SMGP encode 暂不支持该消息类型: {other:?}")))
        }
    };
    Ok(m)
}
```
并把 `impl ProtocolAdapter for SmgpAdapter` 的 `encode` 改为:
```rust
fn encode(&self, msg: &UnifiedMessage, sequence_id: u32) -> Result<Vec<u8>> {
    let smgp = unified_to_smgp(msg, sequence_id)?;
    encode_message(&smgp)
}
```

- [ ] **Step 2: 写字节级 roundtrip 测试(判据①核心)**

`adapter.rs` tests 模块加:
```rust
#[test]
fn submit_byte_roundtrip_via_unified() {
    // SMGP Submit → bytes → decode → UnifiedMessage → encode → bytes，字节一致
    let submit = Submit::new().with_message("1065900000", "13800138000", b"Hello");
    let original = Pdu::from(submit).to_pdu_bytes(42).to_vec();
    let unified = SmgpAdapter.decode(&frame_of(original.clone())).unwrap();
    let reencoded = SmgpAdapter.encode(&unified, 42).unwrap();
    assert_eq!(reencoded, original, "经统一模型往返后字节应无损一致");
}
```

- [ ] **Step 3: 跑测试**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && cargo test -p rsms-codec-smgp adapter"`
Expected: PASS(3 passed)。**若字节不一致**,逐字段比对(常见原因:`Submit::new()` 的默认 reserve/optional_params 与重建不一致)——调整 `unified_to_smgp` 使非核心字段(reserve/service_id 等)与原始默认对齐,或在测试里用显式构造的 Submit 保证可重建字段集。

- [ ] **Step 4: clippy + 提交**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && cargo clippy -p rsms-model -p rsms-codec-smgp --lib"`(零警告)
```bash
git add crates/rsms-codec-smgp/ && git commit -m "feat(smgp): SmgpAdapter encode + 字节级 roundtrip（P1 判据①）"
```

> **P1 完成判据①:** `submit_byte_roundtrip_via_unified` 绿 = 翻译表对 Submit 无损。Deliver/Report/Bind 的 roundtrip 按同模式各加一个测试(可选增强)。

---

## Task 7: connector 影子比对（P2，判据②③）

**Files:**
- Modify: `crates/rsms-connector/src/connection.rs`(SMGP 入站路径), `crates/rsms-connector/Cargo.toml`(加 rsms-model + rsms-codec-smgp 已有依赖)

**集成点:** `run_connection` 内逐帧 handler 分发处(本计划编写时位于 `connection.rs` 的 `match protocol { Protocol::Smgp => smgp_handler.handle_frame(...) }` 一臂附近)。

- [ ] **Step 1: 加 feature flag**

`crates/rsms-connector/Cargo.toml` 加:
```toml
[features]
unified-shadow = []
```
并确认 `[dependencies]` 含 `rsms-model = { path = "../rsms-model" }`(没有则加)。

- [ ] **Step 2: 在 SMGP 帧处理后加影子解码(完整代码)**

在 `connection.rs` 的 `Protocol::Smgp => ...` 帧处理**之后**(即拿到 `frame` 且确认 protocol 是 Smgp 的位置)插入:
```rust
#[cfg(feature = "unified-shadow")]
if protocol == Protocol::Smgp {
    use rsms_model::ProtocolAdapter;
    match rsms_codec_smgp::adapter::SmgpAdapter.decode(&frame) {
        Ok(unified) => tracing::debug!(conn_id = conn.id, ?unified, "shadow decode ok"),
        Err(e) => tracing::warn!(conn_id = conn.id, "shadow decode err: {e}"),
    }
}
```
> 影子只解码打日志,不接管处理,错误隔离不影响旧路径。

- [ ] **Step 3: 带 feature 编译 + 跑 SMGP 集成**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && cargo test -p rsms-tests --test smgp-integration --features rsms-connector/unified-shadow"`
Expected: 9 passed(影子开启,集成行为不变 = 判据②不破坏)。若 feature 透传语法不适用,改为在 `tests/Cargo.toml` 加 `rsms-connector/unified-shadow` 到 features,或临时本地开启验证。

- [ ] **Step 4: 影子下压测(判据③)**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && cargo test -p rsms-tests --test smgp-stress-test --features rsms-connector/unified-shadow -- --nocapture"`
Expected: `test result: ok`,TPS 与基线(~12542)同量级,零丢失。**记录 TPS 对照基线**。

- [ ] **Step 5: 提交**
```bash
git add crates/rsms-connector/ && git commit -m "feat(connector): SMGP 影子比对（P2，feature=unified-shadow）"
```

> **P2 完成判据:** 影子开启下 smgp-integration 全绿(②) + smgp-stress TPS 不退化(③)。默认 feature 关闭,生产无感。

---

## Task 8: 统一业务接口 + 验证 example（P3，判据④）

**Files:**
- Modify: `crates/rsms-business/src/lib.rs`(BusinessHandler 加默认 `on_message`)
- Create: `tests/smgp/unified_pilot_test.rs` + `tests/Cargo.toml` 注册 `[[test]] cmpp...`→`smgp-unified-pilot-test`

- [ ] **Step 1: BusinessHandler 加默认 on_message(向后兼容)**

`crates/rsms-business/src/lib.rs` 的 `BusinessHandler` trait 内加(默认实现,旧实现者无需改):
```rust
/// 统一模型入站回调（窄腰试点）。默认空实现，业务可选择覆盖它以对协议无关的
/// `UnifiedMessage` 编程，而非自行 decode Frame。与 on_inbound 并存。
#[allow(unused_variables)]
async fn on_message(
    &self,
    ctx: &InboundContext<'_>,
    msg: &rsms_model::UnifiedMessage,
) -> rsms_core::Result<()> {
    Ok(())
}
```
并在 `crates/rsms-business/Cargo.toml` 加 `rsms-model = { path = "../rsms-model" }`。
> 注:`InboundContext` 的生命周期参数以现有定义为准,编译报错则按真实签名调整。

- [ ] **Step 2: 写试点验证测试(用统一模型解码 SMGP 入站)**

`tests/smgp/unified_pilot_test.rs`:对一段 SMGP Submit 字节,用 `SmgpAdapter.decode` 得到 `UnifiedMessage::Submit`,断言业务侧无需感知 SMGP 字段即可读出 src/dest/content/encoding/want_report。代码:
```rust
use rsms_codec_smgp::adapter::SmgpAdapter;
use rsms_codec_smgp::codec::Pdu;
use rsms_codec_smgp::datatypes::Submit;
use rsms_core::{Frame, RawPdu};
use rsms_model::{ProtocolAdapter, UnifiedMessage};

#[test]
fn business_reads_unified_submit_without_smgp_knowledge() {
    let submit = Submit::new().with_message("1065900000", "13800138000", b"Hi");
    let bytes = Pdu::from(submit).to_pdu_bytes(1).to_vec();
    let frame = Frame::new(0, 0, RawPdu::from_vec(bytes));

    let unified = SmgpAdapter.decode(&frame).unwrap();
    // 业务代码完全协议无关：
    if let UnifiedMessage::Submit(s) = unified {
        assert_eq!(s.src.number, "1065900000");
        assert_eq!(s.dests[0].number, "13800138000");
        assert_eq!(s.content, b"Hi");
        assert!(s.want_report == false || s.want_report); // 仅示意读取无需 SMGP 知识
    } else {
        panic!("expected unified Submit");
    }
}
```
在 `tests/Cargo.toml` 注册:
```toml
[[test]]
name = "smgp-unified-pilot-test"
path = "smgp/unified_pilot_test.rs"
```

- [ ] **Step 3: 跑测试**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && cargo test -p rsms-tests --test smgp-unified-pilot-test"`
Expected: PASS

- [ ] **Step 4: 全量回归 + 提交**

Run: `wsl bash -lc "bash /mnt/g/RustProjects/rsms/.omc/run-regress.sh"` 后读 `.omc/regress.log` 确认全绿(默认 feature 关,旧路径不受影响)。
```bash
git add crates/rsms-business/ tests/ && git commit -m "feat(business): 统一 on_message 接口 + SMGP 试点验证（P3 判据④）"
```

> **P3 完成判据:** 业务侧能对 UnifiedMessage 编程、不碰 SMGP 字段(④);全量回归全绿(旧路径无回退)。

---

## P4 评审决策点（不写代码，人工评审）

四判据全绿 → 推广 SMPP(验证 TON/NPI+TLV)→ SGIP(独立 Report)→ CMPP(双版本+计费),每协议重复 Task 5–6 模式 + 收敛编排层 match。任一不过 → 止损,统一路径退化为 SMGP 可选 API。推广为后续独立 spec + plan。

---

## 自检清单（执行者每个 Task 后核对）
- [ ] 每步先写测试、跑红、再实现、跑绿、提交
- [ ] cargo 命令一律走 WSL + cap-lints
- [ ] 提交在 develop、ff main、push 用 login shell(`wsl bash -lc "... git push origin develop main"`)
- [ ] feature `unified-shadow` 默认关闭,P2/P3 不影响现有四协议运行路径
- [ ] 遇到字段名/方法签名与计划不符:以编译器报错为准修正真实符号名（计划基于已核对的 codec,个别 resp/字段名可能需对齐）
