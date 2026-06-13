# 窄腰统一模型推广 · SMPP/SGIP/CMPP + 编排层收敛 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 SMGP 试点验证过的 `UnifiedMessage` + `ProtocolAdapter` 窄腰推广到 SMPP/SGIP/CMPP 三协议，并把编排层散落的 `match protocol` 收敛到单一 `AdapterRegistry`，使「新增协议 = 加一个 adapter」。

**Architecture:** 在 `rsms-model` 扩展 `ProtocolExtra`（加 Smpp/Sgip/Cmpp typed 方言）；在三个 `rsms-codec-*` 各加 `adapter.rs`，完全复刻 `SmgpAdapter` 结构（复用各自 `decode_message`/`Pdu::to_pdu_bytes`，只做 `XxxMessage ↔ UnifiedMessage` 翻译）；在 `rsms-connector` 加 `adapter_registry::adapter_for(Protocol)` 把四 adapter 集中到一处 match，用它统一影子比对 + 关闭包/心跳包编码。全程新旧并存、`unified-shadow` feature 默认关闭、可回退。

**Tech Stack:** Rust 2024、tokio、bytes；测试经 WSL + `RUSTFLAGS='--cap-lints allow'`（Windows rustc ICE 规避）。

**环境约定（与试点一致）:** 所有 cargo 命令走：
`wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && <cmd>"`
提交在 `feature/widen-adapters` 分支（当前分支）；push 用 login shell：`wsl bash -lc "... git push origin feature/widen-adapters"`（Windows 直接 SSH 会失败，须走 WSL）。

**范围边界（评审重点，已与用户确认）:**
- **CMPP 仅 V3.0 进 adapter。** 无状态 `decode_message` 默认 V3.0；V2.0 与 V3.0 的 Submit/Deliver 共用 command_id 但字段长度不同，无握手版本上下文无法区分。V2.0 连接继续走旧 `CmppHandler`（它已按 Connect 握手 version 有状态解析），运行路径不受影响。adapter 的 V2.0 支持留待后续「有状态/参数化 adapter」独立一轮。
- **SGIP 头部 12 字节 `SgipSequence`(node_id/timestamp/number) 不进统一模型。** `decode_message` 本就丢弃 header sequence（message.rs:70 `let _sequence`）。adapter 采用固定约定 `node_id=0, timestamp=0, number=sequence_id`，字节级 roundtrip 测试用同约定构造原始 PDU。真实连接的 SgipSequence 生成在编排层别处，与 adapter 无关。
- **编排层收敛限于「无状态翻译能干净接管」的 match 点**：①集中 `AdapterRegistry`、②四协议统一影子解码、③关闭包/心跳包编码经 adapter（受字节相等回归测试**门控**，任一协议字节不一致则该臂留旧路径并记录）。**入站业务分发（`handle_frame`）保留旧 per-protocol handler**——adapter 是无状态翻译器、不含鉴权/写响应等业务逻辑，替换它属另一轮高风险改造，本轮不做。`create_decoder`（帧切分，不属 `ProtocolAdapter` 职责）保留 match。

---

## File Structure

**修改:**
- `crates/rsms-model/src/types.rs` — `ProtocolExtra` 加 `Smpp/Sgip/Cmpp` 三臂 + 新增 `SmppExtra`/`SgipExtra`/`CmppExtra` 结构 + 单测
- `crates/rsms-codec-smpp/Cargo.toml` / `src/lib.rs` — 加 `rsms-model` 依赖 + `pub mod adapter;`
- `crates/rsms-codec-sgip/Cargo.toml` / `src/lib.rs` — 同上
- `crates/rsms-codec-cmpp/Cargo.toml` / `src/lib.rs` — 同上
- `crates/rsms-connector/src/lib.rs`（或 `connection.rs`）— 声明 `mod adapter_registry;`
- `crates/rsms-connector/src/connection.rs` — 影子块从「仅 SMGP」改为「四协议经 registry」；`encode_close_packet` 经 registry
- `crates/rsms-connector/src/client.rs` — `send_keepalive_packet` 经 registry（门控）

**创建:**
- `crates/rsms-codec-smpp/src/adapter.rs` — `SmppAdapter`
- `crates/rsms-codec-sgip/src/adapter.rs` — `SgipAdapter`
- `crates/rsms-codec-cmpp/src/adapter.rs` — `CmppAdapter`
- `crates/rsms-connector/src/adapter_registry.rs` — `adapter_for(Protocol) -> &'static dyn ProtocolAdapter`

**参照模板（执行者务必先读）:** `crates/rsms-codec-smgp/src/adapter.rs` —— 本计划三个 adapter 完全复刻它的分区结构（编码翻译表 / decode 方向 / encode 方向 / `impl ProtocolAdapter` / `#[cfg(test)]`）。

---

## Phase A — rsms-model 扩展（前置，所有 adapter 依赖）

### Task A1: ProtocolExtra 加三协议 typed 方言

**Files:**
- Modify: `crates/rsms-model/src/types.rs`

- [ ] **Step 1: 写失败测试（先红）**

在 `crates/rsms-model/src/types.rs` 的 `#[cfg(test)] mod tests` 内追加：
```rust
    #[test]
    fn protocol_extra_three_new_variants() {
        assert!(matches!(
            ProtocolExtra::Smpp(SmppExtra::default()),
            ProtocolExtra::Smpp(_)
        ));
        assert!(matches!(
            ProtocolExtra::Sgip(SgipExtra::default()),
            ProtocolExtra::Sgip(_)
        ));
        assert!(matches!(
            ProtocolExtra::Cmpp(CmppExtra::default()),
            ProtocolExtra::Cmpp(_)
        ));
    }
```

- [ ] **Step 2: 跑红**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && cargo test -p rsms-model"`
Expected: 编译失败（`SmppExtra`/`SgipExtra`/`CmppExtra` 未定义）。

- [ ] **Step 3: 扩展 ProtocolExtra 枚举**

把 `types.rs` 中现有的 `ProtocolExtra` 定义：
```rust
pub enum ProtocolExtra {
    None,
    Smgp(SmgpExtra),
}
```
替换为：
```rust
pub enum ProtocolExtra {
    None,
    Smgp(SmgpExtra),
    Smpp(SmppExtra),
    Sgip(SgipExtra),
    Cmpp(CmppExtra),
}
```

- [ ] **Step 4: 在 `SmgpExtra` 定义之后追加三个新方言结构**

```rust
/// SMPP 特有方言字段（SubmitSm/DeliverSm 中不进核心模型的部分）。
/// 注：源/目的地址的 ton/npi 进 `Address`，data_coding 进 `Encoding`，
/// TLV 进 `UnifiedSubmit::tlvs`，故此处不含这些。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SmppExtra {
    pub service_type: String,
    pub esm_class: u8,
    pub protocol_id: u8,
    pub priority_flag: u8,
    pub schedule_delivery_time: String,
    pub validity_period: String,
    /// 完整 registered_delivery（bit0 即 want_report，但保留整字节以无损往返）。
    pub registered_delivery: u8,
    pub replace_if_present_flag: u8,
    pub sm_default_msg_id: u8,
}

/// SGIP 特有方言字段（Submit 中不进核心模型的部分）。
/// 注：sp_number→src，user_numbers→dests，message_content→content，
/// msg_fmt→encoding，report_flag→want_report，reserve 默认 [0;8] 不入此处。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SgipExtra {
    pub charge_number: String,
    pub corp_id: String,
    pub service_type: String,
    pub fee_type: u8,
    pub fee_value: String,
    pub given_value: String,
    pub agent_flag: u8,
    pub morelate_to_mt_flag: u8,
    pub priority: u8,
    pub expire_time: String,
    pub schedule_time: String,
    pub tppid: u8,
    pub tpudhi: u8,
    pub message_type: u8,
}

/// CMPP 特有方言字段（V3.0 Submit 中不进核心模型的部分）。
/// 注：src_id→src，dest_terminal_ids→dests，msg_content→content，
/// msg_fmt→encoding，registered_delivery→want_report，dest_usr_tl 由 dests.len() 推导。
/// 本轮仅 V3.0：encode 恒生成 SubmitV30，故不带 version 字段（V2.0 支持时再加并附逻辑）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CmppExtra {
    /// 8 字节 msg_id（与 codec `Submit::msg_id` 同型，Default 即 [0u8;8]）。
    pub msg_id: [u8; 8],
    pub pk_total: u8,
    pub pk_number: u8,
    pub msg_level: u8,
    pub service_id: String,
    pub fee_user_type: u8,
    pub fee_terminal_id: String,
    pub fee_terminal_type: u8,
    pub tppid: u8,
    pub tpudhi: u8,
    pub msg_src: String,
    pub fee_type: String,
    pub fee_code: String,
    pub valid_time: String,
    pub at_time: String,
    pub dest_terminal_type: u8,
    pub link_id: String,
}
```

