# RSMS

Rust 多协议短信消息中间件框架，支持 **CMPP 2.0/3.0**、**SMGP 3.0.3**、**SMPP 3.4/5.0**、**SGIP 1.2** 四种运营商短信协议。

> ⚠️ **WIP / 早期阶段**：当前版本 `0.0.1`，尚未发布。四协议收发、300s 压测零丢失、连接/资源长稳已验证，适合**受控试点**；作为关键生产链路前建议补充真实运营商联调与天级长稳（见 [`docs/OPTIMIZATION_PLAN.md`](docs/OPTIMIZATION_PLAN.md)）。
>
> 「**统一消息模型 / 协议窄腰**」架构已落地为**推荐主路径**：业务经 `rsms-model` 的 `UnifiedMessage` + `ProtocolAdapter`（`CmppAdapter`/`SmgpAdapter`/`SmppAdapter`/`SgipAdapter`）收发，不再直接接触各协议裸 codec；四协议 `examples/` 与 `tests/` 均已按此实现（设计见 [`docs/superpowers/specs/`](docs/superpowers/specs/)）。底层 `rsms-codec-*` 仍可直接使用。

## 特性

- **四协议统一抽象（窄腰模型）** — 业务只依赖协议无关的 `UnifiedMessage` + `ProtocolAdapter` 收发，切换协议基本只改 `EndpointConfig.protocol`、客户端 Decoder 和所用 Adapter
- **高性能** — 单账号 TPS 2500+，5 账号并发 TPS 12500+，300 秒压测消息零丢失
- **结构化日志** — 所有连接日志自动携带 `remote_ip`/`remote_port`，支持按端点配置日志级别
- **动态调整** — 运行时动态调整连接数上限和 QPS，自动剔除多余连接（发送协议层 Close Packet）
- **长短信** — 内置长短信拆分/合包（`rsms-longmsg`），支持 8-bit 和 16-bit UDH
- **账号隔离** — `AccountPool` 按账号独立管理连接、限流、配置；`msg_id`/`sequence_id` 经**每账号独立的 `IdGenerator`** 生成，账号间不串号
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
use rsms_connector::{ServerBuilder, AuthHandler, AuthCredentials, AuthResult};
use rsms_business::{MessageContext, MessageHandler};
use rsms_core::{ConnectionInfo, EndpointConfig, Protocol, Result};
use rsms_model::{UnifiedMessage, UnifiedSubmitResp, MessageId};

struct MyAuth;
#[async_trait]
impl AuthHandler for MyAuth {
    fn name(&self) -> &'static str { "my-auth" }
    async fn authenticate(&self, _: &str, credentials: AuthCredentials, _: &ConnectionInfo) -> Result<AuthResult> {
        // 验证客户端认证
        Ok(AuthResult::success("account"))
    }
}

