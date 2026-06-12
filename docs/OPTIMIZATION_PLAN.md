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

### 改动前压测基线（2026-06-12，多账号 5×5×300s，RUST_LOG=warn）
> 采集自 `target/run_baseline.sh`（`*-multi-account-stress-test`，`--test-threads=1`）。

| 协议 | MT Sent=Resp=Report=Matched | Errors | Pending | MO 收发 | TPS(压测300s) | TPS(总时间) | 总时间 |
|------|------|------|------|------|------|------|------|
| CMPP | 3,762,772 | 0 | 0 | 1,885,217 | 12,542.5 | 12,423.7 | 302.87s |
| SMGP | 3,762,787 | 0 | 0 | 1,885,227 | 12,542.6 | 12,423.7 | 302.87s |
| SMPP | 3,762,804 | 0 | 0 | 1,885,237 | 12,542.7 | 12,423.8 | 302.87s |
| SGIP | 3,769,044 | 0 | 0 | 1,889,917 | 12,563.4 | 12,393.3 | 304.12s |

四协议全部 `test result: ok`，零丢失（resp=sent、report 全匹配、MO 全收、pending=0）。

**重要：基线是速率受限的（生产者按 2500/账号 限速，合计 12,500/s 目标）。**
系统已在该目标速率下零丢失，未饱和。因此 P1 性能改动**不会体现为 TPS 上升**（已封顶在
~12,500），其价值在于降低 CPU/锁竞争/拷贝开销、留出更多余量。该基线的作用是：
(1) **零丢失回归基准**（改动后须保持四协议零丢失）；(2) 若要量化吞吐上限提升，需另跑
一轮提高/解除限速的饱和压测（可选）。

### 阶段 1 实测路径覆盖（来自热路径源码核对）
- 压测 MT 走 `run_outbound_fetcher`→`write_frame`（不走 window），服务端经 `decode_frames_drain`→handler→`write_frame` 回 resp；故基线直接覆盖 **1.3 / 1.5 / 1.6**。
- **1.1（window.offer）/ 1.2（send_request/读循环 clone）/ 1.4（transaction 锁）不在压测路径上**，靠单测+集成测试验证正确性，其性能收益面向 `send_request`/`TransactionManager` 使用方。

### 阶段 1 实施记录（2026-06-12）
- ✅ **1.1 window.offer**：`window.rs` 新增 `Notify`（`space_available`），`offer()` 用「先 `enable()` 注册再 try_offer」避免丢唤醒 + `select!{notified | sleep(remaining)}`，替代 50ms 轮询；`complete/fail/cancel`/超时清理后 `notify_waiters()`。新增单测 `test_offer_wakes_on_complete`。
- ✅ **1.2 去拷贝**：`send_request` 不再把整包 `to_vec()` 塞进 window（窗口仅用 seq 匹配，传空 `Vec`）；读循环去掉响应体的多余 `.clone()`（`window.complete` 不读响应体）。
- ✅ **1.3 decode 缓冲区**：`connection.rs::decode_frames_drain` 与 `client.rs::decode_frames` 改为读游标 `off` 解析 + 循环末一次性 `drain(..off)`，消除每帧 `drain(..total)` 的 O(N·buflen) 搬移；坏长度重同步用 `off += 1`。
- ✅ **1.5 限流器锁**：`SmoothRateLimiter` `tokio::RwLock` → `std::sync::Mutex`（guard 不跨 await）。
- ✅ **1.6 批量 flush**：`Connection`/`ClientConnection` 新增 `write_frames`（末尾单次 flush）；客户端 `run_outbound_fetcher` 与服务端外发循环按一次 fetch 的整批写出（分组连续保序）。
- ⏸️ **1.4 transaction 锁——暂缓**：`TransactionManager` 用单写锁保证 `transactions`/`seq_to_msg_id` 两表在 `on_submit_resp`（按 seq 删 + 按 msg_id 插）与 `on_report` 之间的原子一致性。直接换 `DashMap` 会牺牲该一致性、可能造成偶发漏匹配报告；且该组件不在压测路径上、无可量化收益。**移至独立变更**，配并发测试再做。
- 验证：`cargo test --workspace --lib` 全绿；15 个非压测 `rsms-tests` 目标全绿（含 longmsg/dynamic/transaction）；window 新增唤醒测试通过。