- [ ] **Step 5: 跑绿**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && cargo test -p rsms-model"`
Expected: PASS（含 `protocol_extra_three_new_variants`，原 SMGP 测试不受影响）。

- [ ] **Step 6: 确认下游未破坏（SMGP adapter 的 `_ => ` 兜底仍覆盖新臂）**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && cargo build -p rsms-codec-smgp"`
Expected: `Finished`（`unified_to_smgp` 内 `match &s.extra { ProtocolExtra::Smgp(e) => ..., _ => SmgpExtra::default() }` 的 `_` 已覆盖新增三臂，无 non-exhaustive 错误）。

- [ ] **Step 7: clippy + Commit**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && cargo clippy -p rsms-model --lib"`（零警告）
```bash
git add crates/rsms-model/src/types.rs && git commit -m "feat(model): ProtocolExtra 加 Smpp/Sgip/Cmpp typed 方言（P4-A）"
```

---

## Phase B — SmppAdapter（验 TON/NPI + TLV）

**翻译表依据（已核对 codec）:**
- `SubmitSm`/`DeliverSm` 同构。地址三字段 `source_addr_ton/npi/source_addr`、`dest_addr_ton/npi/destination_addr` → `Address{number,ton,npi}`（**TON/NPI 落进核心 Address**）。
- `data_coding: u8` → `Encoding`：`0x00→Gsm7, 0x01→Ascii, 0x02→Binary, 0x10→Ucs2, 其余→Other(v)`（本 crate `DataCoding` 枚举 UCS2=0x10）。
- `tlvs: Vec<Tlv{tag:u16,length,value:Bytes}>` → `Vec<rsms_model::Tlv{tag,value:Vec<u8>}>`（**TLV 落进核心 tlvs**）。
- 报告判别：`DeliverSm` 的 `esm_class & 0x04 != 0` → `UnifiedReport`，否则 `UnifiedDeliver`。
- `registered_delivery` 整字节进 `SmppExtra`，`want_report = registered_delivery & 0x01 != 0`。
- 无 `encode_message` free fn：编码走 `Pdu::from(struct).to_pdu_bytes(seq).to_vec()`。
- Bind 三型（Transmitter/Receiver/Transceiver）decode → `UnifiedMessage::Bind`（bind 子型/系统类型不进统一模型，encode 不支持 Bind——与 SMGP adapter 一致：encode 仅覆盖 roundtrip 判据所需 + 心跳/解绑）。
- **前置 codec 修复（执行中发现）:** 活路解码 `submit_decode::decode_submit_sm`/`decode_deliver_sm` 原本硬编码 `tlvs: vec![]`，静默丢弃所有可选 TLV（`decode_message` 走的是这两个函数，而非 `SubmitSm::Decodable`）。Phase B 先补上 TLV 解析循环（复刻 `SubmitSm::decode` 既有逻辑），否则 TON/NPI 可往返但 TLV 不可，SMGP「验 TLV」目标不达成。修复随 Phase B 一并提交并跑 SMPP 全套回归。

### Task B1: SMPP 依赖与模块声明

**Files:**
- Modify: `crates/rsms-codec-smpp/Cargo.toml`, `crates/rsms-codec-smpp/src/lib.rs`

- [ ] **Step 1: 加依赖**

`crates/rsms-codec-smpp/Cargo.toml` 的 `[dependencies]` 加一行：
```toml
rsms-model = { path = "../rsms-model" }
```

- [ ] **Step 2: 声明模块**

`crates/rsms-codec-smpp/src/lib.rs` 在现有 `pub mod` 列表末尾加：
```rust
pub mod adapter;
```

- [ ] **Step 3: 占位编译（空文件先建）**

新建 `crates/rsms-codec-smpp/src/adapter.rs`，内容暂为 `//! SmppAdapter（Task B2/B3 填充）`。
Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && cargo build -p rsms-codec-smpp"`
Expected: `Finished`。

### Task B2: SmppAdapter decode + 编码翻译表

**Files:**
- Modify: `crates/rsms-codec-smpp/src/adapter.rs`

- [ ] **Step 1: 写完整 decode 方向（覆盖占位）**

`crates/rsms-codec-smpp/src/adapter.rs`：
```rust
//! SmppAdapter：复用 decode_message + Pdu::to_pdu_bytes，做 SmppMessage ↔ UnifiedMessage 翻译。
//! 验证窄腰对 TON/NPI（落 Address）与 TLV（落 tlvs）的吸收能力。

use crate::codec::Pdu;
use crate::datatypes::{
    DeliverSm, DeliverSmResp, EnquireLink, EnquireLinkResp, SubmitSm, SubmitSmResp, Tlv, Unbind,
    UnbindResp,
};
use crate::message::{decode_message, SmppMessage};
use rsms_core::{Frame, Protocol, Result, RsmsError};
use rsms_model::{
    Address, DeliveryStatus, Encoding, MessageId, ProtocolAdapter, ProtocolExtra, SmppExtra, Tlv as UTlv,
    UnifiedBind, UnifiedDeliver, UnifiedMessage, UnifiedReport, UnifiedSubmit, UnifiedSubmitResp,
};

/// SMPP 协议适配器。
pub struct SmppAdapter;

// ── 编码翻译表（SMPP data_coding ↔ 统一 Encoding）──
fn encoding_from_dcs(dcs: u8) -> Encoding {
    match dcs {
        0x00 => Encoding::Gsm7,
        0x01 => Encoding::Ascii,
        0x02 => Encoding::Binary,
        0x10 => Encoding::Ucs2,
        other => Encoding::Other(other),
    }
}
fn dcs_from_encoding(enc: Encoding) -> u8 {
    match enc {
        Encoding::Gsm7 => 0x00,
        Encoding::Ascii => 0x01,
        Encoding::Binary => 0x02,
        Encoding::Ucs2 => 0x10,
        // SMPP 无 GBK 专属 dcs，落 0x00（有损：回译为 Gsm7）
        Encoding::Gbk => 0x00,
        Encoding::Other(v) => v,
    }
}

fn tlvs_to_unified(tlvs: &[Tlv]) -> Vec<UTlv> {
    tlvs.iter()
        .map(|t| UTlv { tag: t.tag, value: t.value.to_vec() })
        .collect()
}
fn tlvs_from_unified(tlvs: &[UTlv]) -> Vec<Tlv> {
    tlvs.iter().map(|t| Tlv::new(t.tag, t.value.clone())).collect()
}

fn submit_to_unified(s: SubmitSm) -> UnifiedSubmit {
    UnifiedSubmit {
        src: Address {
            number: s.source_addr,
            ton: Some(s.source_addr_ton),
            npi: Some(s.source_addr_npi),
        },
        dests: vec![Address {
            number: s.destination_addr,
            ton: Some(s.dest_addr_ton),
            npi: Some(s.dest_addr_npi),
        }],
        content: s.short_message,
        encoding: encoding_from_dcs(s.data_coding),
        want_report: s.registered_delivery & 0x01 != 0,
        concat: None,
        extra: ProtocolExtra::Smpp(SmppExtra {
            service_type: s.service_type,
            esm_class: s.esm_class,
            protocol_id: s.protocol_id,
            priority_flag: s.priority_flag,
            schedule_delivery_time: s.schedule_delivery_time,
            validity_period: s.validity_period,
            registered_delivery: s.registered_delivery,
            replace_if_present_flag: s.replace_if_present_flag,
            sm_default_msg_id: s.sm_default_msg_id,
        }),
        tlvs: tlvs_to_unified(&s.tlvs),
    }
}

fn deliver_to_unified(d: DeliverSm) -> UnifiedMessage {
    let dest = Address {
        number: d.destination_addr.clone(),
        ton: Some(d.dest_addr_ton),
        npi: Some(d.dest_addr_npi),
    };
    if d.esm_class & 0x04 != 0 {
        // 投递回执：receipted_message_id TLV(0x001E) 优先，否则空
        let msg_id = d
            .tlvs
            .iter()
            .find(|t| t.tag == 0x001E)
            .map(|t| MessageId::Text(String::from_utf8_lossy(&t.value).trim_end_matches('\0').to_string()))
            .unwrap_or_else(|| MessageId::Text(String::new()));
        UnifiedMessage::Report(UnifiedReport {
            msg_id,
            status: DeliveryStatus::Unknown,
            dest,
            raw: d.short_message,
        })
    } else {
        UnifiedMessage::Deliver(UnifiedDeliver {
            src: Address {
                number: d.source_addr,
                ton: Some(d.source_addr_ton),
                npi: Some(d.source_addr_npi),
            },
            dest,
            content: d.short_message,
            encoding: encoding_from_dcs(d.data_coding),
            concat: None,
            extra: ProtocolExtra::None,
            tlvs: tlvs_to_unified(&d.tlvs),
        })
    }
}

fn bind_to_unified(system_id: String, password: String, interface_version: u8) -> UnifiedBind {
    UnifiedBind {
        client_id: system_id,
        authenticator: password.into_bytes(),
        timestamp: 0,
        version: interface_version,
    }
}

fn smpp_to_unified(msg: SmppMessage) -> UnifiedMessage {
    match msg {
        SmppMessage::SubmitSm(s) => UnifiedMessage::Submit(submit_to_unified(s)),
        SmppMessage::SubmitSmResp(r) => UnifiedMessage::SubmitResp(UnifiedSubmitResp {
            msg_id: MessageId::Text(r.message_id),
            status: 0,
        }),
        SmppMessage::DeliverSm(d) => deliver_to_unified(d),
        SmppMessage::DeliverSmResp(_) => UnifiedMessage::DeliverResp,
        SmppMessage::BindTransmitter(b) => {
            UnifiedMessage::Bind(bind_to_unified(b.system_id, b.password, b.interface_version))
        }
        SmppMessage::BindReceiver(b) => {
            UnifiedMessage::Bind(bind_to_unified(b.system_id, b.password, b.interface_version))
        }
        SmppMessage::BindTransceiver(b) => {
            UnifiedMessage::Bind(bind_to_unified(b.system_id, b.password, b.interface_version))
        }
        SmppMessage::BindTransmitterResp(r) => {
            UnifiedMessage::BindResp(rsms_model::UnifiedBindResp { status: r.sc_interface_version as u32 })
        }
        SmppMessage::BindReceiverResp(r) => {
            UnifiedMessage::BindResp(rsms_model::UnifiedBindResp { status: r.sc_interface_version as u32 })
        }
        SmppMessage::BindTransceiverResp(r) => {
            UnifiedMessage::BindResp(rsms_model::UnifiedBindResp { status: r.sc_interface_version as u32 })
        }
        SmppMessage::EnquireLink(_) => UnifiedMessage::Ping,
        SmppMessage::EnquireLinkResp(_) => UnifiedMessage::PingResp,
        SmppMessage::Unbind(_) => UnifiedMessage::Unbind,
        SmppMessage::UnbindResp(_) => UnifiedMessage::UnbindResp,
        SmppMessage::Unknown { command_id, body } => UnifiedMessage::Unknown { command_id, raw: body },
        // Query/Cancel/GenericNack 等次要消息本轮退化为 Unknown，不丢帧（仅 shadow 日志可见）
        _ => UnifiedMessage::Unknown { command_id: 0, raw: vec![] },
    }
}
```

