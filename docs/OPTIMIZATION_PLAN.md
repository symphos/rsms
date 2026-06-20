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

---

## 阶段 4（P4）：压测盲区缺陷 —— 二轮架构研究（2026-06-13）

> 体检方式：三路并行 `architect`（连接核心/并发 · 四协议 codec · API/错误/longmsg/测试）只读分析 + 主调逐项源码核实。
> 标记：✅ = 已逐行核对源码确认为真；⚠️ = agent 报告，实施前需复核。
> **关键洞察**：阶段 0–3 与既有压测覆盖的是**主路径**（固定长连接 5×5×300s、MT/MO 收发、解码安全）。本轮发现的缺陷集中在**压测覆盖不到的区域**——连接断开/重连路径、配置边界、以及 `rsms-longmsg` 业务工具库（connector 内零引用，压测从不经过）。故基线零丢失**并不能**证明这些路径无缺陷。

### 4A 必修（正确性）—— 真实可触发缺陷，逐项已核实

#### 4A.1 ✅ 服务端连接断开后从不从 `AccountConnections` 移除（内存泄漏）
- **位置**：`connection.rs` `run_connection` 收尾仅 `mark_disconnected` + 回调；`server.rs:182` spawn 收尾只 `pool2.remove(id)`（移除的是 `ConnectionPool`）。连接在 `connection.rs` 经 `acc_pool.add_connection` 注册进 `AccountConnections.connections`，但 `remove_connection`(pool.rs:119) 的**唯一调用点**是 `pool.rs:198` 的缩容 `evict_excess_connections`。
- **影响**：每条断开连接的 `Arc<Connection>`（含 `OwnedWriteHalf`、多个 Mutex）永久驻留向量 → 长期重连下内存单调增长；`fetch_available_connection` 线性扫描成本随泄漏增长。`HealthChecker::check_connections` 只打日志不移除。
- **触发**：任何生产环境客户端重连（压测固定长连接，永不触发）。
- **改动**：`run_connection` 收尾取 `account_connections` 调 `remove_connection(conn.id)`（早期未认证即断开的连接本就没注册，逻辑天然安全）；可叠加 `HealthChecker` 兜底回收。
- **验证**：新增「重复连接-断开 N 次后 `connection_count()` 回落」测试。

#### 4A.2 ✅ `SmoothRateLimiter` `max_qps=0` 整数除零 panic
- **位置**：`smooth_rate_limiter.rs:30`（`new`）、`:90`（`set_rate`）：`Duration::from_micros(1_000_000 / max_qps)`。
- **影响**：`AccountConfigProvider` 返回 `max_qps=0`（配置错误）即在账号注册/配置更新的 IO 路径上 panic。
- **改动**：入口 `let qps = max_qps.max(1);`（或返回 `Result`）。
- **验证**：`max_qps=0` 构造不再 panic 的单测。

#### 4A.3 ✅ 长短信 16-bit UDH 分段超长 1 字节（违反 GSM 03.40）
- **位置**：`split.rs:56-62` 分段步长固定 `max_per_segment - UDH_HEADER_LEN`（`UDH_HEADER_LEN=6`，frame.rs:7），但 16-bit 分支 `build_16bit_udh_frame`(split.rs:108-126) 拼 **7** 字节头。故 `frame_content = 7 + 147 = 154 > 153`（GSM 多段上限）。UCS2 分支同理（67-6 vs 67-7）。
- **触发**：`reference_id` 由随机种子生成，**多数 >255 即默认走 16-bit 分支**，几乎每条多段长短信都超长。
- **改动**：先决定 ref_id 宽度，步长按实际 UDH 长度（8-bit=6 / 16-bit=7）计算。
- **验证**：断言每段 `frame_content.len() <= 协议上限`（现有 split→merge 往返测试没断言字节上限，故漏掉）。

