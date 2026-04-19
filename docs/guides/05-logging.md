# 日志配置

## 概述

框架使用 `tracing` 提供结构化日志，所有连接相关日志自动携带以下字段：

| 字段 | 说明 | 示例 |
|------|------|------|
| `conn_id` | 连接唯一 ID（全局递增） | `conn_id=3` |
| `remote_ip` | 对端 IP 地址 | `remote_ip=10.0.1.5` |
| `remote_port` | 对端临时端口 | `remote_port=54321` |

- **服务端**：`remote_ip`/`remote_port` 是客户端的地址
- **客户端**：`remote_ip`/`remote_port` 是服务端的地址

## 日志输出示例

```
INFO rsms_connector::handlers::cmpp: 收到CMPP连接请求: source_addr=900001, version=30
    conn_id=1 remote_ip=127.0.0.1 remote_port=54321

WARN rsms_connector::connection: connection idle timeout, closing
    conn_id=2 remote_ip=192.168.1.100 remote_port=38972 idle_timeout_secs=60

ERROR rsms_connector::connection: read error: Connection reset by peer
    conn_id=5 remote_ip=10.0.2.50 remote_port=49152
```

## 日志级别控制

### EndpointConfig.log_level

通过 `EndpointConfig` 的 `log_level` 字段控制框架日志输出级别：

```rust
let config = Arc::new(EndpointConfig::new("gateway", "0.0.0.0", 7890, 500, 60)
    .with_protocol("cmpp")
    .with_log_level(tracing::Level::WARN));
```

| 设置值 | 效果 |
|--------|------|
| `None`（默认） | 继承全局 `tracing_subscriber` 设置 |
| `Some(Level::ERROR)` | 框架只输出 ERROR 日志 |
| `Some(Level::WARN)` | 框架输出 WARN 及以上 |
| `Some(Level::INFO)` | 框架输出 INFO 及以上 |
| `Some(Level::DEBUG)` | 框架输出 DEBUG 及以上 |
| `Some(Level::TRACE)` | 框架输出所有日志 |

### 级别过滤机制

框架在高频日志路径（每帧处理）中使用 `should_log()` 守卫，避免不必要的日志格式化开销：

```rust
// 框架内部实现（伪代码）
if conn.should_log(tracing::Level::DEBUG) {
    tracing::debug!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), ...);
}
```

涉及级别过滤的高频路径：
- Submit / SubmitSm 处理（DEBUG）
- Deliver / DeliverSm 处理（DEBUG / INFO）
- 客户端 received frame（INFO）
- 出站 fetcher send（TRACE / DEBUG）

### 性能影响

| 日志级别 | 300s 压测 TPS | 说明 |
|----------|---------------|------|
| TRACE | ~2,700 | 每帧多条日志，IO 瓶颈 |
| INFO | ~2,700 | 每帧一条 INFO，仍然有 IO 瓶颈 |
| WARN | 12,500+ | 高频路径被跳过，无 IO 开销 |

**生产环境建议设为 WARN**，仅保留连接建立/关闭、认证失败等关键事件日志。

### 全局 vs 端点级别

```rust
// 全局 subscriber 控制所有模块
tracing_subscriber::fmt()
    .with_max_level(tracing::Level::INFO)
    .init();

// 端点级别控制框架日志（覆盖全局设置）
let config = EndpointConfig::new("gateway", "0.0.0.0", 7890, 500, 60)
    .with_log_level(tracing::Level::WARN);  // 框架日志只输出 WARN+

// 业务代码的 INFO 日志不受影响
```

## 日志字段详情

### 连接生命周期日志

| 事件 | 级别 | 字段 |
|------|------|------|
| 服务端接受连接 | INFO | `peer`（含 IP:端口） |
| 客户端建立连接 | INFO | `conn_id`, `remote_ip`, `remote_port` |
| 连接空闲超时 | WARN | `conn_id`, `remote_ip`, `remote_port`, `idle_timeout_secs` |
| 连接被框架剔除 | WARN | `conn_id`, `remote_ip`, `remote_port`, `protocol` |
| 读取错误 | ERROR | `conn_id`, `remote_ip`, `remote_port` |
| 帧解码错误 | ERROR | `conn_id`, `remote_ip`, `remote_port` |

### 认证日志

| 事件 | 级别 | 字段 |
|------|------|------|
| 收到连接请求 | INFO | `conn_id`, `remote_ip`, `remote_port`, 协议特定字段 |
| 认证成功 | INFO | `conn_id`, `remote_ip`, `remote_port`, `account` |
| 认证失败 | WARN | `conn_id`, `remote_ip`, `remote_port`, `status` |
| 认证错误 | ERROR | `conn_id`, `remote_ip`, `remote_port`, 错误信息 |

### 消息处理日志

| 事件 | 级别 | 字段 | 频率 |
|------|------|------|------|
| 收到 Submit | DEBUG | `conn_id`, `remote_ip`, `remote_port` | 每条消息 |
| 收到 Deliver | DEBUG/INFO | `conn_id`, `remote_ip`, `remote_port` | 每条消息 |
| 收到心跳 | INFO | `conn_id`, `remote_ip`, `remote_port` | 每次心跳 |
| 收到关闭请求 | INFO | `conn_id`, `remote_ip`, `remote_port` | 断连时 |

### 账号池日志

| 事件 | 级别 | 字段 |
|------|------|------|
| 注册到账号池 | INFO | `conn_id`, `remote_ip`, `remote_port`, `account` |
| 更新账号配置 | INFO | `conn_id`, `remote_ip`, `remote_port`, `account` |
| 剔除多余连接 | INFO | `conn_id`, `remote_ip`, `remote_port`, `account`, `max_connections` |
| 限流器重配置 | INFO | `account`, `old_qps`, `new_qps` |

## 问题查证

### 通过 conn_id 追踪连接

`conn_id` 是全局递增的唯一标识，贯穿该连接的所有日志。使用 `grep` 过滤：

```bash
# 追踪连接 ID=3 的所有事件
grep "conn_id=3" app.log

# 追踪某个 IP 的所有连接
grep "remote_ip=10.0.1.5" app.log

# 追踪某个账号的所有事件
grep "account=900001" app.log
```

### 通过 sequence_id 关联请求和响应

请求和响应通过 `seq_id` / `sequence_id` 字段关联：

```bash
# 查找特定 sequence_id 的请求和响应
grep "seq_id=12345" app.log
```

### 报文内容调试

框架在 DEBUG 级别输出原始报文十六进制数据（仅在解码失败时）：

```
DEBUG rsms_connector::handlers::cmpp: 原始数据 (hex): [0x00, 0x00, ...]
    conn_id=5 remote_ip=192.168.1.100 remote_port=49152
```

正常运行时不会输出报文内容，避免性能影响。