> 注：若 `BindTransmitterResp` 等无 `sc_interface_version` 字段或字段名不同，按编译器报错改为真实字段名（agent 探得为 `sc_interface_version: u8`）。

- [ ] **Step 2: 写 decode 测试（先红→实现已就绪→绿）**

`adapter.rs` 末尾：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::Pdu;
    use crate::datatypes::{SubmitSm, DeliverSm};
    use rsms_core::RawPdu;

    fn frame_of(bytes: Vec<u8>) -> Frame {
        Frame::from(RawPdu::from_vec(bytes))
    }

    #[test]
    fn decode_submit_carries_ton_npi_and_tlv() {
        let mut s = SubmitSm::new();
        s.source_addr = "1065900000".to_string();
        s.source_addr_ton = 5;
        s.source_addr_npi = 0;
        s.destination_addr = "13800138000".to_string();
        s.dest_addr_ton = 1;
        s.dest_addr_npi = 1;
        s.data_coding = 0x10; // Ucs2
        s.short_message = b"Hi".to_vec();
        s.tlvs.push(crate::datatypes::Tlv::new(0x0204, vec![0x00, 0x2A]));
        let bytes = Pdu::from(s).to_pdu_bytes(7).to_vec();

        match SmppAdapter.decode(&frame_of(bytes)).unwrap() {
            UnifiedMessage::Submit(u) => {
                assert_eq!(u.src.number, "1065900000");
                assert_eq!(u.src.ton, Some(5));
                assert_eq!(u.dests[0].ton, Some(1));
                assert_eq!(u.dests[0].npi, Some(1));
                assert!(matches!(u.encoding, Encoding::Ucs2));
                assert_eq!(u.tlvs.len(), 1);
                assert_eq!(u.tlvs[0].tag, 0x0204);
                assert_eq!(u.tlvs[0].value, vec![0x00, 0x2A]);
            }
            _ => panic!("expected Submit"),
        }
    }

    #[test]
    fn decode_deliver_receipt_is_report() {
        let mut d = DeliverSm::new();
        d.esm_class = 0x04; // delivery receipt
        d.destination_addr = "1065900000".to_string();
        let bytes = Pdu::from(d).to_pdu_bytes(8).to_vec();
        assert!(matches!(
            SmppAdapter.decode(&frame_of(bytes)).unwrap(),
            UnifiedMessage::Report(_)
        ));
    }
}
```

- [ ] **Step 3: 此刻 `impl ProtocolAdapter` 尚缺 → 编译失败**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && cargo test -p rsms-codec-smpp adapter"`
Expected: 编译失败（`SmppAdapter.decode` 未实现）。Task B3 补齐 `impl`。

### Task B3: SmppAdapter encode + 字节级 roundtrip（判据①）

**Files:**
- Modify: `crates/rsms-codec-smpp/src/adapter.rs`

- [ ] **Step 1: 加 encode 方向 + impl（接在翻译函数之后、tests 之前）**

```rust
// ── Encode 方向：UnifiedMessage → SMPP struct → Pdu → bytes ──
fn unified_to_smpp_bytes(msg: &UnifiedMessage, seq: u32) -> Result<Vec<u8>> {
    let pdu: Pdu = match msg {
        UnifiedMessage::Submit(s) => {
            let extra = match &s.extra {
                ProtocolExtra::Smpp(e) => e.clone(),
                _ => SmppExtra::default(),
            };
            let dest = s.dests.first().cloned().unwrap_or(Address::plain(""));
            let mut sm = SubmitSm::new();
            sm.service_type = extra.service_type;
            sm.source_addr_ton = s.src.ton.unwrap_or(0);
            sm.source_addr_npi = s.src.npi.unwrap_or(0);
            sm.source_addr = s.src.number.clone();
            sm.dest_addr_ton = dest.ton.unwrap_or(0);
            sm.dest_addr_npi = dest.npi.unwrap_or(0);
            sm.destination_addr = dest.number;
            sm.esm_class = extra.esm_class;
            sm.protocol_id = extra.protocol_id;
            sm.priority_flag = extra.priority_flag;
            sm.schedule_delivery_time = extra.schedule_delivery_time;
            sm.validity_period = extra.validity_period;
            sm.registered_delivery = extra.registered_delivery;
            sm.replace_if_present_flag = extra.replace_if_present_flag;
            sm.data_coding = dcs_from_encoding(s.encoding);
            sm.sm_default_msg_id = extra.sm_default_msg_id;
            sm.short_message = s.content.clone();
            sm.tlvs = tlvs_from_unified(&s.tlvs);
            Pdu::from(sm)
        }
        UnifiedMessage::SubmitResp(r) => {
            let message_id = match &r.msg_id {
                MessageId::Text(t) => t.clone(),
                MessageId::Binary(b) => String::from_utf8_lossy(b).into_owned(),
            };
            Pdu::from(SubmitSmResp { message_id })
        }
        UnifiedMessage::DeliverResp => Pdu::from(DeliverSmResp { message_id: String::new() }),
        UnifiedMessage::Ping => Pdu::from(EnquireLink),
        UnifiedMessage::PingResp => Pdu::from(EnquireLinkResp),
        UnifiedMessage::Unbind => Pdu::from(Unbind),
        UnifiedMessage::UnbindResp => Pdu::from(UnbindResp),
        other => {
            return Err(RsmsError::Other(format!(
                "SMPP encode 暂不支持该消息类型: {other:?}"
            )))
        }
    };
    Ok(pdu.to_pdu_bytes(seq).to_vec())
}

impl ProtocolAdapter for SmppAdapter {
    fn protocol(&self) -> Protocol {
        Protocol::Smpp
    }
    fn decode(&self, frame: &Frame) -> Result<UnifiedMessage> {
        let msg = decode_message(frame.data_as_slice())?;
        Ok(smpp_to_unified(msg))
    }
    fn encode(&self, msg: &UnifiedMessage, sequence_id: u32) -> Result<Vec<u8>> {
        unified_to_smpp_bytes(msg, sequence_id)
    }
}
```

> 注：未使用的 import（如 `BindReceiver`/`BindTransmitter`/`BindTransceiver` 仅在 decode 用到则保留，否则按 clippy 删）；以编译器/clippy 为准清理。

- [ ] **Step 2: 加字节级 roundtrip 测试（判据①核心）**