### ⚠️ 工具链问题：rustc 1.94.0 ICE（与本次改动无关）
- `cargo test` 编译部分 `tests/` 下测试文件时 rustc **崩溃（ICE）**：query stack 为 `early_lint_checks → lint_level_impl` 在「发射某 lint 诊断」时 panic（`alloc/vec/mod.rs:2873`）。
- **已证实为既有问题**：将本次 P1 改动 `git stash` 回退到提交版后，`smpp-longmsg-test` 仍 ICE；且崩溃发生在测试文件自身 AST 早期 lint（早于解析外部 crate），与库改动无关。
- **绕过方式**：`RUSTFLAGS='--cap-lints allow'`（仅抑制 lint 发射，不影响 codegen/运行时）。本仓库压测与全部测试均在此 flag 下通过。
- 建议：升级/切换 rustc 版本，或向 rust-lang/rust 提 ICE 报告。

---

## 阶段 0 补强（P0.3）：解码器全量加固（由 P0/P1 代码审查触发）

对已提交的 P0/P1 做了 `code-reviewer` + `critic` 双路对抗审查。结论：**两路均通过**（无 Critical/High），
P1 的 window/decode/flush/锁/去拷贝改动均验证正确。但审查追查「sgip message.rs 是否可达」时，
发现 **P0.2（解码越界防护）此前打错了路径**：

| 协议 | 服务端实际解码路径 | P0.2 当时改的位置 | 实况 |
|------|------|------|------|
| CMPP | `decode_message_with_version`→registry→`datatypes::*::decode` | 同一处 ✅ | 实路径已修 |
| SMPP | `decode_message_with_version`→`submit_decode.rs`（本就有守卫） | `datatypes/*`（**非实路径**） | 实路径本安全 |
| SGIP | `decode_message`（**内联**解码） | `datatypes/*`（**非实路径**） | 实路径曾漏 |
| SMGP | `decode_message`→`datatypes::*::decode` | **P0.2 完全没碰** | 实路径曾漏 |

进一步用针对**实路径** `decode_message` 的截断测试实证：不止 `copy_to_slice`，**未受保护的
`get_u8()`/`get_u32()` 在 body 过短时同样越界 panic**（`bytes` 在 under-read 时 panic）——
这是比 P0.2 更广的一整面，最初的健壮性分析也没覆盖。`panic=unwind`（默认），故畸形 PDU
会断开该连接（非整进程 DoS）。

**全量加固（4 路并行 executor 完成）**：把四个 codec crate 解码路径里所有会 panic 的
`get_u8/u16/u32/u64/copy_to_slice` 换成 fallible 的 `try_get_*`/`try_copy_to_slice`
（映射 `CodecError::Incomplete` / `RsmsError::Codec`），共约 283 处；编码/`put_*`/测试代码不动。
另：
- SGIP/SMPP `message.rs` 顶部 `buf[H..total_length]` 切片按 `min(buf.len())` 钳制，防越界 panic。
- 为四协议各加「短 body 不 panic」模糊回归（`decode_message_short_body_never_panics`，遍历各命令 × body_len 0..48），全部通过。
- 发现并修复 **SMPP `src/tests.rs` 是孤儿模块**（从未被声明 → 此前根本没编译/运行），已在 lib.rs 接入，8 个 roundtrip 测试现在真正运行。

验证：`cargo test --workspace --lib` 全绿（含四个新模糊测试）；四协议集成测试全绿（cmpp15/cmpp20-8/sgip8/smgp9/smpp9）；多账号压测复跑零丢失（见下）。

### 审查记录的 follow-up
- ✅ **P1-M1**（critic/code-reviewer 共识）：`write_frames` 批量写中途失败可能在线上留半个 PDU（流错位），剩余 PDU 又无法重发（`MessageSource` 至多一次）。**已修**：客户端/服务端写失败即 `mark_disconnected`，不复用错位的流。
- ✅ **1.4 transaction 锁**：**已完成**——两表改 `DashMap`，`on_submit_resp` 用「先 msg_id 落键、再删 seq 键」保持一致性，配并发零漏匹配回归测试。
- **P1-M2**：`run_outbound_fetcher` 取消逐条 `ready_for_fetch()`（改批粒度）。低风险；P1-M1 的「写失败即拆连接」已覆盖「向正在拆除的连接写入」的主要后果。视为可接受，暂不再改。

