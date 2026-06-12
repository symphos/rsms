# RSMS 整体优化计划

> 分支：`refactor/overall-optimization`
> 体检方式：`cargo clippy --workspace --all-targets` + 静态扫描 + 三路并行专项分析（性能 / 架构 / 健壮性）。
> 标记说明：✅ = 已逐行核对源码确认；⚠️ = 专项分析报告，实施前需复核。
> 语言约定：按 `AGENTS.md`，思考与输出均用中文。

## 总体原则

- **分阶段、每阶段独立提交**，提交信息遵循 `feat`/`fix`/`refactor`/`perf` 前缀。
- **行为不变的前提下重构**：P2/P3 不得改变对外语义；改动后 `cargo build --workspace` + `cargo test --workspace --lib` + 四协议集成测试必须全绿。
- **P0/P1 改完必须补/跑回归**：P0 加针对性单测，P1 用压测验证 TPS 不退化且消息零丢失。

---

## 阶段 0（P0）：正确性 / 安全缺陷 —— 最高优先级

### 0.1 ✅ SMPP 投递报告识别位掩码 bug

- **位置**：`crates/rsms-connector/src/transaction/smpp.rs:84`
- **现状**：`(self.inner.esm_class & 0x03) == 4` —— `& 0x03` 只保留 bit0-1（Messaging Mode），结果恒在 0..=3，永远 ≠ 4 ⇒ `is_report()` 恒为 `false`。clippy 以 correctness lint（deny 级）报 **error**，导致 `cargo clippy` 编译失败。
- **影响**：SMPP `TransactionManager` 永远识别不出 DeliverSm 承载的状态报告，`on_report` 回调不触发。
- **改动**：按 SMPP 规范，SMSC 投递回执位是 `esm_class` bit2（`0x04`）。改为：
  ```rust
  pub fn is_report(&self) -> bool {
      (self.inner.esm_class & 0x04) != 0
  }
  ```
  （与 `AGENTS.md` 记录的 `esm_class=0x04` 一致。）
- **验证**：新增单测覆盖 `esm_class=0x04`(报告) / `0x00`(普通 MO) / `0x01`(普通)；`cargo clippy -p rsms-connector` 不再报 error。

### 0.2 ⚠️ 远程可触发 panic / 内存 DoS（解码越界）

- **位置（8 处）**：
  - CMPP：`codec-cmpp/src/datatypes/submit.rs:225-227` ✅、`v20.rs:200-204`、`deliver.rs:107-109`
  - SGIP：`codec-sgip/src/datatypes/submit.rs:209-211` ✅、`deliver.rs:79-83`
  - SMPP：`codec-smpp/src/datatypes/submit_sm.rs:148-150`、`deliver_sm.rs:111-112`、`submit_decode.rs:49-50`(及 114-115)
- **现状**：读取对端可控长度字段后，`let mut v = vec![0u8; declared_len]; buf.copy_to_slice(&mut v);`。当 `buf.remaining() < declared_len` 时 `copy_to_slice` **panic**，连接任务崩溃。**SGIP 的 `message_length` 是 u32**，恶意值可触发最高 ~4GB 预分配（内存 DoS）。
- **参照模板**：`codec-smpp/src/datatypes/tlv.rs:66` 已正确实现 `remaining < length` 校验，复制该模式。
- **改动**：每处 `copy_to_slice` 前加：
  ```rust
  if buf.remaining() < declared_len {
      return Err(CodecError::Incomplete);
  }
  ```
  并对 u32 长度（SGIP）额外加一个合理上限常量（如 `MAX_MSG_LEN`，按协议规范取值），超限返回 `CodecError::Invalid`/`TooLong`，避免在校验前就 `vec![0u8; huge]`（即先判长度再分配）。
- **验证**：为每个协议新增「截断 PDU」「超大 length 字段」解码测试，断言返回 `Err` 而非 panic；可加一个 `should_panic` 反向回归（修复后不再 panic）。复核 8 处中尚未读源码的 6 处，确认 `body_len` 前置校验不足以覆盖（前面变长字段会提前消耗 buffer）。

### 阶段 0 验收
- `cargo clippy --workspace` 0 error；新增解码健壮性单测全绿；四协议集成测试全绿。
- 提交：`fix: SMPP 报告位掩码 + 四协议解码越界防护`