`adapter.rs` tests 模块加：
```rust
    #[test]
    fn submit_byte_roundtrip_via_unified() {
        let mut s = SubmitSm::new();
        s.service_type = "CMT".to_string();
        s.source_addr_ton = 5;
        s.source_addr_npi = 0;
        s.source_addr = "1065900000".to_string();
        s.dest_addr_ton = 1;
        s.dest_addr_npi = 1;
        s.destination_addr = "13800138000".to_string();
        s.esm_class = 0;
        s.registered_delivery = 1;
        s.data_coding = 0x10;
        s.short_message = b"\x4e\x2d".to_vec();
        s.tlvs.push(crate::datatypes::Tlv::new(0x0204, vec![0x00, 0x2A]));
        let original = Pdu::from(s).to_pdu_bytes(42).to_vec();

        let unified = SmppAdapter.decode(&frame_of(original.clone())).unwrap();
        let reencoded = SmppAdapter.encode(&unified, 42).unwrap();
        assert_eq!(reencoded, original, "SMPP 经统一模型往返后字节应无损一致");
    }
```

- [ ] **Step 3: 跑测试（字节不一致按字段比对修正）**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && cargo test -p rsms-codec-smpp adapter"`
Expected: PASS（3 passed）。**若字节不一致**：常见原因是 `SubmitSm::new()` 的某默认字段（如 `sm_default_msg_id`/`replace_if_present_flag`）未被 extra 覆盖——逐字段对照 `submit_to_unified`/`unified_to_smpp_bytes`，确保每个 SubmitSm 字段都「decode 进 unified、encode 写回」。TLV `length` 由 `Tlv::new` 重算，无需单独搬运。

- [ ] **Step 4: clippy + Commit**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && cargo clippy -p rsms-codec-smpp --lib"`（零警告）
```bash
git add crates/rsms-codec-smpp/ && git commit -m "feat(smpp): SmppAdapter decode/encode + TON/NPI/TLV 字节级 roundtrip（P4-B 判据①）"
```

---

## Phase C — SgipAdapter（验独立 Report 命令）

**翻译表依据（已核对 codec）:**
- `Submit{sp_number, charge_number, user_count, user_numbers, corp_id, service_type, fee_type:u8, fee_value, given_value, agent_flag, morelate_to_mt_flag, priority, expire_time, schedule_time, report_flag, tppid, tpudhi, msg_fmt, message_type, message_content, reserve:[u8;8]}`：`sp_number→src`、`user_numbers→dests`、`message_content→content`、`msg_fmt→encoding`、`report_flag→want_report`，其余进 `SgipExtra`。
- `Deliver{user_number(dest), sp_number(src), tppid, tpudhi, msg_fmt, message_content, reserve}` → `UnifiedDeliver`。
- **独立 Report 命令**：`SgipMessage::Report(Report{submit_sequence:SgipSequence, report_type, user_number, state, error_code, reserve})` → `UnifiedReport`（这是 SGIP 区别于 CMPP/SMGP 的关键，`submit_sequence` 进 `MessageId::Binary`(12B)）。
- `msg_fmt → Encoding`：`0→Ascii, 4→Ucs2, 8→Binary, 其余→Other(v)`（SGIP 规范：0=ASCII、4=UCS2、8=二进制——与 SMGP/SMPP 取值不同，**勿照抄**）。
- 无 `encode_message` free fn：编码走 `Pdu::from(struct).to_pdu_bytes(node_id, timestamp, number).to_vec()`，**adapter 约定 `node_id=0, timestamp=0, number=sequence_id`**（header SgipSequence 不进统一模型，见范围边界）。
- SGIP 无 ActiveTest 心跳：encode `Ping`/`PingResp` → `Err`（SGIP 不参与心跳收敛）。

### Task C1: SGIP 依赖与模块声明

**Files:**
- Modify: `crates/rsms-codec-sgip/Cargo.toml`, `crates/rsms-codec-sgip/src/lib.rs`

- [ ] **Step 1:** `Cargo.toml` 的 `[dependencies]` 加 `rsms-model = { path = "../rsms-model" }`。
- [ ] **Step 2:** `src/lib.rs` 加 `pub mod adapter;`。
- [ ] **Step 3:** 新建 `crates/rsms-codec-sgip/src/adapter.rs` 占位 `//! SgipAdapter（Task C2/C3 填充）`。
Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && cargo build -p rsms-codec-sgip"` → `Finished`。

### Task C2: SgipAdapter decode + 翻译表

**Files:**
- Modify: `crates/rsms-codec-sgip/src/adapter.rs`

- [ ] **Step 1: 写完整 decode 方向（覆盖占位）**

```rust
//! SgipAdapter：复用 decode_message + Pdu::to_pdu_bytes，做 SgipMessage ↔ UnifiedMessage 翻译。
//! 验证窄腰对「独立 Report 命令」的吸收能力。

use crate::codec::Pdu;
use crate::datatypes::{Deliver, Report, SgipSequence, Submit, SubmitResp, DeliverResp, Unbind, UnbindResp};
use crate::message::{decode_message, SgipMessage};
use rsms_core::{Frame, Protocol, Result, RsmsError};
use rsms_model::{
    Address, DeliveryStatus, Encoding, MessageId, ProtocolAdapter, ProtocolExtra, SgipExtra,
    UnifiedDeliver, UnifiedMessage, UnifiedReport, UnifiedSubmit, UnifiedSubmitResp,
};

/// SGIP 协议适配器。
pub struct SgipAdapter;

// ── 编码翻译表（SGIP msg_fmt ↔ 统一 Encoding）——取值不同于 SMGP/SMPP ──
fn encoding_from_fmt(fmt: u8) -> Encoding {
    match fmt {
        0 => Encoding::Ascii,
        4 => Encoding::Ucs2,
        8 => Encoding::Binary,
        other => Encoding::Other(other),
    }
}
fn fmt_from_encoding(enc: Encoding) -> u8 {
    match enc {
        Encoding::Ascii | Encoding::Gsm7 => 0,
        Encoding::Ucs2 => 4,
        Encoding::Binary => 8,
        // SGIP 无 GBK 专属 msg_fmt，落 0（有损：回译为 Ascii）
        Encoding::Gbk => 0,
        Encoding::Other(v) => v,
    }
}

/// SGIP 状态码 → 统一 DeliveryStatus（state: 0=成功投递，其余按未知保留）。
fn status_from_state(state: u8) -> DeliveryStatus {
    match state {
        0 => DeliveryStatus::Delivered,
        _ => DeliveryStatus::Unknown,
    }
}

/// 把 SgipSequence 编为 12 字节大端，装进 MessageId::Binary。
fn seq_to_msg_id(seq: SgipSequence) -> MessageId {
    let mut b = Vec::with_capacity(12);
    b.extend_from_slice(&seq.node_id.to_be_bytes());
    b.extend_from_slice(&seq.timestamp.to_be_bytes());
    b.extend_from_slice(&seq.number.to_be_bytes());
    MessageId::Binary(b)
}

fn submit_to_unified(s: Submit) -> UnifiedSubmit {
    UnifiedSubmit {
        src: Address::plain(s.sp_number),
        dests: s.user_numbers.into_iter().map(Address::plain).collect(),
        content: s.message_content,
        encoding: encoding_from_fmt(s.msg_fmt),
        want_report: s.report_flag != 0,
        concat: None,
        extra: ProtocolExtra::Sgip(SgipExtra {
            charge_number: s.charge_number,
            corp_id: s.corp_id,
            service_type: s.service_type,
            fee_type: s.fee_type,
            fee_value: s.fee_value,
            given_value: s.given_value,
            agent_flag: s.agent_flag,
            morelate_to_mt_flag: s.morelate_to_mt_flag,
            priority: s.priority,
            expire_time: s.expire_time,
            schedule_time: s.schedule_time,
            tppid: s.tppid,
            tpudhi: s.tpudhi,
            message_type: s.message_type,
        }),
        tlvs: vec![],
    }
}

fn report_to_unified(r: Report) -> UnifiedReport {
    let mut raw = Vec::new();
    raw.push(r.report_type);
    raw.push(r.state);
    raw.push(r.error_code);
    UnifiedReport {
        msg_id: seq_to_msg_id(r.submit_sequence),
        status: status_from_state(r.state),
        dest: Address::plain(r.user_number),
        raw,
    }
}

fn sgip_to_unified(msg: SgipMessage) -> UnifiedMessage {
    match msg {
        SgipMessage::Submit(s) => UnifiedMessage::Submit(submit_to_unified(s)),
        SgipMessage::SubmitResp(r) => UnifiedMessage::SubmitResp(UnifiedSubmitResp {
            msg_id: MessageId::Text(String::new()),
            status: r.result,
        }),
        SgipMessage::Deliver(d) => UnifiedMessage::Deliver(UnifiedDeliver {
            src: Address::plain(d.sp_number),
            dest: Address::plain(d.user_number),
            content: d.message_content,
            encoding: encoding_from_fmt(d.msg_fmt),
            concat: None,
            extra: ProtocolExtra::None,
            tlvs: vec![],
        }),
        SgipMessage::DeliverResp(_) => UnifiedMessage::DeliverResp,
        SgipMessage::Report(r) => UnifiedMessage::Report(report_to_unified(r)),
        SgipMessage::ReportResp(_) => UnifiedMessage::DeliverResp,
        SgipMessage::Bind(b) => UnifiedMessage::Bind(rsms_model::UnifiedBind {
            client_id: b.login_name,
            authenticator: b.login_password.into_bytes(),
            timestamp: 0,
            version: b.login_type,
        }),
        SgipMessage::BindResp(r) => {
            UnifiedMessage::BindResp(rsms_model::UnifiedBindResp { status: r.result as u32 })
        }
        SgipMessage::Unbind(_) => UnifiedMessage::Unbind,
        SgipMessage::UnbindResp(_) => UnifiedMessage::UnbindResp,
        SgipMessage::Trace(_) | SgipMessage::TraceResp(_) => {
            UnifiedMessage::Unknown { command_id: 0x1000, raw: vec![] }
        }
        SgipMessage::Unknown { command_id, body } => UnifiedMessage::Unknown { command_id, raw: body },
    }
}
```