#### 4A.4 ✅ 删除 `InMemoryFrameCache`（async 上下文必 panic 的死重复代码）
- **位置**：`cache.rs:49/59/66/75` `put/get/remove/cleanup` 在同步方法内 `Handle::current().block_on()` —— 在 tokio 任务里调用必 panic（"Cannot block the current thread from within a runtime"）。
- **理由**：与 `LongMessageMerger` 功能重复（均 `HashMap + TTL`）且实现质量更低；connector 内**零引用**；构造时已另起后台清理任务，`cleanup()` 方法本身多余。
- **改动**：删除 `cache.rs` 整个 `FrameCache`/`InMemoryFrameCache`（workspace 内零引用，破坏面极小）；统一用 `LongMessageMerger`。
- **验证**：删除后 `cargo build --workspace` + 全测试全绿。

#### 4A.5 ✅ `LongMessageMerger` 不完整分片无自动 TTL 清理（OOM）+ ref_id 跨会话碰撞
- **位置**：`merge.rs:62` `cleanup_expired` 定义但**全仓零调用点**；`frame.rs:120` `unique_id = "{ref_id}-{total}"` 不含主/被叫号码。
- **影响**：MO 接收丢段（常见）→ `PendingEntry` 永久驻留 → 内存无界；16 位 ref_id 不含号码 → 高并发跨会话碰撞 → 两条长短信内容串台。与框架「不缓存 MsgId 避免 OOM」哲学一致——merger 也必须有界。
- **改动**：(a) `add_frame` 内惰性触发 `cleanup_expired`（记 `last_cleanup`）或文档强制调用方周期清理；(b) `unique_id` 纳入号码维度。
- **验证**：「只发 N-1 段 + 推进时间 → `pending_count()` 归 0」「相同 ref_id 不同号码不被错并」两个测试。

#### 4A.6 ⚠️ 编码侧长度字段 `.len() as u8/u32` 静默截断（错帧）
- **位置**：`codec-*/datatypes/submit*.rs` 等约 11 处 `put_u8(x.len() as u8)`（CMPP/SMGP 单字节长度）/`put_u32(... as u32)`。
- **影响**：正文 >255（单字节长度域）时静默截断，PDU 长度字段与实际正文不符 → 对端解析错帧/丢字节。是阶段 0「解码越界守卫」的对称编码面。
- **改动**：encode 入口校验 `len`，超限返回 `CodecError::FieldValidation`。
- **验证**：超长正文 encode 断言返回 `Err` 的单测。

### 4B 健壮性 / 可维护性（建议）
- **4B.1 ✅** 客户端断开时 `pending_responses` 不批量 drain（`client.rs:784` `mark_disconnected` 不清理）→ 在途请求挂到各自 timeout 才失败（有 per-request timeout 兜底，故为延迟尖峰非永久泄漏）。改：断开时 `drain()` 全部 `send(Err(ConnectionClosed))`。
- **4B.2 ✅** `account_pool` 账号条目永不回收（`pool.rs:374` `remove` 无调用点）+ `inbound_fetch_loop` 无退出条件、空账号下僵尸任务空转。改：空闲账号由 `HealthChecker` 定期 `remove`，fetch loop 加退出条件并递减线程计数。
- **4B.3 ⚠️** `EndpointConfig.protocol` 裸 `String`，拼错/漏设静默退化为 cmpp 偏移（AGENTS Discovery#9 记录的 SMPP TPS 骤降根因）。改：`enum Protocol { Cmpp, Smgp, Smpp, Sgip }` 派生 `header_len()`/`seq_offset()`，并由 `with_decoder` 推导。**对外破坏性变更**，需同步示例/文档。
- **4B.4 ⚠️** `RawPdu::sequence_id()`(encoded_pdu.rs:63) 硬编码偏移 8–11，对 SMPP/SGIP（12–15）错误的潜伏陷阱。随 4B.3 一并处理或加文档限定。
- **4B.5 ⚠️** codec→connector `From<CodecError>` 一律 `to_string()` 压成 `String`，连接层丢失 `Incomplete`（半包）vs 真错误的可编程区分。改：`RsmsError` 携带 codec 错误分类枚举，至少保留 `Incomplete` 语义位。

