# 测试参考

> 集成 / 压测 / 长短信等测试已统一收进 **`rsms-tests`** 包（`tests/` 目录），按
> `tests/<协议>/<文件>.rs` 组织，共享辅助在 `tests/common/`（crate 名 `rsms-test-common`）。
> 旧文档里的 `-p cmpp-endpoint-example --test cmpp-integration-tests` 等包名/目标名均已废弃。

## 测试架构（窄腰统一模型）

测试代码（服务端业务处理器 + 客户端/对端模拟）与 `examples/` 一致，**统一走
`rsms_model::UnifiedMessage` + 各 `*Adapter`**（`rsms_codec_<proto>::adapter::<Proto>Adapter`）：

- 收包：`<Proto>Adapter.decode(frame) -> UnifiedMessage`，按统一枚举分支
- 发包/回执：构造 `UnifiedMessage` -> `<Proto>Adapter.encode(&msg, seq)`；回复请求用
  `<Proto>Adapter.sequence_of(frame)` 回显序列（SGIP 复合序列由此保留）

按协议能力边界保留裸 codec 的少数处（已在源码注释）：
- **CMPP**：CMPP 2.0 版本化 decode 与 V2.0 PDU 构造（adapter 仅 V3.0）
- **SMPP**：客户端读 PDU 头 `command_status`（adapter 不透传结果码，失败用例需区分成败）
- **SGIP**：`Report_Resp` 经 `UnifiedMessage::Unknown{command_id=ReportResp}`（统一模型无该变体）

## 测试分类与目标名

`cargo test -p rsms-tests --test <目标名>`，目标名见 `tests/Cargo.toml` 的 `[[test]]`，规律为 `<协议>-<类型>`。

| 类型 | 源文件 | 目标名示例 | 说明 |
|------|--------|-----------|------|
| 集成测试 | `tests/<proto>/integration.rs`（CMPP 为 `cmpp_test.rs`） | `cmpp-integration` / `smgp-integration` / `smpp-integration` / `sgip-integration` | 协议基本功能端到端验证 |
| 单账号压测 | `tests/<proto>/stress_test.rs` | `cmpp-stress-test` … | 1 连接 + 5 连接，默认 30 秒 |
| 多账号压测 | `tests/<proto>/multi_account_stress_test.rs` | `cmpp-multi-account-stress-test` … | 5 账号 × 5 连接，默认 300 秒 |
| 长短信 | `tests/<proto>/<proto>_longmsg_test.rs` | `cmpp-longmsg-test` … | 长短信拆分/合包 |
| 动态调整 | `tests/<proto>/dynamic_connection_test.rs` | `cmpp-dynamic-connection-test` … | 连接数动态调整 |

CMPP 额外目标：`cmpp20-test`（CMPP 2.0 专项）、`cmpp-transaction-test`、`cmpp-transaction-manager-test`、
`cmpp-soak-test`、`cmpp-soak-dynamic-test`、`cmpp-fault-injection-test`、`cmpp-network-fault-test`。
SMGP 额外目标：`smgp-unified-pilot-test`（窄腰试点单测）。

## 测试统计（本次实测，集成 / 长短信）

| 协议 | 集成 | 其它集成类 | 长短信 |
|------|------|-----------|--------|
| CMPP | `cmpp-integration` 15 | `cmpp20-test` 8 / `cmpp-transaction-test` 3 / `cmpp-transaction-manager-test` 9 | `cmpp-longmsg-test` 11 |
| SMGP | `smgp-integration` 9 | `smgp-unified-pilot-test` 1 | `smgp-longmsg-test` 7 |
| SMPP | `smpp-integration` 9 | — | `smpp-longmsg-test` 11 |
| SGIP | `sgip-integration` 8 | — | `sgip-longmsg-test` 7 |

压测用例数：CMPP `stress` 3 / SMGP `stress` 2 / SMPP `stress` 4（V3.4·V5.0 × 1·5连）/ SGIP `stress` 2；
各协议 `multi-account-stress` 各 1。