- [ ] **Step 2: 写 decode 测试**

`adapter.rs` 末尾：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::Pdu;
    use crate::datatypes::{Report, SgipSequence, Submit};
    use rsms_core::RawPdu;

    fn frame_of(bytes: Vec<u8>) -> Frame {
        Frame::from(RawPdu::from_vec(bytes))
    }

    #[test]
    fn decode_submit_to_unified() {
        let s = Submit::new().with_message("10655000000", "13800138000", b"Test");
        let bytes = Pdu::from(s).to_pdu_bytes(0, 0, 10).to_vec();
        match SgipAdapter.decode(&frame_of(bytes)).unwrap() {
            UnifiedMessage::Submit(u) => {
                assert_eq!(u.src.number, "10655000000");
                assert_eq!(u.dests[0].number, "13800138000");
                assert_eq!(u.content, b"Test");
            }
            _ => panic!("expected Submit"),
        }
    }

    #[test]
    fn decode_standalone_report() {
        let report = Report {
            submit_sequence: SgipSequence::new(1, 0x04051200, 42),
            report_type: 0,
            user_number: "13800138000".to_string(),
            state: 0,
            error_code: 0,
            reserve: [0u8; 8],
        };
        let bytes = Pdu::from(report).to_pdu_bytes(0, 0, 7).to_vec();
        match SgipAdapter.decode(&frame_of(bytes)).unwrap() {
            UnifiedMessage::Report(u) => {
                assert_eq!(u.dest.number, "13800138000");
                assert!(matches!(u.status, DeliveryStatus::Delivered));
                // submit_sequence 编为 12 字节进 msg_id
                assert!(matches!(&u.msg_id, MessageId::Binary(b) if b.len() == 12));
            }
            _ => panic!("SGIP 独立 Report 命令应翻译为 UnifiedMessage::Report"),
        }
    }
}
```

- [ ] **Step 3: impl 尚缺 → 编译失败（Task C3 补齐）**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && cargo test -p rsms-codec-sgip adapter"`
Expected: 编译失败（`SgipAdapter.decode` 未实现）。

### Task C3: SgipAdapter encode + Submit 字节级 roundtrip（判据①）

**Files:**
- Modify: `crates/rsms-codec-sgip/src/adapter.rs`

- [ ] **Step 1: 加 encode 方向 + impl**

```rust
// ── Encode 方向（约定 node_id=0, timestamp=0, number=sequence_id）──
fn unified_to_sgip_bytes(msg: &UnifiedMessage, seq: u32) -> Result<Vec<u8>> {
    let pdu: Pdu = match msg {
        UnifiedMessage::Submit(s) => {
            let extra = match &s.extra {
                ProtocolExtra::Sgip(e) => e.clone(),
                _ => SgipExtra::default(),
            };
            let mut sub = Submit::new();
            sub.sp_number = s.src.number.clone();
            sub.charge_number = extra.charge_number;
            sub.user_count = s.dests.len() as u8;
            sub.user_numbers = s.dests.iter().map(|a| a.number.clone()).collect();
            sub.corp_id = extra.corp_id;
            sub.service_type = extra.service_type;
            sub.fee_type = extra.fee_type;
            sub.fee_value = extra.fee_value;
            sub.given_value = extra.given_value;
            sub.agent_flag = extra.agent_flag;
            sub.morelate_to_mt_flag = extra.morelate_to_mt_flag;
            sub.priority = extra.priority;
            sub.expire_time = extra.expire_time;
            sub.schedule_time = extra.schedule_time;
            sub.report_flag = if s.want_report { 1 } else { 0 };
            sub.tppid = extra.tppid;
            sub.tpudhi = extra.tpudhi;
            sub.msg_fmt = fmt_from_encoding(s.encoding);
            sub.message_type = extra.message_type;
            sub.message_content = s.content.clone();
            // reserve 保持 Submit::new() 默认 [0u8;8]
            Pdu::from(sub)
        }
        UnifiedMessage::SubmitResp(r) => Pdu::from(SubmitResp { result: r.status }),
        UnifiedMessage::DeliverResp => Pdu::from(DeliverResp { result: 0 }),
        UnifiedMessage::Unbind => Pdu::from(Unbind),
        UnifiedMessage::UnbindResp => Pdu::from(UnbindResp),
        other => {
            return Err(RsmsError::Other(format!(
                "SGIP encode 暂不支持该消息类型（含 Ping，SGIP 无心跳）: {other:?}"
            )))
        }
    };
    Ok(pdu.to_pdu_bytes(0, 0, seq).to_vec())
}

impl ProtocolAdapter for SgipAdapter {
    fn protocol(&self) -> Protocol {
        Protocol::Sgip
    }
    fn decode(&self, frame: &Frame) -> Result<UnifiedMessage> {
        let msg = decode_message(frame.data_as_slice())?;
        Ok(sgip_to_unified(msg))
    }
    fn encode(&self, msg: &UnifiedMessage, sequence_id: u32) -> Result<Vec<u8>> {
        unified_to_sgip_bytes(msg, sequence_id)
    }
}
```

- [ ] **Step 2: 加 Submit 字节级 roundtrip 测试**

```rust
    #[test]
    fn submit_byte_roundtrip_via_unified() {
        let s = Submit::new().with_message("10655000000", "13800138000", b"Test");
        let original = Pdu::from(s).to_pdu_bytes(0, 0, 42).to_vec();
        let unified = SgipAdapter.decode(&frame_of(original.clone())).unwrap();
        let reencoded = SgipAdapter.encode(&unified, 42).unwrap();
        assert_eq!(reencoded, original, "SGIP Submit 经统一模型往返后字节应无损一致");
    }
```

- [ ] **Step 3: 跑测试**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && cargo test -p rsms-codec-sgip adapter"`
Expected: PASS（3 passed）。**若字节不一致**：`Submit::new().with_message` 设置了哪些非默认字段需对照——确保 `submit_to_unified`/`unified_to_sgip_bytes` 覆盖每个字段；`user_count` 由 `dests.len()` 重算、`reserve` 用默认 `[0;8]`。

- [ ] **Step 4: clippy + Commit**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && cargo clippy -p rsms-codec-sgip --lib"`（零警告）
```bash
git add crates/rsms-codec-sgip/ && git commit -m "feat(sgip): SgipAdapter decode/encode + 独立 Report + Submit 字节级 roundtrip（P4-C 判据①）"
```

---

## Phase D — CmppAdapter（V3.0；双版本计费）

**翻译表依据（已核对 codec）:**
- `decode_message`/`encode_message` 为 free fn（同 SMGP，clean）。`CmppMessage` 变体携带 `version: CmppVersion` 与 `sequence_id: u32`。
- **本轮仅 V3.0**：`decode_message`（version=None）默认 V3.0 → `CmppMessage::SubmitV30{sequence_id, submit: Submit}`。V2.0 见范围边界。
- `Submit{msg_id:[u8;8], pk_total, pk_number, registered_delivery, msg_level, service_id, fee_user_type, fee_terminal_id, fee_terminal_type, tppid, tpudhi, msg_fmt, msg_src, fee_type, fee_code, valid_time, at_time, src_id, dest_usr_tl, dest_terminal_ids, dest_terminal_type, msg_content, link_id}`：`src_id→src`、`dest_terminal_ids→dests`、`msg_content→content`、`msg_fmt→encoding`、`registered_delivery→want_report`，`dest_usr_tl` 由 `dests.len()` 推导，其余进 `CmppExtra`（`msg_id` 为 `[u8;8]`，无 version 字段）。
- 报告判别：`Deliver.registered_delivery == 1` → `UnifiedReport`（CMPP 状态报告经 Deliver，报告标志位即 registered_delivery）。
- `msg_fmt → Encoding`：CMPP 取值 `0=Ascii, 8=Ucs2, 15=Gbk, 4=Binary`（同 SMGP 取值），其余 `Other(v)`。

