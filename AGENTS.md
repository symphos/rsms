## 语言要求
1.  你在处理所有问题时，**全程思考过程必须使用中文**（包括需求分析、逻辑拆解、方案选择、步骤推导等所有内部推理环节）。
2.  最终输出的所有回答内容（包括文字解释、代码注释、步骤说明等）**必须全部使用中文**，仅代码语法本身的英文关键词除外。

---

## Goal

重构并压测多协议（CMPP/SMGP/SMPP/SGIP）消息中间件框架。四协议的集成测试和压测已全部完成。

核心目标：
1. **框架重构**：移除框架对 SubmitResp/SubmitSmResp 的自动发送，业务方在 `MessageHandler::on_message` 中自己处理
2. **消息队列**：使用方自己建内存消息队列，通过 `MessageSource.fetch()` 拉取，按账号隔离
3. **ID 生成器**：按账号维度的 MsgId/SequenceId 生成器，`AccountConnections` 持有 `IdGenerator` 实例
4. **客户端**：业务方自己维护 msgId → 业务信息映射，用于匹配 Report
5. **优化**：使用 DashMap 替代 RwLock+HashMap 减少锁竞争
6. **压测**：完成 CMPP/SMGP/SMPP/SGIP 四协议的单账号和多账号压测，保证消息零丢失

## Instructions

- 框架不缓存 MsgId，避免 OOM 风险
- 服务端收到 Submit → 立即返回 Resp → 异步写队列
- `MessageSource` trait：`fetch(account, batch_size) -> Vec<Vec<u8>>`，使用方自己序列化/反序列化
- **`IdGenerator` trait（rsms-core）**：`next_msg_id() -> u64` + `next_sequence_id() -> u32`，按账号隔离
- **`AccountConnections` 持有 `Arc<dyn IdGenerator>`**：`SimpleIdGenerator`（rsms-connector）为默认实现
- **`MessageContext.id_generator`**：`Option<Arc<dyn IdGenerator>>`，业务 Handler 通过 `ctx.id_generator` 获取
- 客户端匹配完 msgId 后立即移除
- `send_request` 改用 `window.offer()` 等待而非 `try_offer()` 立即报错
- 压测要有两个时间维度：压测时间 + 程序总时间
- 压测要保证消息零丢失（优雅停止、分阶段 drain）
- **压测日志级别必须设为 WARN**（INFO 会导致 240万+ 条日志拖慢 TPS 从 12500 降到 2700）
- CMPP 压测参数：5账号×5连接，每账号 TPS 2500，300秒
- SMGP 压测参数：同 CMPP
- SMPP 压测参数：同上，额外支持 V3.4 和 V5.0 两个版本
- SGIP 压测参数：同 CMPP，SGIP 有独立的 Report 命令（非 Deliver 承载）
- **压测 MT 发送使用 MessageSource.fetch() 框架机制**：mt_producer_task 按 rate push PDU 到队列 → connect() 传入 MessageSource → 框架 `run_outbound_fetcher` 自动 fetch + write_frame
- `run_outbound_fetcher` 已改为 `write_frame`（不走 window），批量 fetch 16 条
- **SMPP 客户端 EndpointConfig 必须加 `.with_protocol(Protocol::Smpp)`**，否则 sequence_id 提取位置错误
- **MessageSource push/fetch 的 key 必须和 endpoint ID 一致**（`run_outbound_fetcher` 用 `conn.authenticated_account()` 返回的是 endpoint.id）
- **长短信支持**：`MessageSource` 支持 `push_group` 推送分段组，框架保证同组帧走同一连接顺序发出
- `MessageItem::Single(Vec<u8>)` 用于普通短消息，`MessageItem::Group { items: Vec<Vec<u8>> }` 用于长短信分段
- 长短信拆分/合包由业务层使用 `rsms-longmsg` crate 处理（`LongMessageSplitter`/`LongMessageMerger`）

## Discoveries

### 1. WindowFull 错误 → 改为 offer() 等待
- `client.rs` 的 `send_request` 使用 `window.try_offer()` → 改为 `window.offer()` 循环等待

### 2. 客户端 window_size 默认只有 16
- 压测中需显式 `.with_window_size(2048)`

### 3. 压测消息丢失的两个原因
- 计数问题：`sender_task` 在 `send_request` 之前就计数 → 改为成功后才 +1
- 停止顺序：同时 abort 所有任务 → 改为分阶段优雅停止（5 阶段 drain）