## 运行命令

### 单元测试（核心 crate 库测试）

```bash
cargo test --workspace --lib
# 或按 crate：
cargo test -p rsms-core -p rsms-connector -p rsms-longmsg
cargo test -p rsms-codec-cmpp -p rsms-codec-smgp -p rsms-codec-smpp -p rsms-codec-sgip
```

### 集成测试

```bash
cargo test -p rsms-tests --test cmpp-integration
cargo test -p rsms-tests --test smgp-integration
cargo test -p rsms-tests --test smpp-integration
cargo test -p rsms-tests --test sgip-integration
```

### 压测

```bash
# 单账号压测（默认约 30 秒）
cargo test -p rsms-tests --test cmpp-stress-test -- --nocapture

# 多账号压测（默认约 300 秒 = 5 分钟）
cargo test -p rsms-tests --test cmpp-multi-account-stress-test -- --nocapture
```

> **重要**：压测必须以日志级别 **WARN** 运行——压测的 `EndpointConfig` 已 `.with_log_level(WARN)`，
> 切勿调低。INFO 下 300s 会刷 240 万+ 行日志，TPS 从 ~12,500 崩到 ~2,700。
>
> 临时缩短压测时长：把对应文件里的 `const STRESS_TEST_DURATION_SECS` 改小（断言 `expected_min`
> 随时长按比例缩放，不会因缩短而失败），跑完用 `git checkout` 还原。

### 长短信 / 动态调整

```bash
cargo test -p rsms-tests --test cmpp-longmsg-test -- --nocapture
cargo test -p rsms-tests --test smgp-longmsg-test -- --nocapture
cargo test -p rsms-tests --test smpp-longmsg-test -- --nocapture
cargo test -p rsms-tests --test sgip-longmsg-test -- --nocapture

cargo test -p rsms-tests --test cmpp-dynamic-connection-test -- --nocapture
cargo test -p rsms-tests --test smgp-dynamic-connection-test -- --nocapture
cargo test -p rsms-tests --test smpp-dynamic-connection-test -- --nocapture
cargo test -p rsms-tests --test sgip-dynamic-connection-test -- --nocapture
```

> 偶发：连续压测可能出现单目标端口竞争超时，属环境抖动非回归，单独重跑即可。

## 压测性能参考

> 全链路（submit → resp → report → MO）零丢失：每用例 **Sent = Received = MsgId Matched，Pending = 0**。

### 单账号（1 账号，30s）

| 协议 | 连接数 | MT TPS（压测时间） | MT TPS（总时间） | 消息丢失 |
|------|--------|-------------------|-----------------|----------|
| CMPP V3.0 | 1 | 2,500 | 2,383 | 0 |
| CMPP V3.0 | 5 | 2,500 | 2,250 | 0 |
| CMPP V2.0 | 1 | 2,500 | 2,383 | 0 |
| SMGP | 1 | 2,500 | 2,356 | 0 |
| SMGP | 5 | 2,567 | 2,311 | 0 |
| SMPP V3.4 | 1 | 2,500 | 2,365 | 0 |
| SMPP V3.4 | 5 | 2,500 | 2,250 | 0 |
| SMPP V5.0 | 1 | 2,500 | 2,365 | 0 |
| SMPP V5.0 | 5 | 2,500 | 2,251 | 0 |
| SGIP | 1 | 2,500 | 2,365 | 0 |
| SGIP | 5 | 2,500 | 2,250 | 0 |

### 多账号（5 账号 × 5 连接 = 25 连接，60s 实测）

| 协议 | 60s 内全链路消息数 | 消息丢失 |
|------|-------------------|----------|
| CMPP | 762,843 | 0 |
| SMGP | 762,791 | 0 |
| SMPP | 762,781 | 0 |
| SGIP | 769,050 | 0 |

> 300s 全程下各协议总 MT TPS ~12,500+（每账号 ~2,500+），消息零丢失。