### Task D1: CMPP 依赖与模块声明

**Files:**
- Modify: `crates/rsms-codec-cmpp/Cargo.toml`, `crates/rsms-codec-cmpp/src/lib.rs`

- [ ] **Step 1:** `Cargo.toml` 加 `rsms-model = { path = "../rsms-model" }`。
- [ ] **Step 2:** `src/lib.rs` 加 `pub mod adapter;`。
- [ ] **Step 3:** 新建 `crates/rsms-codec-cmpp/src/adapter.rs` 占位 `//! CmppAdapter（Task D2/D3 填充）`。
Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && cargo build -p rsms-codec-cmpp"` → `Finished`。

### Task D2: CmppAdapter decode + 翻译表

**Files:**
- Modify: `crates/rsms-codec-cmpp/src/adapter.rs`

- [ ] **Step 1: 写完整 decode 方向**

```rust
//! CmppAdapter：复用 decode_message/encode_message，做 CmppMessage ↔ UnifiedMessage 翻译。
//! 本轮仅 V3.0（无状态 decode 默认 V3.0；V2.0 见计划范围边界）。

use crate::datatypes::{CmppVersion, Connect, Deliver, Submit, SubmitResp};
use crate::message::{decode_message, encode_message, CmppMessage};
use rsms_core::{Frame, Protocol, Result, RsmsError};
use rsms_model::{
    Address, DeliveryStatus, Encoding, MessageId, ProtocolAdapter, ProtocolExtra, CmppExtra,
    UnifiedBind, UnifiedDeliver, UnifiedMessage, UnifiedReport, UnifiedSubmit, UnifiedSubmitResp,
};

/// CMPP 协议适配器（V3.0）。
pub struct CmppAdapter;

fn encoding_from_fmt(fmt: u8) -> Encoding {
    match fmt {
        0 => Encoding::Ascii,
        4 => Encoding::Binary,
        8 => Encoding::Ucs2,
        15 => Encoding::Gbk,
        other => Encoding::Other(other),
    }
}
fn fmt_from_encoding(enc: Encoding) -> u8 {
    match enc {
        Encoding::Ascii | Encoding::Gsm7 => 0,
        Encoding::Binary => 4,
        Encoding::Ucs2 => 8,
        Encoding::Gbk => 15,
        Encoding::Other(v) => v,
    }
}

fn submit_v30_to_unified(s: Submit) -> UnifiedSubmit {
    UnifiedSubmit {
        src: Address::plain(s.src_id),
        dests: s.dest_terminal_ids.into_iter().map(Address::plain).collect(),
        content: s.msg_content,
        encoding: encoding_from_fmt(s.msg_fmt),
        want_report: s.registered_delivery != 0,
        concat: None,
        extra: ProtocolExtra::Cmpp(CmppExtra {
            msg_id: s.msg_id,
            pk_total: s.pk_total,
            pk_number: s.pk_number,
            msg_level: s.msg_level,
            service_id: s.service_id,
            fee_user_type: s.fee_user_type,
            fee_terminal_id: s.fee_terminal_id,
            fee_terminal_type: s.fee_terminal_type,
            tppid: s.tppid,
            tpudhi: s.tpudhi,
            msg_src: s.msg_src,
            fee_type: s.fee_type,
            fee_code: s.fee_code,
            valid_time: s.valid_time,
            at_time: s.at_time,
            dest_terminal_type: s.dest_terminal_type,
            link_id: s.link_id,
        }),
        tlvs: vec![],
    }
}

fn deliver_v30_to_unified(d: Deliver) -> UnifiedMessage {
    if d.registered_delivery == 1 {
        UnifiedMessage::Report(UnifiedReport {
            msg_id: MessageId::Binary(d.msg_id.to_vec()),
            status: DeliveryStatus::Unknown,
            dest: Address::plain(d.dest_id),
            raw: d.msg_content,
        })
    } else {
        UnifiedMessage::Deliver(UnifiedDeliver {
            src: Address::plain(d.src_terminal_id),
            dest: Address::plain(d.dest_id),
            content: d.msg_content,
            encoding: encoding_from_fmt(d.msg_fmt),
            concat: None,
            extra: ProtocolExtra::None,
            tlvs: vec![],
        })
    }
}

fn connect_to_unified(c: Connect) -> UnifiedBind {
    UnifiedBind {
        client_id: c.source_addr,
        authenticator: c.authenticator_source.to_vec(),
        timestamp: c.timestamp,
        version: c.version,
    }
}

fn cmpp_to_unified(msg: CmppMessage) -> UnifiedMessage {
    match msg {
        CmppMessage::SubmitV30 { submit, .. } => UnifiedMessage::Submit(submit_v30_to_unified(submit)),
        CmppMessage::SubmitResp { resp, .. } => UnifiedMessage::SubmitResp(UnifiedSubmitResp {
            msg_id: MessageId::Binary(resp.msg_id.to_vec()),
            status: resp.result,
        }),
        CmppMessage::DeliverV30 { deliver, .. } => deliver_v30_to_unified(deliver),
        CmppMessage::DeliverResp { .. } => UnifiedMessage::DeliverResp,
        CmppMessage::Connect { connect, .. } => UnifiedMessage::Bind(connect_to_unified(connect)),
        CmppMessage::ConnectResp { resp, .. } => {
            UnifiedMessage::BindResp(rsms_model::UnifiedBindResp { status: resp.status })
        }
        CmppMessage::ActiveTest { .. } => UnifiedMessage::Ping,
        CmppMessage::ActiveTestResp { .. } => UnifiedMessage::PingResp,
        CmppMessage::Terminate { .. } => UnifiedMessage::Unbind,
        CmppMessage::TerminateResp { .. } => UnifiedMessage::UnbindResp,
        CmppMessage::Unknown { command_id, body, .. } => {
            UnifiedMessage::Unknown { command_id, raw: body }
        }
        // V2.0 Submit/Deliver、Query/Cancel 等本轮退化为 Unknown（仅 shadow 日志可见）
        _ => UnifiedMessage::Unknown { command_id: 0, raw: vec![] },
    }
}
```

> 注：`Submit::msg_id` 与 `SubmitResp::msg_id`/`Deliver::msg_id` 均为 `[u8;8]`，`.to_vec()` 即可；`Connect::authenticator_source` 为 `[u8;16]`。字段名以编译器为准。

- [ ] **Step 2: 写 decode 测试 + impl 缺失 → 红**

`adapter.rs` 末尾：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::Pdu;
    use crate::datatypes::{Deliver, Submit};
    use rsms_core::RawPdu;

    fn frame_of(bytes: Vec<u8>) -> Frame {
        Frame::from(RawPdu::from_vec(bytes))
    }

    #[test]
    fn decode_submit_v30_billing_fields() {
        let mut s = Submit::new();
        s.src_id = "1065900000".to_string();
        s.dest_terminal_ids = vec!["13800138000".to_string()];
        s.dest_usr_tl = 1;
        s.msg_content = b"Hello".to_vec();
        s.fee_type = "01".to_string();
        s.fee_code = "000100".to_string();
        let pdu: Pdu = s.into();
        let bytes = pdu.to_pdu_bytes(7).to_vec();
        match CmppAdapter.decode(&frame_of(bytes)).unwrap() {
            UnifiedMessage::Submit(u) => {
                assert_eq!(u.src.number, "1065900000");
                assert_eq!(u.dests[0].number, "13800138000");
                match &u.extra {
                    ProtocolExtra::Cmpp(e) => {
                        assert_eq!(e.version, 0x30);
                        assert_eq!(e.fee_type, "01");
                        assert_eq!(e.fee_code, "000100");
                    }
                    _ => panic!("extra 应为 Cmpp"),
                }
            }
            _ => panic!("expected Submit"),
        }
    }

    #[test]
    fn decode_deliver_report_via_registered_delivery() {
        let mut d = Deliver::new();
        d.registered_delivery = 1;
        d.dest_id = "1065900000".to_string();
        let pdu: Pdu = d.into();
        let bytes = pdu.to_pdu_bytes(8).to_vec();
        assert!(matches!(
            CmppAdapter.decode(&frame_of(bytes)).unwrap(),
            UnifiedMessage::Report(_)
        ));
    }
}
```

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && cargo test -p rsms-codec-cmpp adapter"`
Expected: 编译失败（`CmppAdapter.decode` 未实现）。

### Task D3: CmppAdapter encode + V3.0 Submit 字节级 roundtrip（判据①）

**Files:**
- Modify: `crates/rsms-codec-cmpp/src/adapter.rs`