### 4. 优雅停止 5 阶段流程
1. 停止 sender tasks
2. 等待 SubmitResp 回齐（resp >= sent，超时 10-15s）
3. 等待 msg_source 队列排空 + Report 全部发出
4. 停止 Report/MO 生成器
5. 等待 Report/MO 全部收齐

### 5. 多账号 SubmitResp = 0 的根因（CMPP）
- `multi_account_stress_test.rs` 的 `build_submit_pdu` 缺少 `submit.dest_usr_tl = 1`
- **已修复**

### 6. 服务端 `decode_frames` 不处理跨 read 不完整 PDU
- 改为 `Vec<u8>` 累积缓冲区 + `decode_frames_drain` drain 消费模式

### 7. **日志级别是长时压测的致命瓶颈**
- 300 秒 CMPP 压测，INFO 日志导致 TPS 从 12500 骤降到 2700（仅 21%）
- 改为 WARN 后恢复到 12500+ TPS
- **所有压测必须用 WARN 级别**

### 8. Handler 设计一致性
- CMPP/SMGP/SMPP 三个 handler 对 Submit 都只返回 `Continue`（不自动回 SubmitResp），由 MessageHandler 回
- 但 SMGP 和 SMPP 的 ActiveTest/EnquireLink 原来返回 `Stop`（会断连），需要改为 `Continue`

### 9. ~~SMPP 多账号压测 MT TPS 骤降问题~~ **已解决**
- **根因 1**：`send_request` 中 sequence_id 提取偏移量硬编码为 bytes 8-11（CMPP 格式），SMPP 应为 bytes 12-15
  - 修复：根据 `endpoint.protocol.seq_offset()` 动态选择偏移量（`Protocol::Smpp`/`Protocol::Sgip` → 12，其他 → 8）
  - 影响：所有 SMPP/SGIP 客户端的 `send_request` 和 read loop 中的 pending queue 处理
- **根因 2**：SMPP 客户端 `EndpointConfig` 缺少 `.with_protocol(Protocol::Smpp)`，导致 protocol 默认为 `Protocol::Cmpp`
  - 即使 `send_request` 修复了偏移量，如果不设 protocol，仍然会用错误的偏移量
- **根因 3**：`send_request` 的 window 机制在高并发+高 DeliverSm 负载下成为瓶颈
  - read loop 处理大量 DeliverSm（Report+MO）时，SubmitSmResp 的 window.complete 被延迟
  - window 填满后 offer() 阻塞，TPS 骤降
  - **解决方案**：压测 sender_task 改用 `write_frame`，不走 window（和 CMPP 单账号压测一致）
  - CMPP 多账号压测用 `send_request` 没问题，因为 Deliver 的处理开销比 SMPP DeliverSm 低

### 10. SMPP 关键协议差异
- 16 字节头（多了 command_status 4 字节），CMPP/SMGP 是 12 字节
- Message ID 是 C-string（String 类型），CMPP 是 `[u8; 8]`，SMGP 是 `SmgpMsgId`(10字节)
- 认证是明文 BindTransmitter，不需要 MD5 计算
- Report 通过 `DeliverSm(esm_class=0x04)` 承载
- 版本差异仅在字段长度限制（V3.4 vs V5.0），不涉及不同 PDU struct

### 11. `AccountConfig::new()` 默认 `max_qps = 100`
- MockAccountConfigProvider 已设为 10000，但 pool 创建时初始值是 100
- update_config 在连接注册到 pool 后才调用

### 12. `Protocol` trait 死代码清理
- `Protocol` trait 的 `next_msg_id()`、`encode_submit_resp()` 及关联类型（MsgId/Submit/SubmitResp/Deliver）从未被调用
- `CmppProtocol`/`SmgpProtocol`/`SmppProtocol`/`SgipProtocol` 从未被实例化
- 各 handler 的全局静态计数器（`static NEXT_MSG_ID: AtomicU64`）也是死代码
- **已全部移除**，ID 生成职责迁移到 `IdGenerator` trait

### 13. 服务端 `decode_frames_drain` sequence_id 偏移量 bug
- 原来 `decode_frames_drain` 固定用 bytes 8-11 提取 sequence_id（只对 CMPP/SMGP 正确）
- SMPP（16字节头）和 SGIP（20字节头）的 sequence_id 在 bytes 12-15
- **已修复**：`decode_frames_drain` 新增 `protocol` 参数，SMPP/SGIP 用 offset 12

