# 动态连接数调整

## 概述

框架支持在运行时动态调整账号的连接数上限和 QPS，无需重启服务。

## 核心机制

### AccountPool.update_config()

```rust
// 动态更新账号配置
account_pool.update_config("900001", AccountConfig::new()
    .with_max_connections(3)   // 从 5 调整为 3
    .with_max_qps(500)         // 从 1000 调整为 500
    .with_window_size(2048)
).await?;
```

调用后框架立即执行：
1. 更新 rate limiter（QPS 立即生效）
2. 如果 `max_connections` 减小，自动剔除多余连接

### 自动剔除流程

当 `max_connections` 从 5 调整为 3 时：

```
1. update_config() 更新配置
      ↓
2. evict_excess_connections() 执行剔除
      ↓
3. 按优先级排序连接：
   - 非 ready（空闲）连接优先剔除
   - 其次按最后活跃时间倒序（最久不活跃的优先剔除）
      ↓
4. 对每个被剔除的连接：
   a. 设置 ready = false（立即停止 fetch 分配）
   b. 发送协议层 Close Packet（CMPP Terminate / SMGP Exit / SMPP Unbind / SGIP Unbind）
   c. 关闭 TCP 写端（shutdown）
   d. 从 AccountConnections 移除
      ↓
5. 客户端 read loop 检测到 Close Packet + EOF
      ↓
6. 客户端 ClientEventHandler.on_disconnected() 被触发
```

### 剔除优先级

| 优先级 | 条件 | 说明 |
|--------|------|------|
| 1（最高） | `ready == false` | 非 ready 连接（空闲或已断开） |
| 2 | `last_active` 最久远 | 最久未活跃的连接 |

### 关闭包（Close Packet）

框架在剔除连接前发送协议层关闭命令，让客户端明确知道是服务端主动关闭：

| 协议 | 关闭命令 | Command ID | Body |
|------|----------|------------|------|
| CMPP | Terminate | 0x00000001 | 空 |
| SMGP | Exit | 0x00000003 | 1 字节 reserve |
| SMPP | Unbind | 0x00000006 | 空 |
| SGIP | Unbind | 0x00000001 | 空 |

## 使用场景

### 场景 1：调低连接数（5 → 3）

```rust
// 运营要求从 5 连接调整为 3 连接
account_pool.update_config("900001", AccountConfig::new()
    .with_max_connections(3)
    .with_max_qps(500)
    .with_window_size(2048)
).await?;

// 框架自动：
// - 关闭 2 个多余连接（优先空闲连接）
// - 发送 Close Packet 后再关闭 TCP
// - 剩余 3 个连接正常工作
```

### 场景 2：调低 QPS

```rust
// 只调整 QPS，不影响连接数
account_pool.update_config("900001", AccountConfig::new()
    .with_max_connections(5)    // 保持不变
    .with_max_qps(200)          // 从 1000 降为 200
    .with_window_size(2048)
).await?;

// 框架立即更新令牌桶，QPS 生效
```

### 场景 3：调高上限

```rust
// 调高 max_connections 不会自动创建新连接
account_pool.update_config("900001", AccountConfig::new()
    .with_max_connections(10)   // 从 5 调高为 10
    .with_max_qps(5000)
    .with_window_size(2048)
).await?;

// 当前 5 个连接保持不变
// 新连接可以继续建立（不超过 10 个上限）
```

### 场景 4：多步调整

```rust
// 5 → 3 → 1
account_pool.update_config("900001", AccountConfig::new()
    .with_max_connections(3).with_max_qps(500).with_window_size(2048)
).await?;

// 等待稳定
tokio::time::sleep(Duration::from_secs(2)).await;

account_pool.update_config("900001", AccountConfig::new()
    .with_max_connections(1).with_max_qps(200).with_window_size(2048)
).await?;
```

## 客户端感知断连

当服务端剔除连接时，客户端通过以下方式感知：

1. **ClientEventHandler.on_disconnected(conn_id)** — 连接断开回调
2. **ConnectionEvent::Disconnected(id)** — 如果使用 `ClientPool`，通过事件通道通知
3. **write_frame() 返回错误** — 尝试发送时发现连接已关闭

客户端收到的是协议层 Close Packet（如 CMPP Terminate），而非突然的 TCP 断开，因此可以区分"服务端主动关闭"和"网络异常"。

## 日志

框架日志自动携带 `remote_ip` 和 `remote_port` 字段，便于追踪连接来源：

| 级别 | 日志内容 | 示例 |
|------|----------|------|
| INFO | `evicting excess connection` | `conn_id=3 remote_ip=10.0.1.5 remote_port=54321 evicting excess connection` |
| INFO | `connection eviction completed` | `evicted=2 remaining=3 max_connections=3` |
| WARN | `connection closed by framework (evict)` | `conn_id=3 remote_ip=10.0.1.5 remote_port=54321 protocol=cmpp` |
| WARN | `connection disconnected` | ClientPool 感知断连 |

更多日志配置参见 [日志配置](05-logging.md)。

## 配置参考

| 参数 | 说明 | 建议值 |
|------|------|--------|
| `max_connections` | 账号最大连接数 | 1-10 |
| `max_qps` | 账号最大 QPS | 根据运营商合同 |
| `window_size` | 滑动窗口大小 | 2048（压测） |

## 参考测试

| 协议 | 测试文件 |
|------|----------|
| CMPP | `examples/cmpp-endpoint/tests/dynamic_connection_test.rs`（4 个测试） |
| SMGP | `examples/smgp-endpoint/tests/dynamic_connection_test.rs`（2 个测试） |
| SMPP | `examples/smpp-endpoint/tests/dynamic_connection_test.rs`（2 个测试） |
| SGIP | `examples/sgip-endpoint/tests/dynamic_connection_test.rs`（2 个测试） |

运行命令：
```bash
RUST_LOG=warn cargo test -p cmpp-endpoint-example --test dynamic-connection-test -- --nocapture
```