### 4C 性能微调（可选，需 profile 佐证）
- **4C.1 ⚠️** CMPP/SMGP `to_pdu_bytes`(codec.rs:177/144) 用 `BytesMut::new()` 不预留容量 → 每条 encode realloc，且各 PDU 的 `encoded_size()` 沦为死代码。改：`with_capacity(12 + encoded_size())` 对齐 SGIP。
- **4C.2 ⚠️** `transaction::on_report`(mod.rs:193-221) 兜底分支 O(n) 全表扫描；按命中率决定是否加反向索引。
- **4C.3 ⚠️** server `inbound_fetch_loop` inflight 信号量两次独立读 `window_size`（TOCTOU），配置中途改 0 会令计数器永偏高、账号外发卡死。改：单次读 + RAII guard 配对。
- **4C.4 ⚠️** `client.rs:732` 每条响应 `tokio::spawn` 一个 drain-pending 任务 → 任务创建开销 + 并发 drain 竞态。改：长驻单 drain 任务 + `Notify`。
- **4C.5 ⚠️** 锁类型对齐：服务端 `Connection.last_active` 用 `tokio::Mutex<Instant>`，客户端用 `StdMutex`；`last_active` 可 `AtomicU64`，`authenticated_account`/`account_connections` 认证后不变可去锁。
- **4C.6 ⚠️** SGIP/SMPP `message.rs` 解码 body 多余 `.to_vec()`，可借用 `&buf[H..body_end]` 建 Cursor。

### 4D 合规（doc + 测试）
- **4D.1** 补公共 API doc 注释（项目硬约定）：`BusinessHandler`/`InboundContext`（尤其固化「收到 Submit 必须业务方回 Resp」契约）、`MessageSource`/`MessageItem`、longmsg 全部 pub API、各 codec `decode_message`/`encode_message`/`Pdu`/`PduHeader`。
- **4D.2** 下沉三份重复 `CodecError`（CMPP/SMGP/SGIP 近乎逐字节相同）+ `to_command_status()` 到 `rsms-core`，四 crate 复用。
- **4D.3** 补测试盲区：① 长短信分段字节上限（漏掉 4A.3）；② Merger TTL 回收 & ref_id 碰撞（4A.5）；③ 连接断开后资源回收（4A.1/4B.1）；④ `with_protocol` 误用退化行为；⑤ codec `Incomplete` 半包语义跨层。

### 4A 实施记录（2026-06-13）
全部 TDD（先写失败测试看红 → 修 → 验证绿）。回归基线：改动前四协议压测零丢失、TPS 12,542.x。
- ✅ **4A.2 除零**：`smooth_rate_limiter.rs` `new`/`set_rate` 入口 `max_qps.max(1)`；新增 `new_with_zero_qps_does_not_panic`、`set_rate_zero_does_not_panic`（红：两处 `divide by zero` → 绿 5 passed）。
- ✅ **4A.3 16-bit 分段**：`split.rs` 改按实际 UDH 宽度（8-bit=6 / 16-bit=7）算 `payload_per_segment`，段数用 `div_ceil`；import 换 `UDH_HEADER_8BIT_LEN`/`UDH_HEADER_16BIT_LEN`。新增 4 测试（GSM/UCS2 上限断言 + 8-bit 回归 + 去头往返还原）（红：154>153、68>67 → 绿）。
- ✅ **4A.4 删 cache**：删除 `cache.rs`（`InMemoryFrameCache`，async 上下文 `block_on` 必 panic 的死重复代码，全仓零引用）；`lib.rs` 移除 `mod cache` + 再导出。
- ✅ **4A.5 Merger TTL**：`merge.rs` 加 `last_cleanup` 字段，`add_frame` 入口距上次清理超 ttl 即惰性 `cleanup_expired`，防丢段无界堆积 OOM。新增 `cleanup_expired_removes_stale_incomplete_entries`、`add_frame_triggers_lazy_cleanup_of_stale_entries`（红：pending=2≠1 → 绿）。**号码维度 key 已落地**（分支 `fix/longmsg-merger-sender-key`）：`add_frame(sender, frame)` 增发送方参数、分组键改 `sender\u{1f}ref-total`（不改 `LongMessageFrame`，由调用方传发送方），杜绝不同发送方 reference 撞号串合；新增 `different_senders_same_reference_do_not_cross_merge` 回归。出站 reference 种子改进程级分发器去撞号（`fresh_generators_have_distinct_starting_references`）。
- ✅ **4A.1 连接泄漏**：`connection.rs` `run_connection` 收尾补 `conn.account_connections() → remove_connection(conn_id)`，与 `mark_disconnected` 同处注销。新增集成测试 `test_disconnected_connections_removed_from_account_pool`（裸 TcpStream 4 连接，断开前 4 → 断开后 0）。
- ✅ **4A.6 编码截断**：10 处正文长度域 encode 前校验 `len ≤ u8/u32 上限`，超限返回 `CodecError::FieldValidation`（CMPP 4 + SMGP 2 + SMPP 2 + SGIP 2 u32）。四协议各加 encode 超长测试（cmpp43/smgp33/smpp10/sgip18 全绿）。
  - **遗留（→ 4D 死代码清理）**：`codec-cmpp/datatypes/v20.rs:159` `build_submit_v20_pdu`（pub、二级再导出、不返回 Result）同样截断，但**全仓零生产调用点**，生产 V2.0 encode 走已修的 `encode_pdu_submit_v20`，截断 bug **生产不可达**；`message.rs:321` 为 `#[cfg(test)]` 测试夹具。两者均建议作为死代码删除或改签名，不在 4A.6「生产可达缺陷」范围内强行改 API。