### 14. CMPP 压测 harness 既有 bug（版本感知/spec 回执落地后未同步），非回归
- **现象**：CMPP 单+多账号压测稳定失败（cmpp20 "Connection 0 failed"；cmpp30 `Report=0` → `report_matched >= submit_resp-100` 断言失败）。SMGP/SMPP/SGIP 全绿。
- **定性**：在 `22c7dee`（任何近期重构之前）复跑**同样失败** → 既有问题，**不是** R1–R5/版本感知重构引入的回归。框架本身正常（三协议全绿即证）。
- **根因（均在 `tests/cmpp/*stress_test.rs`）**：
  1. client 用版本无关 `CmppAdapter.decode`（默认 V3.0）解服务端按 `ed894ca` 回的 **V2.0 宽度 ConnectResp(18B)** 失败 → 永不置 `connected`。
  2. 回执经 adapter 编为定长二进制后（PR #12 spec-71B），原 `raw` 文本被丢弃，client 仍用 `parse_msg_id_from_report` 从**文本**抽 msgId → 永不匹配。且服务端把 `UnifiedReport.msg_id` 置 0、真 msgId 仅放文本。
- **已修复（仅测试代码，分支 `fix/cmpp-stress-harness`）**：client 改 `decode_with_version(frame, 本连接版本)`；服务端 `UnifiedReport.msg_id` 填真实 8B msgId；client 按解码后 `r.msg_id` 结构化匹配。单/多账号两份 harness 同改。
- **验证（60s/WARN）**：cmpp-stress 3 passed（CMPP3.0 Report 2500）、cmpp-multi 1 passed（Report 12713）；四协议单+多账号全绿、零丢失、MT 2500/12700 TPS。

### 15. msg_id / sequence / 长短信 隔离性审计与加固
- **审计结论**：msg_id（服务端出站，`IdGenerator`）按账号独立实例、账号间物理隔离；sequence_id 由业务构造 PDU 时填、框架不代生成；滑动窗口与 pending_responses **按连接隔离**（同步响应匹配不串）；`TransactionManager` **按账号共享**、以 sequence_id 为键（回执走 msg_id 键、连接无关）。
- **🔴 真缺陷已修**：`LongMessageMerger` 此前仅按 `(reference,total)` 分组（`frame.unique_id()`、不含发送方），不同发送方 reference 撞号（16-bit、各自从小值起）会串合/丢段。已改 `add_frame(sender, frame)`、键带发送方；四协议示例服务端传 phone、客户端传 src；新增 `different_senders_same_reference_do_not_cross_merge` 回归（去掉 sender 维度即 FAILED）。
- **🟢 出站 reference 加固**：`ReferenceIdGenerator::new()` 种子改为进程级分发器（一次性纳秒时间基 + 黄金比例素数步长），保证「每条长短信新建 splitter」时多个生成器**起始 reference 互不相同**（`fresh_generators_have_distinct_starting_references`）。理想仍是每账号持久生成器（`with_generator`）。
- **🟢 sequence 契约文档化**：同账号多连接的 sequence_id **须账号内唯一**（否则共享 TM 按 seq 键会互相覆盖）；框架已提供正解——同账号多连接共用一个 `IdGenerator`，用 `account_connections.id_generator().next_sequence_id()` 取值即可。已在 `client.rs::send_request` 与 `TransactionManager::add_submit_transaction` 注明。**不**采用「TM 键带 conn_id」（会破坏从其他连接回来的回执匹配）。

## Accomplished

### CMPP（✅ 全部完成）
- ✅ 框架重构（MessageSource, window.offer, DashMap）
- ✅ 单账号 1连接 + 5连接压测（30s，零丢失）
- ✅ 多账号 5×5连接压测（300s，零丢失，MT TPS 12,553）

### SMGP（✅ 全部完成）
- ✅ 框架修复：Submit 改 debug 日志，ActiveTest 改 Continue
- ✅ 现有 9 个集成测试通过
- ✅ 单账号 1连接 + 5连接压测（30s，零丢失）
- ✅ 多账号 5×5连接压测（300s，零丢失，MT TPS 12,553）

### SMPP（✅ 全部完成）
- ✅ 框架修复：EnquireLink 改 Continue，SubmitSm/DeliverSm 改 debug 日志
- ✅ 现有 9 个集成测试通过
- ✅ stress_test.rs（V3.4 × 1+5连接，V5.0 × 1+5连接）全部通过，零丢失
- ✅ **multi_account_stress_test.rs（5×5，300s）已通过，零丢失**
- ✅ 修复 `send_request` 中 SMPP/SGIP sequence_id 提取偏移量错误

