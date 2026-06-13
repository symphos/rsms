# 统一消息模型 · SMGP 窄腰试点设计

> 状态：设计待评审
> 日期：2026-06-13
> 范围：P0–P3（SMGP 试点到可验证）+ 四协议全量推广蓝图（概述）
> 前置讨论：窄腰沙漏模型、三类参数分治、值映射下沉 codec、渐进路径 P0–P4、试点选 SMGP（见对话记录与 `docs/OPTIMIZATION_PLAN.md` 阶段 4 收束）

## 1. 背景与目标

RSMS 当前是「**宽腰 + 穷尽 match**」形态：编排层（`connection.rs`/`client.rs`/`server.rs`）用 `match protocol` 在多处分发到 per-protocol handler / decoder / keepalive / close。加一个新协议要同步改所有 match 点。这个形态**作为能上受控生产的状态没问题**（阶段 0–4 已验证），但不利于「协议持续增加 + 团队扩大 + 长期演进」。

**目标**：引入一条「窄腰」——一个**协议无关的统一消息模型 `UnifiedMessage`** + **`ProtocolAdapter` trait**，让各协议 codec 把自己 ↔ 统一模型双向翻译，编排/业务层只对统一模型编程。终态：新增协议 = 加一个 adapter，编排/业务层零改动。

**本设计不追求一步到位**，而是用 **SMGP 单协议试点**，以最小代价、全程可回退地**验证「这条腰能不能立住」**，再决定是否全量推广。

## 2. 范围

**做（P0–P3，仅涉及 SMGP）**：
- P0：新增 `rsms-model` crate（`UnifiedMessage` + 语义枚举 + `ProtocolAdapter` trait）。
- P1：在 `rsms-codec-smgp` 实现 `SmgpAdapter`（复用现有 codec 中转）+ 翻译表 + roundtrip 测试。
- P2：在 `rsms-connector` 加影子比对（feature flag，新路径只解码比对、不接管）。
- P3：加统一 `BusinessHandler::on_message(UnifiedMessage)` 与旧接口并存 + SMGP example 验证业务侧简化。

**不做（明确排除）**：
- 不改 codec 字节解析层（adapter 复用，不重写）。
- 不动 `TransactionManager`（报告匹配仍 per-protocol；统一后可协议无关，属后续收益）。
- 不动其他三协议（CMPP/SMPP/SGIP）的运行路径——它们仅作为统一模型设计时的「全局对照」，不实现 adapter。
- 计费、TON/NPI 等强方言**主动不进核心模型**（放协议扩展）。
- 不做「codec 字节直出 UnifiedMessage」的性能优化（等腰确认立住后再议）。

## 3. 架构与 crate 结构

```
            rsms-connector (P2 影子比对 / P3 统一业务接口)
                 │ 依赖
   rsms-codec-smgp ──impl SmgpAdapter──┐
   (现有 SmgpMessage + decode_message) │ 依赖
                 │ 依赖              ┌─▼─────────────┐
                 └──────────────────► rsms-model     │ ★窄腰层
                                    │ UnifiedMessage  │
                                    │ ProtocolAdapter │
                                    └─┬───────────────┘
                                      │ 依赖
                                   rsms-core (Frame/Protocol/RsmsError)
```

**结构决策**：
1. **`rsms-model` 独立成 crate**（不并入 rsms-core）：窄腰是会被频繁演进、被多方依赖的层，独立 crate 让编译边界与版本清晰，避免 core 膨胀。依赖 `rsms-core`（复用 `Frame`/`Protocol`/`RsmsError`/`Result`）。
2. **`SmgpAdapter` 实现放 `rsms-codec-smgp`**（新增 `adapter` 模块）：与它复用的 `decode_message`/`SmgpMessage` 同 crate，「字节解析」与「方言翻译」内聚。codec crate 新增对 `rsms-model` 的依赖（无环：codec→model→core）。
3. **试点期只动 2 个 crate**：`rsms-model`（全新）、`rsms-codec-smgp`（加 adapter 模块）。其余 10 个 crate 在 P2 之前零改动。

