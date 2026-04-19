# RSMS - 多协议短信消息中间件框架

Rust 实现的多协议短信网关框架，支持 **CMPP 2.0/3.0**、**SMGP 3.0.3**、**SMPP 3.4/5.0**、**SGIP 1.2** 四种运营商短信协议。

## 核心特性

- **四协议统一抽象**：服务端/客户端 API 统一，切换协议只需改 Decoder 和 EndpointConfig
- **高吞吐**：单账号 TPS 2500+，5 账号并发 TPS 12500+，消息零丢失
- **动态调整**：运行时动态调整连接数上限和 QPS，自动剔除多余连接
- **长短信**：内置长短信拆分/合包（`rsms-longmsg`），支持 8-bit 和 16-bit UDH
- **结构化日志**：所有连接日志自动携带 `remote_ip`/`remote_port`，支持按端点配置日志级别
- **消息队列**：`MessageSource` trait 按账号隔离，业务方自己管理内存队列

## 架构概览

```
┌──────────────────────────────────────────────────────────────┐
│                        使用方（业务代码）                       │
│                                                              │
│  ┌──────────────┐  ┌──────────────┐  ┌────────────────────┐ │
│  │ AuthHandler   │  │ Business     │  │ MessageSource      │ │
│  │ (认证逻辑)    │  │ Handler      │  │ (待发消息队列)      │ │
│  │              │  │ (业务处理)    │  │ fetch(account,batch)│ │
│  └──────────────┘  └──────────────┘  └────────────────────┘ │
└───────────────────────────┬──────────────────────────────────┘
                            │
┌───────────────────────────┼──────────────────────────────────┐
│                     rsms-connector                           │
│                           │                                  │
│  ┌────────────┐  ┌───────┴────────┐  ┌───────────────────┐  │
│  │ Protocol   │  │ AccountPool    │  │ Connection Pool   │  │
│  │ Handler    │  │ (账号隔离)      │  │ (连接管理)         │  │
│  │ (帧处理)   │  │  ├ rate_limiter│  │                   │  │
│  └────────────┘  │  ├ connections │  └───────────────────┘  │
│                  │  └ max_conn   │                          │
│                  └───────────────┘                          │
└───────────────────────────┬──────────────────────────────────┘
                            │
┌───────────────────────────┼──────────────────────────────────┐
│  rsms-codec-cmpp / smgp / smpp / sgip                       │
│           (协议编解码，Header 解析，PDU 序列化)                 │
└───────────────────────────┬──────────────────────────────────┘
                            │
                        TCP Socket
```

## Crate 结构

| Crate | 说明 |
|-------|------|
| `rsms-core` | 核心类型：`Frame`、`RawPdu`、`EncodedPdu` trait、`EndpointConfig` |
| `rsms-connector` | 连接管理：服务端 `serve()`、客户端 `connect()`、`AccountPool`、`MessageSource` |
| `rsms-business` | 业务处理器：`BusinessHandler` trait、`run_chain()` |
| `rsms-codec-cmpp` | CMPP 2.0/3.0 协议编解码 |
| `rsms-codec-smgp` | SMGP 3.0.3 协议编解码 |
| `rsms-codec-smpp` | SMPP 3.4/5.0 协议编解码 |
| `rsms-codec-sgip` | SGIP 1.2 协议编解码 |
| `rsms-longmsg` | 长短信拆分/合包 |
| `rsms-window` | 滑动窗口（请求-响应匹配） |
| `rsms-ratelimit` | 令牌桶限流 |
| `rsms-session` | 连接上下文和状态管理 |
| `rsms-pipeline` | 处理管道 |

## 文档索引

### 使用指南（`guides/`）

| 文档 | 说明 |
|------|------|
| [01-quickstart.md](guides/01-quickstart.md) | 快速开始：核心概念和最小示例 |
| [02-protocols.md](guides/02-protocols.md) | 四协议差异速查表 |
| [03-longmessage.md](guides/03-longmessage.md) | 长短信处理 |
| [04-dynamic-adjustment.md](guides/04-dynamic-adjustment.md) | 动态连接数调整 |
| [05-logging.md](guides/05-logging.md) | 日志配置（remote_ip/remote_port、日志级别控制） |

### 协议专项（`protocols/`）

| 文档 | 说明 |
|------|------|
| [01-cmpp.md](protocols/01-cmpp.md) | CMPP 协议专项（v2.0 / v3.0） |
| [02-smgp.md](protocols/02-smgp.md) | SMGP 协议专项（v3.0.3） |
| [03-smpp.md](protocols/03-smpp.md) | SMPP 协议专项（v3.4 / v5.0） |
| [04-sgip.md](protocols/04-sgip.md) | SGIP 协议专项（v1.2） |

### 参考资料（`reference/`）

| 文档 | 说明 |
|------|------|
| [01-tests.md](reference/01-tests.md) | 测试参考和压测性能数据 |

### 协议规范（`specs/`）

厂商原始协议规范文档（PDF/DOC），仅供查阅：

| 文件 | 说明 |
|------|------|
| `CMPP2.0.doc` | 中国移动 CMPP 2.0 协议规范 |
| `CMPP接口协议V3.0.0.pdf` | 中国移动 CMPP 3.0 协议规范 |
| `【集成文档】中国电信SMGP协议（V3.0.3）.pdf` | 中国电信 SMGP 协议规范 |
| `中国联通SGIP.pdf` | 中国联通 SGIP 协议规范 |

## 快速链接

- 示例代码：`examples/<protocol>-endpoint/src/{server,client}.rs`
- 压测代码：`examples/<protocol>-endpoint/tests/*stress_test.rs`