### SGIP（✅ 全部完成）
- ✅ 现有 8 个集成测试通过
- ✅ stress_test.rs（1连接 + 5连接）全部通过，零丢失
- ✅ **multi_account_stress_test.rs（5×5，300s）已通过，零丢失，MT TPS 12,563**

### IdGenerator 重构（✅ 已完成）
- ✅ `IdGenerator` trait 定义在 `rsms-core`，`SimpleIdGenerator` 实现在 `rsms-connector`
- ✅ `AccountConnections` 按账号持有独立 `IdGenerator` 实例
- ✅ `MessageContext.id_generator` 传递给业务 Handler
- ✅ 移除 `Protocol` trait 死代码（`next_msg_id`、`encode_submit_resp`、4个 Protocol struct）
- ✅ 修复 `decode_frames_drain` sequence_id 偏移量（SMPP/SGIP 用 offset 12）
- ✅ 四协议示例 server 接入 `id_generator`
- ✅ 全量测试通过（41 集成 + 11 压测）

## 重构方向 / 技术债（待办，非缺陷）

> 来源：PR #16（CMPP 2.0 版本感知 + 服务端事件回调）code-review。以下均为「真实存在但与该 PR 主题正交」或「架构方向」，当前代码功能正确，未在该 PR 内改动，留作后续独立 PR。

### R1. CMPP 回执编码去重（codec 风险区，需测试先行）✅ 已完成（702cbb8）
> `to_bytes`/`to_bytes_v20` 已抽为参数化 `to_fixed_bytes(dest_width)`；先补 71B 字节级表征测试锁布局再重构，逐字节等价、全测试绿。
> adapter 的 **Report 臂** V20/V30 已去重：重复的 `CmppReport` 合成字面量抽为 `report_body(is_v20)` 闭包，两分支已有 60B/71B roundtrip 测试护航（在 `refactor/cmpp-adapter-report-arms` 分支）。Submit/Deliver(MO) 两臂因 `SubmitV20`/`Submit`、`DeliverV20`/`Deliver` 字段集本就不同，强行去重低价值，按设计保留。

- `datatypes/deliver.rs` 的 `to_bytes` / `to_bytes_v20` 重复同一 `fixed()` 闭包，函数体仅 `Dest_terminal_Id` 宽度不同（V3.0=32 / V2.0=21），其余 `Msg_Id/Stat/时间/SMSC_sequence` 一致。
- `adapter.rs` 的 Submit/Deliver/Report 各有 V20/V30 近重复臂，可抽 builder。
- **不在 PR #16 内做的原因**：定长二进制回执（60B/71B）字节布局直接影响真机（cmos）解析，是该 PR 刚联调修好的高风险区；合并前应先补「V2.0/V3.0 回执逐字节对拍」单测，再做参数化合并，保证逐字节等价。

### R2. 版本表示统一 ✅ 已收口（重新界定范围）
> **复核结论：原计划「让 `CmppVersion` 贯穿全链路、改 `ProtocolConnection` trait 签名」不应做。** `protocol_version()/set_protocol_version(u8)` 是四协议**共用**的泛型 API（**SMPP 也用它存 interface_version 0x34/0x50**，见 `handlers/smpp.rs`），定义在 `rsms-business`，**不能反向依赖 CMPP 专属的 `CmppVersion`**（否则破坏分层、拖累 SMPP/SMGP）。那个 `u8` 是正确的「跨协议线路版本字节」窄腰，保留。
> **真正残余已收口**：`handlers/cmpp.rs` 的 `is_cmpp_v2` / `is_version_supported` 改为复用 `CmppVersion::from_wire`（版本字节集 0x20/0x00/0x01/0x30 的**唯一来源**），删除平行的 `matches!` 与冗余常量 `CMPP_VERSION_2_0`。CMPP codec 内部本就已用 `CmppVersion` enum，无散落裸字节。（PR #19）

### R3. `encode_message` V2.0 手写路径下沉 ✅ 已完成（PR #18）
> **复核发现**：`SubmitRespV20`/`DeliverRespV20`/`ConnectRespV20` 类型早已存在（有 `BODY_SIZE` + `Decodable` + `From`→收敛到共享 `Pdu::*Resp`），**唯独缺 `Encodable`**——这正是 `encode_message` 不得不手写 V2.0 应答字节的根因，是 `Decodable`/`Encodable` 的真实不对称。
> **已做**：补三者的 `Encodable`（与 decode 对称，字节宽度知识入数据类型层）；`encode_message` 的手写早 return + 本地 `write_header` 改为泛型 `encode_v20_pdu` 委托。先补 DeliverResp V20 表征测试（Submit/Connect 已有），重构后三条 V2.0 应答字节级测试逐字节不变、codec-cmpp 72 passed、cmpp-integration 20 passed、clippy 零警告。
> **未做（按设计）**：不新增 `Pdu::*RespV20` 变体——应答类型按既有设计 decode 后收敛到共享 `Pdu::*Resp`，加 encode-only 变体会破坏该对称，故 encode 直接走类型的 `Encodable`。