### 阶段 0 实施记录（2026-06-12）
- ✅ 0.1：`transaction/smpp.rs` `is_report` 改为 `& 0x04 != 0`；clippy 在 rsms-connector 上不再报 correctness error（已验证 `cargo clippy -p rsms-connector` Finished）。
- ✅ 0.2：7 处 `copy_to_slice` 前加 `buf.remaining()` 校验（cmpp submit/v20/deliver、sgip submit/deliver、smpp submit_sm/deliver_sm）；v20/sgip 的尾部 `reserve[8]` 也加守卫。**核实发现 `smpp/submit_decode.rs:48/113` 原本已正确守卫，无需改动**（专项分析此处为误报，实际为 7 处而非 8 处）。
- ✅ 新增 3 个回归测试并通过：cmpp `submit_decode_oversized_msg_length_is_err_not_panic`、sgip `submit_decode_oversized_message_length_is_err_not_panic`（u32 超大长度）、connector `is_report_detects_smsc_delivery_receipt_bit`。
- 测试结果：cmpp 40 / sgip 15 / connector 1 全绿。

---

## 阶段 1（P1）：性能热点

> 每项改动后用对应协议单账号 + 多账号压测（WARN 日志）验证：TPS 不低于基线、300s 零丢失。先记录当前基线再动手。

### 1.1 window.offer() 轮询 → 事件唤醒
- **位置**：`crates/rsms-window/src/window.rs`（`offer` 循环 `sleep(50ms)`）
- **改动**：引入 `tokio::sync::Notify` 或容量=`max_size` 的 `Semaphore`，由 `complete`/`cancel`/`fail` 唤醒等待者，移除 50ms 轮询。
- **风险**：中。涉及窗口核心同步语义，需保证无唤醒丢失 / 无死锁。优先用 `Semaphore`（许可=窗口容量）最简单安全。

### 1.2 消除每条消息的全 PDU 拷贝
- **位置**：`crates/rsms-connector/src/client.rs`（发送侧 `pdu.to_vec()` 入窗口；接收侧 `data.to_vec()` + `.clone()`）
- **改动**：窗口仅需 sequence key，不需 payload —— 去掉入窗口的 payload 拷贝；响应路径用 `Bytes`/`Arc<[u8]>` 单次共享，避免二次 clone。
- **风险**：低-中。需确认窗口条目确实不读 payload。

### 1.3 decode_frames_drain 缓冲区搬移
- **位置**：`crates/rsms-connector/src/connection.rs:435` 附近
- **改动**：循环内用读游标/offset 解析，循环结束后一次性 `drain`/`split_to` 压缩；坏长度重同步用 offset 前进而非 `drain(..1)`。可考虑底层换 `bytes::Bytes` 零拷贝切片。
- **风险**：中。是热路径且涉及跨 read 不完整 PDU 累积逻辑，需重点测「半包/粘包/坏包」。

### 1.4 transaction 锁与 key
- **位置**：`crates/rsms-connector/src/transaction/mod.rs`
- **改动**：`RwLock<HashMap<String,_>>` → `DashMap<u32,_>`（直接用 `sequence_id: u32` 做 key，去掉 `to_string()` 堆分配）；消除 `on_submit_resp` 中两把锁嵌套。
- **风险**：中。需保证 seq→msg_id 重定向语义不变（resp 到达后按 msg_id 匹配 report）。

### 1.5 限流器锁类型
- **位置**：`crates/rsms-ratelimit/src/smooth_rate_limiter.rs`
- **改动**：所有路径都走 write 的 `RwLock` → `std::sync::Mutex`（临界区小且非 async），或原子化 token bucket。
- **风险**：低。

### 1.6 write_frame 批量 flush
- **位置**：`crates/rsms-connector/src/connection.rs`、`client.rs`（`run_outbound_fetcher` 每帧 `write_all`+`flush`）
- **改动**：批量 fetch 的一组帧写完后 flush 一次，而非每帧 flush。
- **风险**：低-中。需保证停止/drain 路径仍能及时 flush 残留。

### 阶段 1 验收
- 四协议单账号 + 多账号压测 TPS ≥ 基线、零丢失；`cargo test --workspace` 全绿。
- 提交（建议按子项拆分）：`perf: ...`

