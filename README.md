# RSMS

Rust 多协议短信消息中间件框架，支持 **CMPP 2.0/3.0**、**SMGP 3.0.3**、**SMPP 3.4/5.0**、**SGIP 1.2** 四种运营商短信协议。

## 特性

- **四协议统一抽象** — 服务端/客户端 API 统一，切换协议只需改 Decoder 和 EndpointConfig
- **高性能** — 单账号 TPS 2500+，5 账号并发 TPS 12500+，300 秒压测消息零丢失
- **结构化日志** — 所有连接日志自动携带 `remote_ip`/`remote_port`，支持按端点配置日志级别
- **动态调整** — 运行时动态调整连接数上限和 QPS，自动剔除多余连接（发送协议层 Close Packet）
- **长短信** — 内置长短信拆分/合包（`rsms-longmsg`），支持 8-bit 和 16-bit UDH
- **账号隔离** — `AccountPool` 按账号独立管理连接、限流、配置
- **消息队列** — `MessageSource` trait 按账号隔离，业务方自己管理内存队列

## 快速开始

### 环境要求

- Rust 1.85+（edition 2024）
- Tokio 运行时

### 添加依赖

```toml
[dependencies]
rsms-connector = { path = "crates/rsms-connector" }
rsms-codec-cmpp = { path = "crates/rsms-codec-cmpp" }
# 或 rsms-codec-smgp / rsms-codec-smpp / rsms-codec-sgip
```

### 服务端示例

```rust
use rsms_connector::{serve, AuthHandler, AuthCredentials, AuthResult};
use rsms_business::BusinessHandler;
use rsms_core::{EndpointConfig, Frame, Result};

struct MyAuth;
#[async_trait]
impl AuthHandler for MyAuth {
    fn name(&self) -> &'static str { "my-auth" }
    async fn authenticate(&self, _: &str, credentials: AuthCredentials) -> Result<AuthResult> {
        // 验证客户端认证
        Ok(AuthResult::success("account"))
    }
}

struct MyBiz;
#[async_trait]
impl BusinessHandler for MyBiz {
    fn name(&self) -> &'static str { "my-biz" }
    async fn on_inbound(&self, ctx: &InboundContext, frame: &Frame) -> Result<()> {
        // 处理收到的 Submit/Deliver 等 PDU
        // ctx.conn.write_frame() 发送响应
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = Arc::new(EndpointConfig::new("gateway", "0.0.0.0", 7890, 500, 60)
        .with_protocol("cmpp")
        .with_log_level(tracing::Level::WARN));

    let server = serve(
        config,
        vec![Arc::new(MyBiz)],
        Some(Arc::new(MyAuth)),
        None, None, None, None,
    ).await?;

    server.run().await
}
```

### 客户端示例

```rust
use rsms_connector::{connect, CmppDecoder, ClientHandler};
use rsms_core::{EndpointConfig, Frame, Result};

struct MyClient;
#[async_trait]
impl ClientHandler for MyClient {
    fn name(&self) -> &'static str { "my-client" }
    async fn on_inbound(&self, ctx: &ClientContext<'_>, frame: &Frame) -> Result<()> {
        // 处理 SubmitResp / Deliver 等
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let endpoint = Arc::new(EndpointConfig::new("client", "127.0.0.1", 7890, 500, 60));

    let conn = connect(
        endpoint,
        Arc::new(MyClient),
        CmppDecoder,
        None, None, None,
    ).await?;

    conn.write_frame(&pdu_bytes).await?;
    Ok(())
}
```

### 切换协议

只需改 3 处：

```rust
// 1. protocol
.with_protocol("smpp")    // "cmpp" | "smgp" | "smpp" | "sgip"

// 2. Decoder
SmppDecoder                // CmppDecoder | SmgpDecoder | SmppDecoder | SgipDecoder

// 3. Codec 类型
use rsms_codec_smpp::{BindTransmitter, SubmitSm, ...};
```

## 架构