### R4. 预定 MO 版本感知能力的归属
- `examples/cmpp_server` 的 `FileMessageSource` 用 `raw_mo`/`mo_enqueued` 在**应用层**实现「按连接版本延迟编码」。作为演示代码这是其职责所在。
- 若多应用都需要，可考虑框架层抽象 `VersionAwareMessageSource` 包装或 adapter 提供 `encode_auto_version`——属产品决策，不在 example 内定。
- 关联：PR #16（fae4322）已把去重键由 `account` 改为 `account#version`，修掉换版本重连发错形态的问题。
- **语义决定（推荐保持）**：预定 MO 采用「种子语义」——同账号同版本仅入队下发一次。推荐保持该语义：预定 MO 本质是「开机种子消息」，按种子语义可避免每次断线重连重复下发同一批。换版本重连的正确性已由 fae4322 覆盖。若后续确有「每连接补发」需求，再单独把去重改为连接级即可。

### R5. `ServerEventHandler` 文档注释欠账（既有代码，非本 PR 引入）✅ 已完成（b56b1c5）
- `crates/rsms-connector/src/protocol.rs` 的 `on_connected` / `on_disconnected` / `on_authenticated` 缺 `///` 文档注释。该 trait 是既有代码，PR #16 仅首次调用它们、未改 `protocol.rs`，故未在该 PR 内补；可单开 docs 改动补齐（CLAUDE.md 要求公开 API 必须有文档注释）。

## Relevant files / directories

### 框架核心
```
crates/rsms-core/src/id_generator.rs              # IdGenerator trait 定义
crates/rsms-connector/src/id_generator.rs          # SimpleIdGenerator 实现
crates/rsms-connector/src/protocol.rs              # MessageSource, AccountConfig, ProtocolHandler
crates/rsms-connector/src/client.rs                # send_request, window.offer, sequence_id 偏移量
crates/rsms-connector/src/server.rs                # inbound_fetcher_task, serve()
crates/rsms-connector/src/connection.rs            # decode_frames_drain, run_connection, write_frame
crates/rsms-connector/src/handlers/cmpp.rs         # CMPP handler
crates/rsms-connector/src/handlers/smgp.rs         # SMGP handler
crates/rsms-connector/src/handlers/smpp.rs         # SMPP handler
crates/rsms-connector/src/handlers/sgip.rs         # SGIP handler
crates/rsms-connector/src/pool.rs                  # AccountConnections + id_generator
crates/rsms-business/src/lib.rs                    # MessageContext + id_generator, MessageHandler
crates/rsms-core/src/endpoint.rs                   # EndpointConfig + window_size + protocol
crates/rsms-window/src/window.rs                   # offer vs try_offer
```

### SMPP 编解码
```
crates/rsms-codec-smpp/src/codec.rs                # PduHeader(16字节), Pdu, to_pdu_bytes
crates/rsms-codec-smpp/src/message.rs              # SmppMessage, decode_message
crates/rsms-codec-smpp/src/datatypes/submit_sm.rs  # SubmitSm, SubmitSmResp(message_id: String)
crates/rsms-codec-smpp/src/datatypes/deliver_sm.rs # DeliverSm, DeliverSmResp
crates/rsms-codec-smpp/src/datatypes/bind_transmitter.rs # BindTransmitter
crates/rsms-codec-smpp/src/datatypes/command_id.rs # CommandId enum
crates/rsms-codec-smpp/src/version.rs              # SmppVersion (V34/V50)
```

### 测试文件
```
tests/cmpp/                                        # CMPP 集成+压测测试
tests/smgp/                                        # SMGP 集成+压测测试
tests/smpp/                                        # SMPP 集成+压测测试
tests/sgip/                                        # SGIP 集成+压测测试
tests/common/                                      # 测试公共库
```

### 示例服务端/客户端
```
examples/cmpp_server/    examples/cmpp_client/
examples/smgp_server/    examples/smgp_client/
examples/smpp_server/    examples/smpp_client/
examples/sgip_server/    examples/sgip_client/
```