---

## 阶段 2（P2）：架构 / 可维护性

### 2.1 移除死代码 crate `rsms-pipeline` ✅ 已完成（2849f11）
- 复核确认除自身外无引用；已删除 crate，并把 `mark_pipeline_ready()` 重命名为 `mark_ready()`（实际只是置 ready 位，与该 crate 无关）。

### 2.2 4 个 handler 去重 ✅ 已完成（b9d9393）
- 每个 handler 内把重复的认证响应构造收成本文件助手：CMPP `send_connect_resp`（4 处）、SMGP `send_login_resp`（4 处）、SGIP `send_bind_resp`（3 处）；SMPP 本就用 `extract_bind_info` 去重过，仅修 unwrap。
- 全部 `resp.encode(...).unwrap()` 改 `.map_err(RsmsError::Codec)?`（修审查 Medium 项；响应为定长结构实际不会失败，行为不变）。
- 未做跨协议 trait 抽象（关键 connect 路径风险/收益不划算）。净减 48 行。
- 验证：`cargo clippy -p rsms-connector --lib` 0 警告；四协议集成测试全绿（含 auth 路径）。

### 2.3 API builder 化 ✅ 已完成
- 引入 `ServerBuilder`（替换 `serve()`）、`ClientBuilder`（替换 `connect()`，泛型 over decoder）；`connect_with_pool` 降为 `pub(crate)` 内部 API。**不保留旧签名**（按决策）。
- 全量迁移 ~30 文件：8 个 example + `tests/common` + ~20 个测试文件（9 server + 23 client 调用点），README/CLAUDE.md/docs 指南同步改为 builder 写法。
- 验证：`cargo build --workspace --tests` 全绿；workspace 单测 + 11 个集成/dynamic/transaction 目标全绿；多账号 5×5×300s 四协议压测**零丢失、TPS 与基线一致**（CMPP/SMGP/SMPP 12,542.x，SGIP 12,563.5）。

### 2.4 trait 清理 ⏭️ 经核实，多数前提不成立 → 跳过
- **`submit_limiter()` 并非死代码**：在 `connection.rs:236/293` 经 `SubmitLimiterAdapter` 桥接到 business 的 `rate_limiter()`，而后者被真实业务 handler 使用（`tests/cmpp/cmpp_test.rs:148/175`）。删除会破坏限流桥接。
- **两处同名 `ProtocolConnection` 是有意的双层视图**：connector 的全量 trait（handler↔连接，14 方法）+ business 的最小视图（4 方法），由适配器桥接。合并会迫使 business 层依赖更多。
- 唯一残留是「同名」造成的认知成本（connection.rs 用 `as BusinessProtocolConnection` 别名）——纯命名问题，公开 API 重命名收益低，暂不做。

### 2.5 crate 边界 ✅ 已完成（65b977f）
- `rsms-core` tokio features 收窄 `["sync","rt","macros","time"]` → `["sync"]`，rt/macros 移 dev-deps。
- SMPP `address.rs` ton/npi 逐项 `#[allow(dead_code)]` 合并为模块级单标注（保留为规范参考表）。
- `EndpointConfig` 内 `tracing::Level`：用于按端点控制日志级别，属合理配置项，不外移。

### 阶段 2 验收
- 全量测试（单元 + 四协议集成 + 压测）全绿；对外 API 变更在 README/示例中同步。
- 提交（按子项）：`refactor: ...`

---

## 阶段 3（P3）：clippy 清理

✅ 已完成（848c5fc）
- `cargo clippy --fix` 自动修约 20 处；手动加 5 个类型别名（`PduDecodeFn` / `ReadyCallbackFn` / `UnhealthyCallback`×3）消除「过复杂类型」；内部编排函数 `run_connection` 加 `#[allow(clippy::too_many_arguments)]`。
- 结果：**`cargo clippy --workspace --lib` 0 警告**。
- 验证：workspace 全部 lib 测试 + 15 个集成/longmsg/dynamic/transaction 目标（111 测试）全绿。
- 注：`--all-targets` 因 rustc 1.94 在部分测试文件上的既有 ICE 无法纯净运行，故以 `--lib` 为准（见阶段0补强中的 ICE 说明）。

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