### 4B 实施记录（2026-06-13）
- ✅ **4B.1 pending drain**（commit 72963bb）：客户端 `mark_disconnected` 末尾 drain `pending_responses` 发 `ConnectionClosed`，在途请求立即失败而非挂到 `endpoint.timeout`。集成测试断开后 future 0.01s 内返回 Err（远短于 30s timeout）。
- ⏭️ **4B.2 账号/线程回收——暂缓**（用户确认）：账号 map 回收存在 `remove` vs 重连 `get_or_create` 跨双层锁竞态，且账号数通常有界、严重性远低于已修的连接级泄漏（4A.1）；僵尸 `inbound_fetch_loop` 空转可单独安全解决但需共享计数 DashMap 重构 + 压测。仿阶段 2.4「收益/前提不足则暂缓」，移作独立专项。
  - **P1 实测量化**（`cmpp-soak-dynamic-test`）：60 账号轮换，每账号驻留成本 **~17KB**（含 TransactionManager/限流器/空连接 vec），且每账号连接均归零（4A.1 生效）。固定/有界账号（百/千级）影响 KB–MB 级可忽略，**数据印证暂缓合理**；仅百万级海量动态账号长期运行才需实现回收。
- ✅ **4B.3 protocol enum**：新增 `rsms_core::Protocol { Cmpp, Smgp, Smpp, Sgip }`，派生 `header_len()`/`seq_offset()`/`as_str()`/`Display`/`FromStr`；`EndpointConfig.protocol: String → Protocol`，`with_protocol(Protocol)`，从 `rsms_core`/`rsms_connector` 双导出。连接器内部（`send_request` 偏移、`decode_frames_drain`/`encode_close_packet`/handler 分发、server、keepalive、`create_decoder`）全部改枚举（偏移由 `seq_offset()` 派生）；迁移 6 示例 + 17 测试 + tests-common（`FromStr` 解析）+ 11 文档。拼错/漏设协议从「运行期丢消息」变为「编译期报错」。`Protocol` 3 单测 + clippy 零警告 + 全量回归 + 四协议集成全绿。
- ✅ **4B.4 RawPdu::sequence_id 文档警告**：核实该 `EncodedPdu` 方法全仓零调用（连接层用 `Protocol::seq_offset` 解析），硬编码偏移 8–11 仅对 12B 头有效；改 trait 签名波及全部实现且无收益，故加文档警告标注「仅 CMPP/SMGP 适用，其它协议用 `Protocol::seq_offset`」。
- ⏭️ **4B.5 错误分类——暂缓**：核实 `decode_frames_drain`（帧层）靠长度前缀自行判半包（`total > buf.len()-off 即 break`），不调 codec `decode_message`、不依赖 `CodecError::Incomplete`；业务层 decode 处理的是已切好的完整 PDU，不存在半包。故「连接层丢失半包语义」前提不成立，剩余仅业务层错误可读性、无明确消费者。仿 2.4 暂缓。