```
┌──────────────────────────────────────────────────────────────┐
│                        使用方（业务代码）                       │
│  ┌──────────────┐  ┌──────────────┐  ┌────────────────────┐ │
│  │ AuthHandler   │  │ Business     │  │ MessageSource      │ │
│  │ (认证逻辑)    │  │ Handler      │  │ (待发消息队列)      │ │
│  │              │  │ (业务处理)    │  │ fetch(account,batch)│ │
│  └──────────────┘  └──────────────┘  └────────────────────┘ │
└───────────────────────────┬──────────────────────────────────┘
                            │
┌───────────────────────────┼──────────────────────────────────┐
│                     rsms-connector                           │
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
| `rsms-business` | 业务处理器：`BusinessHandler` trait |
| `rsms-codec-cmpp` | CMPP 2.0/3.0 协议编解码 |
| `rsms-codec-smgp` | SMGP 3.0.3 协议编解码 |
| `rsms-codec-smpp` | SMPP 3.4/5.0 协议编解码 |
| `rsms-codec-sgip` | SGIP 1.2 协议编解码 |
| `rsms-longmsg` | 长短信拆分/合包 |
| `rsms-window` | 滑动窗口（请求-响应匹配） |
| `rsms-ratelimit` | 令牌桶限流 |
| `rsms-session` | 连接上下文和状态管理 |
| `rsms-pipeline` | 处理管道 |

## 四协议对比

| 特性 | CMPP | SMGP | SMPP | SGIP |
|------|------|------|------|------|
| 标准组织 | 中国移动 | 中国电信 | SMPP.org | 中国联通 |
| 版本 | 2.0 / 3.0 | 3.0.3 | 3.4 / 5.0 | 1.2 |
| Header 长度 | 12B | 12B | 16B | 20B |
| 认证方式 | MD5 | MD5 | 明文 | 明文 |
| 状态报告 | Deliver 承载 | Deliver 承载 | DeliverSm 承载 | 独立 Report |
| 心跳 | ActiveTest | ActiveTest | EnquireLink | 无 |
| MsgId | 8B 二进制 | 10B 自定义 | C-string | SgipSequence |

## 压测性能

所有协议均通过单账号和多账号压测，300 秒零丢失：

| 场景 | CMPP | SMGP | SMPP | SGIP |
|------|------|------|------|------|
| 单账号 × 1 连接（30s） | 2,500 | 2,500 | 2,500 | 2,500 |
| 单账号 × 5 连接（30s） | 2,500 | 2,567 | 2,500 | 2,500 |
| 5 账号 × 25 连接（300s） | 12,500+ | 12,500+ | 12,500+ | 12,500+ |

## 文档

完整文档见 [docs/](docs/) 目录：

- [快速开始](docs/guides/01-quickstart.md) — 核心概念、服务端/客户端最小示例、配置参考
- [四协议差异速查](docs/guides/02-protocols.md) — Header、认证、消息类型、MsgId 格式对比
- [CMPP 专项](docs/protocols/01-cmpp.md) — V2.0/V3.0、MD5 认证、Submit/Deliver
- [SMGP 专项](docs/protocols/02-smgp.md) — TLV 扩展、Login 认证
- [SMPP 专项](docs/protocols/03-smpp.md) — 16B Header、明文认证、V3.4/V5.0
- [SGIP 专项](docs/protocols/04-sgip.md) — 20B Header、SgipSequence、独立 Report
- [长短信处理](docs/guides/03-longmessage.md) — UDH 格式、拆分/合包
- [动态连接数调整](docs/guides/04-dynamic-adjustment.md) — 运行时调整 max_connections/QPS
- [日志配置](docs/guides/05-logging.md) — remote_ip/remote_port、日志级别控制
- [测试参考](docs/reference/01-tests.md) — 测试分类、运行命令、压测数据

协议规范原文见 [docs/specs/](docs/specs/)。

## 运行测试

```bash
# 单元测试
cargo test -p rsms-core -p rsms-connector -p rsms-longmsg

# 集成测试（四协议）
cargo test -p cmpp-endpoint-example --test cmpp-integration-tests
cargo test -p smgp-endpoint-example --test smgp-integration-tests
cargo test -p smpp-endpoint-example --test smpp-integration-tests
cargo test -p sgip-endpoint-example --test sgip-integration-tests

# 压测（EndpointConfig 已配置 log_level=WARN）
cargo test -p cmpp-endpoint-example --test cmpp-stress-test -- --nocapture
cargo test -p cmpp-endpoint-example --test multi-account-stress-test -- --nocapture

# 长短信测试
cargo test -p cmpp-endpoint-example --test cmpp-longmsg-test -- --nocapture

# 动态连接调整测试
cargo test -p cmpp-endpoint-example --test dynamic-connection-test -- --nocapture
```

## License

Apache-2.0