---

## 阶段 2（P2）：架构 / 可维护性

### 2.1 移除死代码 crate `rsms-pipeline`
- **现状**：除自身外无任何 crate/example/test 引用（仅文档与 `Cargo.toml` 成员）。`server.rs` 的 `mark_pipeline_ready()` 名不副实。
- **决策点**（需你拍板）：(A) 从 workspace 移除该 crate 并清理 `mark_pipeline_ready()` 等悬挂命名；(B) 保留作为未来方向。**建议 A**。
- **风险**：低（确认无外部引用后）。

### 2.2 4 个 handler 去重
- **位置**：`crates/rsms-connector/src/handlers/{cmpp,smgp,smpp,sgip}.rs`
- **改动**：抽取 auth 结果→响应的通用流程，参数化「构造 connect-resp + command id」的小 trait；`encode_pdu_header` 提取为共享工具。目标去除约 60-65% 重复。
- **风险**：中。四协议响应结构有差异，需逐协议保持字节级一致；改完跑全部集成测试。

### 2.3 API builder 化
- **位置**：`serve()` (server.rs)、`connect()`/`connect_with_pool()` (client.rs)
- **改动**：引入 `ServerBuilder`/`ClientBuilder`（或 `ServeConfig`/`ConnectConfig` + `with_*`），与既有 `EndpointConfig`/`AccountConfig` 风格一致；合并 `connect` 与 `connect_with_pool` 公共部分。
- **风险**：中（对外 API 变更）。**需保留旧函数或提供迁移**：示例/测试同步更新。

### 2.4 trait 清理
- 合并 core / business 两处同名 `ProtocolConnection`（定义到 `rsms-core` 再 re-export）。
- 评估 `ProtocolHandler::submit_limiter()`：返回值无人消费 → 删除或真正接入。
- 评估 `ProtocolHandler` 是否值得保留多态（当前是字符串 match 派发）。
- **风险**：中。跨 crate 改动，逐步验证编译。

### 2.5 crate 边界
- `rsms-core` 的 tokio 依赖裁剪到必要 feature（如仅 `sync`）；评估 `EndpointConfig` 内 `tracing::Level` 是否外移。
- smpp `address.rs` 的 ton/npi 死常量：合并为单个 `#[allow(dead_code)]` 模块或删除。
- **风险**：低-中。

### 阶段 2 验收
- 全量测试（单元 + 四协议集成 + 压测）全绿；对外 API 变更在 README/示例中同步。
- 提交（按子项）：`refactor: ...`

---

## 阶段 3（P3）：clippy 清理

- **现状**：约 40 条 warning，多数可 `cargo clippy --fix` 自动修：needless borrow ×31、unnecessary cast、`map_or` 化简、`if` 可合并、过复杂类型（建 `type` 别名）、函数参数过多（随 2.3 builder 化解决）。
- **改动**：先 `--fix` 跑自动修，再人工处理剩余（复杂类型别名、`is_some()`+`unwrap()` 等），目标 `cargo clippy --workspace --all-targets` 0 warning。
- **风险**：低。但 `--fix` 后必须全量测试，防止自动改动引入语义偏差。
- 提交：`style: 清理 clippy 警告`

---

## 里程碑顺序与依赖

```
阶段0 (P0 正确性/安全)  ──►  阶段1 (P1 性能)  ──►  阶段2 (P2 架构)  ──►  阶段3 (P3 clippy)
   独立、可单独合并          需先有压测基线        含对外API变更，需同步文档    最后统一清理
```

- 阶段 0 与阶段 3 的「自动修」互不依赖，但建议 0 先行（修掉 clippy error 后 P3 才能干净跑 `--fix`）。
- 阶段 2.3（builder）会顺带消掉 P3 的「参数过多」warning，故 2 在 3 之前。

## 决策点（已确认 2026-06-12）
1. **rsms-pipeline**：✅ 直接移除（阶段 2.1 执行 A 方案）。
2. **API builder 化**：✅ builder 化且**不保留旧签名**，同步改示例/文档（阶段 2.3）。
3. **推进节奏**：✅ 先完成阶段 0，验证通过后停下评审，再决定后续。
4. 提交粒度：阶段 0 作为一个修复提交（或按 0.1/0.2 两提交）。
