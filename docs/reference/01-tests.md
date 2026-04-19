# 测试参考

## 测试分类

每个协议的测试分为以下几类：

| 类型 | 文件模式 | 说明 |
|------|----------|------|
| 集成测试 | `tests/mod.rs` | 协议基本功能验证 |
| 单账号压测 | `tests/stress_test.rs` | 1 连接 + 5 连接，30 秒 |
| 多账号压测 | `tests/multi_account_stress_test.rs` | 5 账号 × 5 连接，300 秒 |
| 长短信测试 | `tests/<protocol>_longmsg_test.rs` | 长短信拆分/合包验证 |
| 动态调整测试 | `tests/dynamic_connection_test.rs` | 连接数动态调整 |

## 测试统计

### CMPP

| 测试文件 | 测试数 | 说明 |
|----------|--------|------|
| `tests/mod.rs` | 37 | 集成测试（含 V2.0/V3.0 和 TransactionManager） |
| `tests/stress_test.rs` | 2 | 单账号 1 连接 + 5 连接 |
| `tests/multi_account_stress_test.rs` | 1 | 多账号 5×5×300s |
| `tests/cmpp_longmsg_test.rs` | 11 | 长短信（3 纯逻辑 + 4×V2.0 + 4×V3.0） |
| `tests/dynamic_connection_test.rs` | 4 | 动态调整 + rate limiter |

### SMGP

| 测试文件 | 测试数 | 说明 |
|----------|--------|------|
| `tests/mod.rs` | 9 | 集成测试 |
| `tests/stress_test.rs` | 2 | 单账号 1 连接 + 5 连接 |
| `tests/multi_account_stress_test.rs` | 1 | 多账号 5×5×300s |
| `tests/smgp_longmsg_test.rs` | 7 | 长短信 |
| `tests/dynamic_connection_test.rs` | 2 | 动态调整 |

### SMPP

| 测试文件 | 测试数 | 说明 |
|----------|--------|------|
| `tests/mod.rs` | 9 | 集成测试 |
| `tests/stress_test.rs` | 4 | V3.4 × 1/5 连接 + V5.0 × 1/5 连接 |
| `tests/multi_account_stress_test.rs` | 1 | 多账号 5×5×300s |
| `tests/smpp_longmsg_test.rs` | 11 | 长短信（3 纯逻辑 + 4×V3.4 + 4×V5.0） |
| `tests/dynamic_connection_test.rs` | 2 | 动态调整 |

### SGIP

| 测试文件 | 测试数 | 说明 |
|----------|--------|------|
| `tests/mod.rs` | 8 | 集成测试 |
| `tests/stress_test.rs` | 2 | 单账号 1 连接 + 5 连接 |
| `tests/multi_account_stress_test.rs` | 1 | 多账号 5×5×300s |
| `tests/sgip_longmsg_test.rs` | 7 | 长短信 |
| `tests/dynamic_connection_test.rs` | 2 | 动态调整 |

## 运行命令

### 单元测试（核心 crate）

```bash
cargo test -p rsms-core -p rsms-connector -p rsms-longmsg -p rsms-codec-cmpp -p rsms-codec-smgp -p rsms-codec-smpp -p rsms-codec-sgip
```

### 集成测试

```bash
# CMPP
cargo test -p cmpp-endpoint-example --test cmpp-integration-tests

# SMGP
cargo test -p smgp-endpoint-example --test smgp-integration-tests

# SMPP
cargo test -p smpp-endpoint-example --test smpp-integration-tests

# SGIP
cargo test -p sgip-endpoint-example --test sgip-integration-tests
```

### 压测

```bash
# 单账号压测（约 30 秒）
RUST_LOG=warn cargo test -p cmpp-endpoint-example --test stress-test -- --nocapture

# 多账号压测（约 300 秒 = 5 分钟）
RUST_LOG=warn cargo test -p cmpp-endpoint-example --test multi-account-stress-test -- --nocapture
```

> **重要**：压测日志级别必须设为 `WARN`，否则 INFO 日志会导致 TPS 从 12500 降到 2700。

### 长短信测试

```bash
cargo test -p cmpp-endpoint-example --test cmpp-longmsg-test -- --nocapture
cargo test -p smgp-endpoint-example --test smgp-longmsg-test -- --nocapture
cargo test -p smpp-endpoint-example --test smpp-longmsg-test -- --nocapture
cargo test -p sgip-endpoint-example --test sgip-longmsg-test -- --nocapture
```

### 动态调整测试

```bash
RUST_LOG=warn cargo test -p cmpp-endpoint-example --test dynamic-connection-test -- --nocapture
RUST_LOG=warn cargo test -p smgp-endpoint-example --test smgp-dynamic-connection-test -- --nocapture
RUST_LOG=warn cargo test -p smpp-endpoint-example --test smpp-dynamic-connection-test -- --nocapture
RUST_LOG=warn cargo test -p sgip-endpoint-example --test sgip-dynamic-connection-test -- --nocapture
```

## 压测性能参考

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

### 多账号（5 账号 × 5 连接，300s）

| 协议 | 总 MT TPS | 每账号 MT TPS | 消息丢失 |
|------|-----------|-------------|----------|
| CMPP | ~12,500+ | ~2,500+ | 0 |
| SMGP | ~12,500+ | ~2,500+ | 0 |
| SMPP | ~12,500+ | ~2,500+ | 0 |
| SGIP | ~12,500+ | ~2,500+ | 0 |
