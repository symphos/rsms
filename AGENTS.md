## 语言要求
1.  你在处理所有问题时，**全程思考过程必须使用中文**（包括需求分析、逻辑拆解、方案选择、步骤推导等所有内部推理环节）。
2.  最终输出的所有回答内容（包括文字解释、代码注释、步骤说明等）**必须全部使用中文**，仅代码语法本身的英文关键词除外。

---

## Goal

重构并压测多协议（CMPP/SMGP/SMPP/SGIP）消息中间件框架。四协议的集成测试和压测已全部完成。

核心目标：
1. **框架重构**：移除框架对 SubmitResp/SubmitSmResp 的自动发送，业务方在 `BusinessHandler::on_inbound` 中自己处理
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
- **`InboundContext.id_generator`**：`Option<Arc<dyn IdGenerator>>`，业务 Handler 通过 `ctx.id_generator` 获取
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
- **SMPP 客户端 EndpointConfig 必须加 `.with_protocol("smpp")`**，否则 sequence_id 提取位置错误
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
- CMPP/SMGP/SMPP 三个 handler 对 Submit 都只返回 `Continue`（不自动回 SubmitResp），由 BusinessHandler 回
- 但 SMGP 和 SMPP 的 ActiveTest/EnquireLink 原来返回 `Stop`（会断连），需要改为 `Continue`

### 9. ~~SMPP 多账号压测 MT TPS 骤降问题~~ **已解决**
- **根因 1**：`send_request` 中 sequence_id 提取偏移量硬编码为 bytes 8-11（CMPP 格式），SMPP 应为 bytes 12-15
  - 修复：根据 `endpoint.protocol` 动态选择偏移量（"smpp"/"sgip" → 12，其他 → 8）
  - 影响：所有 SMPP/SGIP 客户端的 `send_request` 和 read loop 中的 pending queue 处理
- **根因 2**：SMPP 客户端 `EndpointConfig` 缺少 `.with_protocol("smpp")`，导致 protocol 默认为 "cmpp"
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
- ✅ `InboundContext.id_generator` 传递给业务 Handler
- ✅ 移除 `Protocol` trait 死代码（`next_msg_id`、`encode_submit_resp`、4个 Protocol struct）
- ✅ 修复 `decode_frames_drain` sequence_id 偏移量（SMPP/SGIP 用 offset 12）
- ✅ 四协议示例 server 接入 `id_generator`
- ✅ 全量测试通过（41 集成 + 11 压测）

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
crates/rsms-business/src/lib.rs                    # InboundContext + id_generator, run_chain
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