- [ ] **Step 1: 加 encode 方向 + impl**

```rust
// ── Encode 方向：UnifiedMessage → CmppMessage(V3.0) → encode_message ──
fn unified_to_cmpp(msg: &UnifiedMessage, seq: u32) -> Result<CmppMessage> {
    let m = match msg {
        UnifiedMessage::Submit(s) => {
            let extra = match &s.extra {
                ProtocolExtra::Cmpp(e) => e.clone(),
                _ => CmppExtra::default(),
            };
            let mut sub = Submit::new();
            sub.msg_id = extra.msg_id;
            sub.pk_total = extra.pk_total;
            sub.pk_number = extra.pk_number;
            sub.registered_delivery = if s.want_report { 1 } else { 0 };
            sub.msg_level = extra.msg_level;
            sub.service_id = extra.service_id;
            sub.fee_user_type = extra.fee_user_type;
            sub.fee_terminal_id = extra.fee_terminal_id;
            sub.fee_terminal_type = extra.fee_terminal_type;
            sub.tppid = extra.tppid;
            sub.tpudhi = extra.tpudhi;
            sub.msg_fmt = fmt_from_encoding(s.encoding);
            sub.msg_src = extra.msg_src;
            sub.fee_type = extra.fee_type;
            sub.fee_code = extra.fee_code;
            sub.valid_time = extra.valid_time;
            sub.at_time = extra.at_time;
            sub.src_id = s.src.number.clone();
            sub.dest_usr_tl = s.dests.len() as u8;
            sub.dest_terminal_ids = s.dests.iter().map(|a| a.number.clone()).collect();
            sub.dest_terminal_type = extra.dest_terminal_type;
            sub.msg_content = s.content.clone();
            sub.link_id = extra.link_id;
            CmppMessage::SubmitV30 { sequence_id: seq, submit: sub }
        }
        UnifiedMessage::SubmitResp(r) => {
            let mut msg_id = [0u8; 8];
            if let MessageId::Binary(b) = &r.msg_id {
                let n = b.len().min(8);
                msg_id[..n].copy_from_slice(&b[..n]);
            }
            CmppMessage::SubmitResp {
                version: CmppVersion::V30,
                sequence_id: seq,
                resp: SubmitResp { msg_id, result: r.status },
            }
        }
        UnifiedMessage::Ping => CmppMessage::ActiveTest { version: CmppVersion::V30, sequence_id: seq },
        UnifiedMessage::PingResp => CmppMessage::ActiveTestResp { version: CmppVersion::V30, sequence_id: seq },
        UnifiedMessage::Unbind => CmppMessage::Terminate { version: CmppVersion::V30, sequence_id: seq },
        UnifiedMessage::UnbindResp => CmppMessage::TerminateResp { version: CmppVersion::V30, sequence_id: seq },
        other => {
            return Err(RsmsError::Other(format!(
                "CMPP encode 暂不支持该消息类型: {other:?}"
            )))
        }
    };
    Ok(m)
}

impl ProtocolAdapter for CmppAdapter {
    fn protocol(&self) -> Protocol {
        Protocol::Cmpp
    }
    fn decode(&self, frame: &Frame) -> Result<UnifiedMessage> {
        let msg = decode_message(frame.data_as_slice())?;
        Ok(cmpp_to_unified(msg))
    }
    fn encode(&self, msg: &UnifiedMessage, sequence_id: u32) -> Result<Vec<u8>> {
        let cmpp = unified_to_cmpp(msg, sequence_id)?;
        encode_message(&cmpp)
    }
}
```

> 注：`ActiveTestResp` 若有 `reserved: u8` 字段，则 `CmppMessage::ActiveTestResp` 变体本身不带该字段（探得变体仅 `{version, sequence_id}`）；以编译器为准。

- [ ] **Step 2: 加 V3.0 Submit 字节级 roundtrip 测试**

```rust
    #[test]
    fn submit_v30_byte_roundtrip_via_unified() {
        let mut s = Submit::new();
        s.src_id = "1065900000".to_string();
        s.dest_terminal_ids = vec!["13800138000".to_string()];
        s.dest_usr_tl = 1;
        s.msg_fmt = 8;
        s.msg_content = b"\x4e\x2d\x65\x87".to_vec();
        s.registered_delivery = 1;
        s.service_id = "SVC".to_string();
        s.fee_type = "01".to_string();
        s.fee_code = "000100".to_string();
        s.msg_src = "900001".to_string();
        let pdu: Pdu = s.into();
        let original = pdu.to_pdu_bytes(42).to_vec();

        let unified = CmppAdapter.decode(&frame_of(original.clone())).unwrap();
        let reencoded = CmppAdapter.encode(&unified, 42).unwrap();
        assert_eq!(reencoded, original, "CMPP V3.0 Submit 经统一模型往返后字节应无损一致");
    }
```

- [ ] **Step 3: 跑测试（字节不一致按字段比对）**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && cargo test -p rsms-codec-cmpp adapter"`
Expected: PASS（3 passed）。**若不一致**：CMPP Submit 字段最多——确保 `submit_v30_to_unified`/`unified_to_cmpp` 每个 Submit 字段都「decode 进 unified、encode 写回」；`dest_usr_tl` 由 `dests.len()` 重算；`msg_id` 空 Vec→`[0;8]`。

- [ ] **Step 4: clippy + Commit**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && cargo clippy -p rsms-codec-cmpp --lib"`（零警告）
```bash
git add crates/rsms-codec-cmpp/ && git commit -m "feat(cmpp): CmppAdapter V3.0 decode/encode + 计费字段字节级 roundtrip（P4-D 判据①）"
```

> **判据①汇总:** SMPP/SGIP/CMPP 三协议各自 `*_byte_roundtrip_via_unified` 绿 = 三张翻译表对 Submit 无损（含 TON/NPI、TLV、独立 Report、计费字段、V3.0）。

---

## Phase E — 编排层收敛（AdapterRegistry）

### Task E1: AdapterRegistry（四 adapter 集中一处）

**Files:**
- Create: `crates/rsms-connector/src/adapter_registry.rs`
- Modify: `crates/rsms-connector/src/lib.rs`（声明 `mod adapter_registry;`）

- [ ] **Step 1: 写 registry（唯一 match protocol）**

`crates/rsms-connector/src/adapter_registry.rs`：
```rust
//! 协议适配器登记表：把四协议 adapter 集中到唯一一处 `match protocol`。
//! 新增协议 = 在此加一臂 + 写它的 adapter，编排层其余位置零改动。

use rsms_codec_cmpp::adapter::CmppAdapter;
use rsms_codec_sgip::adapter::SgipAdapter;
use rsms_codec_smgp::adapter::SmgpAdapter;
use rsms_codec_smpp::adapter::SmppAdapter;
use rsms_core::Protocol;
use rsms_model::ProtocolAdapter;

/// 取协议对应的 adapter（零大小 unit struct，`'static` 提升）。
pub fn adapter_for(protocol: Protocol) -> &'static dyn ProtocolAdapter {
    match protocol {
        Protocol::Cmpp => &CmppAdapter,
        Protocol::Smgp => &SmgpAdapter,
        Protocol::Smpp => &SmppAdapter,
        Protocol::Sgip => &SgipAdapter,
    }
}
```

- [ ] **Step 2: 声明模块**

`crates/rsms-connector/src/lib.rs` 加（与其它 `mod` 同区）：
```rust
pub mod adapter_registry;
```

- [ ] **Step 3: 写 registry 测试（四协议 protocol() 自洽）**

`adapter_registry.rs` 末尾：
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_adapter_reports_its_protocol() {
        for p in [Protocol::Cmpp, Protocol::Smgp, Protocol::Smpp, Protocol::Sgip] {
            assert_eq!(adapter_for(p).protocol(), p, "adapter_for({p:?}).protocol() 应自洽");
        }
    }
}
```

- [ ] **Step 4: 编译 + 测试**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && cargo test -p rsms-connector adapter_registry"`
Expected: PASS（1 passed）。

- [ ] **Step 5: Commit**
```bash
git add crates/rsms-connector/src/adapter_registry.rs crates/rsms-connector/src/lib.rs && git commit -m "feat(connector): AdapterRegistry 收敛四协议 adapter 到一处（P4-E1）"
```

### Task E2: 统一影子比对（四协议，替换 SMGP-only）

**Files:**
- Modify: `crates/rsms-connector/src/connection.rs`

- [ ] **Step 1: 替换现有 SMGP-only 影子块（connection.rs:377-386 附近）**