## 4. 统一消息模型（三类参数分治）

### 4.1 消息枚举（主干）
```rust
pub enum UnifiedMessage {
    Submit(UnifiedSubmit),   SubmitResp(UnifiedSubmitResp),
    Deliver(UnifiedDeliver), DeliverResp(UnifiedDeliverResp),
    Report(UnifiedReport),
    Bind(UnifiedBind),       BindResp(UnifiedBindResp),
    Unbind,                  UnbindResp,
    Ping,                    PingResp,
    Unknown { command_id: u32, raw: Vec<u8> },
}
```
Query/Cancel 等次要消息留待后续；`Unknown` 兜底未识别命令，保证 adapter 永不丢帧。

### 4.2 三类参数（以 `UnifiedSubmit` 示范）
```rust
pub struct UnifiedSubmit {
    // 第一类：核心传输语义（协议无关，编排层只读这些）
    pub src: Address,
    pub dests: Vec<Address>,
    pub content: Vec<u8>,
    pub encoding: Encoding,        // 语义枚举，非协议魔数
    pub want_report: bool,
    pub concat: Option<Concat>,    // 长短信分片 ref/total/seq
    // 第二类：typed 协议方言
    pub extra: ProtocolExtra,
    // 第三类：可选 TLV
    pub tlvs: Vec<Tlv>,
}
```

### 4.3 语义类型
```rust
pub enum Encoding { Gsm7, Ascii, Ucs2, Gbk, Binary, Other(u8) }
pub enum DeliveryStatus { Delivered, Expired, Undeliverable, Accepted, Rejected, Unknown, Other(String) }
pub struct Address { pub number: String, pub ton: Option<u8>, pub npi: Option<u8> } // ton/npi 是核心概念「地址」的可选方言修饰
pub struct Concat { pub reference: u16, pub total: u8, pub sequence: u8 }
pub enum MessageId { Binary(Vec<u8>), Text(String) } // 吸收 [u8;8]/10B/String/SgipSequence
pub struct Tlv { pub tag: u16, pub value: Vec<u8> }
pub enum ProtocolExtra { None, Smgp(SmgpExtra) /* 后续 Cmpp/Smpp/Sgip */ }
pub struct SmgpExtra { pub fee_type: String, pub fee_code: String, pub fixed_fee: String, pub msg_type: u8, pub priority: u8 /* …SMGP 特有 */ }
```

### 4.4 字段「画线」原则
- **核心字段**：所有协议都有等价物、且编排层需要。宁可少放（让扩展承接），不做几十个 `Option` 的胖结构。
- **值映射全部下沉 codec**：`Encoding`/`DeliveryStatus` 等用语义枚举，协议魔数（`msg_format`/状态码）由 adapter 翻译，**绝不上浮到编排/业务层**。
- **主动不统一**：计费（`fee_*`）、TON/NPI 进 `extra`/`Address` 可选位；valid_time/at_time 暂留 `extra`。

## 5. ProtocolAdapter 与 SmgpAdapter

### 5.1 trait（定义于 rsms-model）
```rust
pub trait ProtocolAdapter: Send + Sync {
    fn protocol(&self) -> Protocol;
    fn decode(&self, frame: &Frame) -> Result<UnifiedMessage>;
    fn encode(&self, msg: &UnifiedMessage, sequence_id: u32) -> Result<Vec<u8>>;
}
```

### 5.2 SmgpAdapter（rsms-codec-smgp，复用 codec 中转）
- `decode`：`frame.data → decode_message → SmgpMessage → 翻译 → UnifiedMessage`。
- `encode`：`UnifiedMessage → SmgpMessage → 现有 to_pdu_bytes`。
- **三张核心翻译表**：
  - `Encoding ↔ SMGP msg_format`（如 Gsm7↔0、Ucs2↔8、Gbk↔15，以 SMGP 规范为准）
  - `DeliveryStatus ↔ SMGP 报告状态码`
  - **报告判别**：SMGP 无独立 Report command，报告走 `Deliver` 的报告标志位 → adapter 据此把 `Deliver` 翻译成 `UnifiedReport`（是报告）或 `UnifiedDeliver`（普通 MO）。