### 4C 实施记录（2026-06-13）
- ✅ **4C.1 容量预留**（commit 36dff94）：CMPP/SMGP `to_pdu_bytes` 改 `BytesMut::with_capacity(12 + encoded_size())`（对齐 SGIP），消除编码热路径 realloc，并让各 PDU 的 `encoded_size()` 从死代码变真实调用者。压测 TPS 与基线一致。
- ✅ **4C.3 核实为 agent 误报**：`server.rs` `inbound_fetch_loop` 的 `window_size` 是单次读局部变量（line 282），inc(284)/dec(315) 用同一值配对，不存在「两次独立读 window_size」的 TOCTOU。无需改。
- ⏭️ **4C.2/4C.4/4C.5/4C.6 暂缓**：on_report 兜底 O(n)（正常路径 O(1)，命中率未知）、每响应 spawn drain（动 client 读循环有竞态风险）、锁类型对齐、解码去 `to_vec` —— 均为 speculative 微优化，无 profile 数据佐证收益，按 4C「需 profile 佐证」原则暂缓。

### 4D 实施记录（2026-06-13）
- ✅ **4D.1 公共 API doc**：补 `BusinessHandler`/`on_inbound`（写明「框架不自动发 Resp，业务必须 write_frame 回 Resp 否则窗口耗尽假死」核心契约）/`InboundContext`、`MessageSource`/`MessageItem`、longmsg 全部 pub API、四 codec `message.rs`(decode/encode_message) 与 `codec.rs`(Pdu/PduHeader/Encodable/Decodable) 的中文 doc。
- ✅ **4A.6 遗留死代码清理**：删除 codec-cmpp `v20.rs::build_submit_v20_pdu`（pub、含截断隐患、全仓零生产调用）连同私有辅助 `encode_pstring_fixed`、2 个唯一调用测试、`datatypes/mod.rs`+`lib.rs` 再导出；生产 V2.0 编码走 `encode_pdu_submit_v20`。
- ⏭️ **4D.2 CodecError 下沉暂缓**：跨 4 crate 架构改动，且 SMPP 的 `CodecError` 与 CMPP/SMGP/SGIP 不完全相同（去重不彻底），纯去重无功能收益、有破坏 `From` 实现风险，记为可单独专项。
- ✅ **4D.3 测试盲区核实**：分段上限/Merger TTL/连接回收/pending drain 已在 4A/4B 补齐；`with_protocol` 误用经 4B.3 已变编译期（N/A）；半包语义 4B.5 已核实暂缓。无新增缺口。
  - **P0–P2 长稳/异常注入测试落地（生产就绪加固）**：资源采样基建 `ResourceSampler`（RSS/fd + 稳定性断言）；`cmpp-soak-test`（周期断连重连长稳，`RSMS_SOAK_SECS` 驱动，实证 4A.1 零泄漏）；`cmpp-fault-injection-test`（畸形流不崩 + 逐字节分片重组）；`cmpp-soak-dynamic-test`（动态账号轮换，量化 4B.2 ~17KB/账号）；`cmpp-network-fault-test`（**纯 Rust** 故障代理：高延迟认证 + 中途突断清理）。网络层故障用测试内 tokio 代理模拟，**无需 toxiproxy 等外部工具、生产零依赖**。真实运营商联调 + 天级长稳实跑需真实环境（框架入口已就绪）。

### 阶段 4 验收
- 4A 每项 TDD（先写失败测试再修）；`cargo build --workspace` + `cargo test --workspace --lib` + 四协议集成测试全绿；四协议多账号压测复跑零丢失、TPS ≥ 基线。
- 提交按子项拆分：`fix:`（4A/4B 缺陷）、`perf:`（4C）、`docs:`/`test:`（4D）。

### 阶段 4 收官小结（2026-06-13）
二轮架构研究发现的缺陷处理完毕：**4A 全 6 项修复**（连接泄漏/除零/分段超长/死代码/Merger OOM/编码截断），**4B** 完成 4B.1（pending drain）/4B.3（protocol enum）/4B.4（RawPdu doc），**4C** 完成 4C.1（容量预留），**4D** 完成 4D.1（doc）+ 死代码清理。**暂缓项**（均经源码核实前提/收益不足，仿阶段 2.4）：4B.2（账号回收竞态）、4B.5（帧层不依赖 CodecError 判半包）、4C.2/4C.4/4C.5/4C.6（无 profile 的微优化）、4C.3（agent 误报）、4D.2（去重不彻底）。全程 TDD + 单测/集成/clippy 零警告 + 四协议压测零丢失/TPS 一致。