把现有：
```rust
            #[cfg(feature = "unified-shadow")]
            if protocol == Protocol::Smgp {
                use rsms_model::ProtocolAdapter as _;
                match rsms_codec_smgp::adapter::SmgpAdapter.decode(&frame) {
                    Ok(unified) => tracing::debug!(conn_id = conn.id, ?unified, "shadow decode ok"),
                    Err(e) => tracing::warn!(conn_id = conn.id, "shadow decode err: {e}"),
                }
            }
```
替换为（经 registry，覆盖四协议）：
```rust
            // 影子比对：unified-shadow feature 开启时，对任意协议帧经 registry 做统一模型解码。
            // 只打日志，不接管实际处理，错误隔离不影响旧路径。
            #[cfg(feature = "unified-shadow")]
            {
                match crate::adapter_registry::adapter_for(protocol).decode(&frame) {
                    Ok(unified) => tracing::debug!(conn_id = conn.id, proto = protocol.as_str(), ?unified, "shadow decode ok"),
                    Err(e) => tracing::warn!(conn_id = conn.id, proto = protocol.as_str(), "shadow decode err: {e}"),
                }
            }
```

- [ ] **Step 2: 四协议集成测试（影子开启，行为不变）**

逐协议跑（feature 透传）：
```
wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && cargo test -p rsms-tests --test smgp-integration --features rsms-connector/unified-shadow"
wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && cargo test -p rsms-tests --test smpp-integration --features rsms-connector/unified-shadow"
wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && cargo test -p rsms-tests --test sgip-integration --features rsms-connector/unified-shadow"
wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && cargo test -p rsms-tests --test cmpp-integration --features rsms-connector/unified-shadow"
```
Expected: 四个集成测试全绿（影子开启不改变集成行为 = 判据②）。若 feature 透传语法报错，改在 `tests/Cargo.toml` 临时加 `rsms-connector/unified-shadow` 验证后回退。

- [ ] **Step 3: 影子下压测各协议（判据③，记录 TPS 对照基线）**

```
wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && cargo test -p rsms-tests --test smgp-stress-test --features rsms-connector/unified-shadow -- --nocapture"
```
Expected: `test result: ok`，TPS 与基线同量级、零丢失。（CMPP/SMPP/SGIP 压测同法抽测各一。**记录 TPS 写入提交信息或 docs**。）

- [ ] **Step 4: Commit**
```bash
git add crates/rsms-connector/src/connection.rs && git commit -m "feat(connector): 影子比对收敛为四协议经 registry（P4-E2 判据②③）"
```

### Task E3: 关闭包/心跳包编码经 adapter（字节相等门控）

**Files:**
- Modify: `crates/rsms-connector/src/connection.rs`（`encode_close_packet`）, `crates/rsms-connector/src/client.rs`（`send_keepalive_packet`）

- [ ] **Step 1: 先写「字节相等」门控测试（在 connection.rs 内 `#[cfg(test)]`）**

证明 adapter 编码的 Unbind 与旧 `encode_close_packet` 字节一致，再切换才安全。新增测试：
```rust
#[cfg(test)]
mod converge_close_tests {
    use super::*;
    use crate::adapter_registry::adapter_for;
    use rsms_model::UnifiedMessage;

    #[test]
    fn adapter_unbind_matches_legacy_close_packet() {
        for p in [Protocol::Cmpp, Protocol::Smgp, Protocol::Sgip, Protocol::Smpp] {
            let legacy = encode_close_packet(p).expect("legacy close");
            let viaadapter = adapter_for(p).encode(&UnifiedMessage::Unbind, 0).expect("adapter close");
            assert_eq!(
                viaadapter, legacy,
                "{p:?}: adapter Unbind 编码须与旧 encode_close_packet 字节一致"
            );
        }
    }
}
```

- [ ] **Step 2: 跑门控测试，按结果决定收敛范围**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && cargo test -p rsms-connector converge_close_tests"`
Expected: 列出哪些协议字节一致。
- **全绿** → Step 3 全量切换。
- **部分协议红**（常见：SMGP Exit 1B body 默认值、SMPP UNBIND 16B 头的 command_status 位）→ 逐字段在对应 adapter 的 `encode` 里对齐旧字节；若某协议无法对齐，**该协议保留旧 `encode_close_packet` 臂**并在代码注释记录原因（诚实部分收敛，不强行改字节）。

- [ ] **Step 3: 切换 `encode_close_packet` 调用点为 registry（仅对门控通过的协议）**

把 `connection.rs` 中调用 `encode_close_packet(protocol)` 的两处（idle 超时关闭、显式关闭）改为：
```rust
            // 收敛：经 adapter 统一编码关闭包（Unbind）；门控未通过的协议回退旧实现。
            let close_pdu = crate::adapter_registry::adapter_for(protocol)
                .encode(&rsms_model::UnifiedMessage::Unbind, 0)
                .ok()
                .or_else(|| encode_close_packet(protocol));
```
（若全协议门控通过，可删 `encode_close_packet`；否则保留作回退。以 Step 2 结果为准。）

- [ ] **Step 4: 心跳包同法（client.rs `send_keepalive_packet`）**

先加门控测试（SGIP 无心跳，仅测 CMPP/SMGP/SMPP）：
```rust
#[cfg(test)]
mod converge_keepalive_tests {
    use super::*;
    use crate::adapter_registry::adapter_for;
    use rsms_model::UnifiedMessage;
    use rsms_core::Protocol;

    #[test]
    fn adapter_ping_matches_legacy_keepalive() {
        let cases = [
            (Protocol::Cmpp, build_cmpp_active_test_pdu()),
            (Protocol::Smgp, build_smgp_active_test_pdu()),
            (Protocol::Smpp, build_smpp_enquire_link_pdu()),
        ];
        for (p, legacy) in cases {
            let viaadapter = adapter_for(p).encode(&UnifiedMessage::Ping, 0).expect("adapter ping");
            assert_eq!(viaadapter, legacy, "{p:?}: adapter Ping 编码须与旧 keepalive 字节一致");
        }
    }
}
```
Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && cargo test -p rsms-connector converge_keepalive_tests"`
- 门控通过的协议 → 把 `send_keepalive_packet` 的对应 match 臂改为 `adapter_for(protocol).encode(&UnifiedMessage::Ping, seq)`；SGIP 与未通过的协议保留旧 builder。
（注：旧 keepalive builder 可能用固定 sequence_id；门控测试用 `0`，若旧实现非 0 则改测试与切换都用同一 seq。以旧 builder 实际 seq 为准。）

- [ ] **Step 5: 回归 + Commit**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && cargo test -p rsms-connector --lib"`（含两组门控测试）
Expected: PASS。
```bash
git add crates/rsms-connector/ && git commit -m "feat(connector): 关闭包/心跳包编码经 adapter 收敛（字节相等门控，P4-E3）"
```

### Task E4: 全量回归

- [ ] **Step 1: 工作区构建 + clippy**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && cargo build --workspace && cargo clippy --workspace"`
Expected: 零警告（CONTRIBUTING 要求）。

- [ ] **Step 2: 全量单测 + 集成（默认 feature 关，旧路径不受影响）**

Run: `wsl bash -lc "cd /mnt/g/RustProjects/rsms && export RUSTFLAGS='--cap-lints allow' && cargo test --workspace --lib"`
然后四协议集成各跑一遍（无 feature）。
Expected: 全绿（默认 `unified-shadow` 关闭，四协议运行路径无回退）。

- [ ] **Step 3: Commit（如有收尾）**
```bash
git add -A && git commit -m "test(connector): P4 全量回归通过（默认 feature 关，零回退）"
```

> **P4 完成判据:**
> - 判据① 三协议 Submit 字节级 roundtrip 绿（含 TON/NPI、TLV、独立 Report、计费、V3.0）。
> - 判据② 四协议集成在影子开启下全绿。
> - 判据③ 影子下压测 TPS 不退化、零丢失。
> - 收敛：`AdapterRegistry` 成为唯一 `match protocol`（adapter 选取）；影子四协议统一；关闭包/心跳包经 adapter（字节相等门控通过的部分）。入站业务分发与 `create_decoder` 按范围边界保留旧路径（已记录理由）。

---

## 自检清单（执行者每个 Task 后核对）
- [ ] 每步先写测试、跑红、再实现、跑绿、提交
- [ ] cargo 命令一律走 WSL + `--cap-lints allow`
- [ ] 提交在 `feature/widen-adapters`；push 用 login shell `wsl bash -lc "... git push origin feature/widen-adapters"`
- [ ] feature `unified-shadow` 默认关闭，E2/E3 不影响四协议默认运行路径
- [ ] 字段名/方法签名与计划不符时**以编译器报错为准**修正真实符号（计划基于 Explore 已核对的 codec，个别 resp/字段名可能需对齐）
- [ ] CMPP 仅 V3.0、SGIP 用 `node_id=0/timestamp=0/number=seq` 约定——两处「有损/不进模型」均已在范围边界声明，回归中不得静默扩大
- [ ] 收敛字节相等门控未通过的协议臂，必须保留旧路径 + 注释记录（诚实部分收敛，禁止为收敛而改变上线字节）
```