- 翻译失败（未知 command/状态/编码）→ 退化为 `Unknown`/`Other(_)`，不丢帧、不 panic。

## 6. 集成方式（新旧并存，可回退）

- **P2 影子比对**：connector 的 SMGP 入站路径，Frame 同时走 ①现有原生 handler（实际处理）②`SmgpAdapter::decode`（只解码、与①结果比对打日志）。`feature = "unified-shadow"` 或运行时开关控制。**关 flag 即纯走旧路径，生产无感。**
- **P3 统一业务接口**：新增 `BusinessHandler::on_message(&self, ctx, &UnifiedMessage)`，与旧 `on_inbound(&self, ctx, &Frame)` **并存**（默认方法转发或二选一注册）。SMGP example 用统一接口重写，对照旧版验证简化。

## 7. 验证策略与决策判据（P4 评审）

| 判据 | 手段 | 含义 |
|------|------|------|
| ① roundtrip 字节无损 | 单测：SMGP PDU → UnifiedMessage → SMGP PDU 一致 | 翻译表画得对 |
| ② 影子零差异 | 现有 smgp-integration + 压测下，新旧路径解码结果一致 | 生产语义下翻译正确 |
| ③ TPS 不退化 | smgp 压测对照基线（~12542） | 中转开销可接受 |
| ④ 业务侧简化 | 统一 example vs 旧 example 对比 | 腰对上层有价值 |

**四项全绿** → 推广 SMPP（验证 TON/NPI+TLV 上界）→ SGIP（独立 Report）→ CMPP（双版本+计费），最后收敛编排层 `match protocol`。**任一不过** → 止损，统一路径退化为 SMGP 可选 API，不全量。

## 8. 错误处理

- adapter 的 `decode`/`encode` 返回 `rsms_core::Result`；翻译不认识的值退化为 `Other(_)`/`Unknown` 而非报错（不丢帧）。
- 编码超限等沿用 codec 既有的 `CodecError`（阶段 4A.6 已加长度校验）。
- 影子模式下，新路径任何错误只记录日志，**绝不影响旧路径的实际处理**。

## 9. 风险与缓解

| 风险 | 缓解 |
|------|------|
| **翻译开销**（多一层 SmgpMessage→Unified 转换） | 试点期接受，压测验证（判据③）；腰立住后再考虑 codec 直出 |
| **过早抽象**（只 fit SMGP，推广时要改模型） | 设计时摊开四协议字段全局对照（已做，见第 4 节），核心字段照顾全部四协议 |
| **统一模型频繁变动** | 试点期 model 仅覆盖主干消息；判据未过即止损，不扩散 |
| **影子比对自身引入开销/干扰** | feature flag 控制，默认关闭；新路径错误隔离不影响旧路径 |

## 10. 阶段拆分

- **P0** 骨架：`rsms-model` crate（消息模型 + 语义枚举 + trait）+ 基本单测。纯新增，零风险。
- **P1** SMGP adapter：`SmgpAdapter` + 三张翻译表 + roundtrip 单测（判据①）。
- **P2** 影子比对：connector 接入（feature flag）+ smgp-integration/压测验证（判据②③）。
- **P3** 统一业务接口：`on_message` 并存 + SMGP example（判据④）。
- **P4** 评审决策点：四判据评估 → 推广 / 止损。（推广为后续独立设计）

---

附：本设计是 `docs/OPTIMIZATION_PLAN.md` 阶段 4 收束时所提「窄腰演进」的具体落地方案，定位为**前瞻性架构投资**而非救火，故采用「单协议试点先验证、全程可回退」的低风险推进方式。