struct MyBiz;
#[async_trait]
impl MessageHandler for MyBiz {
    fn name(&self) -> &'static str { "my-biz" }
    async fn on_message(&self, ctx: &MessageContext, msg: &UnifiedMessage) -> Result<()> {
        match msg {
            UnifiedMessage::Submit(_s) => {
                // ctx.reply 自动按请求序列回包（窄腰，无需手剥序列/手拼字节）
                ctx.reply(UnifiedMessage::SubmitResp(UnifiedSubmitResp {
                    msg_id: MessageId::Binary(vec![0u8; 8]),
                    status: 0,
                })).await?;
            }
            _ => {}
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = Arc::new(EndpointConfig::new("gateway", "0.0.0.0", 7890, 500, 60)
        .with_protocol(Protocol::Cmpp)
        .with_log_level(tracing::Level::WARN));

    let server = ServerBuilder::new(config)
        .message_handler(Arc::new(MyBiz))
        .auth_handler(Arc::new(MyAuth))
        .serve()
        .await?;

    server.run().await
}
```

### 客户端示例

```rust
use rsms_connector::{ClientBuilder, CmppDecoder};
use rsms_business::{MessageContext, MessageHandler};
use rsms_core::{EndpointConfig, Result};
use rsms_codec_cmpp::adapter::CmppAdapter;
use rsms_model::{ProtocolAdapter, UnifiedMessage};

struct MyClient;
#[async_trait]
impl MessageHandler for MyClient {
    fn name(&self) -> &'static str { "my-client" }
    async fn on_message(&self, ctx: &MessageContext, msg: &UnifiedMessage) -> Result<()> {
        match msg {
            UnifiedMessage::SubmitResp(_r) => { /* 处理回执 */ }
            UnifiedMessage::Report(_) => {
                ctx.reply(UnifiedMessage::DeliverResp).await?;
            }
            _ => {}
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let endpoint = Arc::new(EndpointConfig::new("client", "127.0.0.1", 7890, 500, 60));
    let handler = Arc::new(MyClient);

    let conn = ClientBuilder::new(endpoint, handler, CmppDecoder)
        .connect()
        .await?;

    // 构造 UnifiedMessage 经适配器编码后发送（CMPP 序列用 Sequence::Plain）
    let msg = UnifiedMessage::Submit(/* UnifiedSubmit { .. } */);
    let pdu_bytes = CmppAdapter.encode(&msg, rsms_model::Sequence::Plain(seq))?;
    conn.send_request(pdu_bytes).await?;
    Ok(())
}
```

### 切换协议

只需改 3 处：

```rust
// 1. protocol（需 use rsms_core::Protocol; 或 use rsms_connector::Protocol;）
.with_protocol(Protocol::Smpp)    // Protocol::Cmpp | Smgp | Smpp | Sgip

// 2. 客户端 Decoder（ClientBuilder 第三参）
SmppDecoder                // CmppDecoder | SmgpDecoder | SmppDecoder | SgipDecoder

// 3. 协议适配器（收发统一走它，不直接碰裸 codec PDU 类型）
use rsms_codec_smpp::adapter::SmppAdapter;   // CmppAdapter | SmgpAdapter | SmppAdapter | SgipAdapter
```

业务/收发代码只依赖协议无关的 `UnifiedMessage`，换协议时基本不动——把 `<Proto>Adapter` 换成目标协议即可。
也可按枚举动态取适配器：`rsms_connector::adapter_registry::adapter_for(Protocol::Smpp)`。

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
│  rsms-model（窄腰）：UnifiedMessage ↔ 字节，经 *Adapter 适配各协议   │
│  rsms-codec-cmpp / smgp / smpp / sgip（适配器底层编解码）            │
│           (Header 解析，PDU 序列化)                            │
└───────────────────────────┬──────────────────────────────────┘
                            │
                        TCP Socket
```

> 业务代码经 `rsms-model` 的 `ProtocolAdapter` 在 `UnifiedMessage` 与字节间互译，`rsms-codec-*` 退居适配器底层。

## Crate 结构

| Crate | 说明 |
|-------|------|
| `rsms-core` | 核心类型：`Frame`、`RawPdu`、`EncodedPdu` trait、`EndpointConfig`、`Protocol` |
| `rsms-model` | 窄腰统一模型：`UnifiedMessage`、`ProtocolAdapter` trait、`Sequence`、`Address`/`MessageId`/`Encoding` |
| `rsms-connector` | 连接管理：服务端 `serve()`、客户端 `connect()`、`AccountPool`、`MessageSource`、`adapter_registry` |
| `rsms-business` | 业务处理器：`MessageHandler` trait、`MessageContext` |
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

# 集成测试（四协议，均在 rsms-tests 包中）
cargo test -p rsms-tests --test cmpp-integration
cargo test -p rsms-tests --test smgp-integration
cargo test -p rsms-tests --test smpp-integration
cargo test -p rsms-tests --test sgip-integration

# 压测（EndpointConfig 已配置 log_level=WARN）
cargo test -p rsms-tests --test cmpp-stress-test -- --nocapture
cargo test -p rsms-tests --test cmpp-multi-account-stress-test -- --nocapture

# 长短信测试
cargo test -p rsms-tests --test cmpp-longmsg-test -- --nocapture

# 动态连接调整测试
cargo test -p rsms-tests --test cmpp-dynamic-connection-test -- --nocapture
```

## 鸣谢

感谢 [**lihuanghe/SMSGate**](https://github.com/Lihuanghe/SMSGate)（Java 多协议短信网关）—— 开发期间以它作为联调对端，帮助验证了 rsms 的协议线路兼容性。

## License

Apache-2.0