### 决策点（已确认 2026-06-13/14）
- **4B.3 protocol enum**：✅ 已接受并落地（`EndpointConfig.protocol: String → Protocol`，见 4B.3 实施记录）。
- **4A.4 删除 cache**：✅ 已删除（workspace 内零引用，确认无第三方依赖该 pub 类型）。
- **推进顺序**：✅ 4A 全修 → 评审 → 4B/4C/4D 按收益取舍执行完毕。

---

## 阶段 5（P5）：窄腰统一模型升为业务主路径 ✅ 已落地（2026-06-14，PR #1/#2/#3 已合入 main）

> 承接 `docs/superpowers/specs/` 的「统一消息模型 / 协议窄腰」设计。README 旧标注「设计与试点验证阶段、**尚未落地**」**已不成立**——本阶段将其升为**业务主路径并全量迁移**，README/docs 已同步。

### 5.1 ✅ 核心契约与四协议适配器
- 新增 `rsms-model` crate：`UnifiedMessage`（Submit/SubmitResp/Deliver/Report/Bind/BindResp/Ping/Unbind/Unknown…）、`ProtocolAdapter` trait、协议无关类型（`Address`/`MessageId`/`Encoding`/`Sequence`/`ProtocolExtra`/`Tlv`）。
- 四协议 adapter（`rsms_codec_<proto>::adapter::<Proto>Adapter`）实现 decode（Frame→UnifiedMessage）/encode（UnifiedMessage→字节）+ `sequence_of(frame)`（SGIP 复合序列 `Sequence::Sgip` 由此保留）；各补字节往返测试。
- `rsms_connector::adapter_registry::adapter_for(Protocol)` 按枚举动态取适配器。

### 5.2 ✅ 与 Java cmos(SMSGate) 联调 + 修 5 个服务端真 bug
> rsms↔rsms 同 codec 对称测不出，仅第三方客户端能暴露。
- **connector**：accept 后未 `mark_connected` → 服务端 MO/回执从不下发（通用，惠及四协议）。
- **smpp**：BindResp `sc_interface_version` 编成裸字节 → 应为可选 TLV(0x0210)；dcs 0x08(UCS2) 未映射。
- **sgip**：Submit/Deliver/Report_Resp 改 Result(1B)+Reserve(8B)=9B；msg_fmt 4/8 编码弄反已纠正。
- **smgp**：Exit_Resp 多 1 保留字节 → 合规 12B 空体。
- **cmpp**：Connect 鉴权与状态报告二进制解析对齐 cmos。

### 5.3 ✅ 全量迁移 example + test
- 8 个 example（四协议 server/client）全改统一模型，零裸 codec。
- 全部 tests（集成/压测/longmsg/动态）迁统一模型；按协议能力边界保留少量裸 codec（CMPP 2.0 版本化 decode、SMPP `command_status`、SGIP Report_Resp 经 `Unknown`），均注释说明。
- 修迁移暴露的 **SGIP 压测 future 非 Send**：Report 分支 std 锁（edition 2024 下）进 async 协程 witness 跨 await → 锁操作收进同步块、await 前全部释放。

### 5.4 验证
- `cargo build`/`clippy` 全 workspace + `rsms-tests` 包零告警零错误。
- 四协议 integration + longmsg + 事务 ~98 测试全过。
- 8 个 stress 目标实跑零丢失（多账号 60s 各 76 万+ 全链路，Sent=Received=MsgId Matched、Pending=0）；300s 全程 TPS ~12,500+ 不退化。

> **工具链补记**：阶段 0 记录的 rustc 1.94.0 ICE 经本轮定位为 **`annotate_snippets` 渲染器**对「async 协程 Send 多 span 诊断」渲染时切片越界（`StyledBuffer::replace`）。除 `RUSTFLAGS='--cap-lints allow'` 外，**`cargo … --message-format=short` 可避开该渲染器**、拿到真实错误 headline（短格式不渲染源码片段）。
