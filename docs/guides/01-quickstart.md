# 快速开始

## 核心概念

### EncodedPdu

所有协议 PDU 的统一抽象，框架内部只操作 `&[u8]` 字节切片。

```rust
// trait 定义
pub trait EncodedPdu: Send + Sync {
    fn as_bytes(&self) -> &[u8];
    fn sequence_id(&self) -> Option<u32>;
    fn command_id(&self) -> Option<u32>;
}

// 两种实现
pub struct RawPdu { ... }      // 原始字节 PDU
pub struct Frame { ... }       // 解析后的帧（command_id + sequence_id + data）
```

- `RawPdu`：协议编码后的原始字节，用于 `write_frame()` 发送
- `Frame`：解码后的帧结构，用于 `on_inbound()` 接收

### 窄腰统一模型（UnifiedMessage + ProtocolAdapter）—— 推荐主路径

业务代码无需直接接触各协议的裸 codec 类型（`Submit`/`Deliver`/`decode_message` 等），
统一用协议无关的 `rsms_model::UnifiedMessage` + 每协议的 `ProtocolAdapter` 收发：

```rust
use rsms_model::{ProtocolAdapter, UnifiedMessage, UnifiedSubmitResp, MessageId};
use rsms_codec_cmpp::adapter::CmppAdapter;   // 各协议：rsms_codec_<proto>::adapter::<Proto>Adapter

// 收包：字节帧 -> 统一消息，按协议无关的枚举分支处理
let unified = CmppAdapter.decode(frame)?;
match unified {
    UnifiedMessage::Submit(s)  => { /* s.src / s.dests / s.content / s.encoding / s.want_report */ }
    UnifiedMessage::Deliver(d) => { /* MO 上行 */ }
    UnifiedMessage::Report(r)  => { /* 状态报告（不分底层是 Deliver 还是独立 Report 命令）*/ }
    UnifiedMessage::Ping       => { /* 心跳 */ }
    _ => {}
}

// 发包/回执：构造统一消息 -> 字节；回复请求用 sequence_of(frame) 回显序列
let resp = UnifiedMessage::SubmitResp(UnifiedSubmitResp {
    msg_id: MessageId::Binary(msg_id_bytes.to_vec()),   // SMPP 用 MessageId::Text(..)
    status: 0,
});
let bytes = CmppAdapter.encode(&resp, CmppAdapter.sequence_of(frame))?;
ctx.conn.write_frame(&bytes).await?;
```

- `ProtocolAdapter` trait 来自 `rsms_model`；四个实现：`CmppAdapter` / `SmgpAdapter` / `SmppAdapter` / `SgipAdapter`
- 协议方言字段（CMPP/SMGP/SGIP 的 fee/service_id 等）经 `UnifiedMessage` 的 `ProtocolExtra` 携带
- 序列号抽象为 `Sequence`：CMPP/SMGP/SMPP 为 `Sequence::Plain(u32)`，SGIP 为复合 `Sequence::Sgip{node_id,timestamp,number}`；回复一律用 `adapter.sequence_of(frame)`
- 也可按协议枚举动态取适配器：`rsms_connector::adapter_registry::adapter_for(Protocol::Cmpp)`
- 四协议的 `examples/` server/client 与 `tests/` 均为该模型的完整参考

### MessageSource

业务方实现此 trait，提供待发送的消息。框架通过 `fetch()` 拉取消息并自动发送。

```rust
#[async_trait]
pub trait MessageSource: Send + Sync {
    async fn fetch(&self, account: &str, batch_size: usize) -> Result<Vec<MessageItem>>;
}

pub enum MessageItem {
    Single(Arc<dyn EncodedPdu>),         // 普通短消息
    Group { items: Vec<Arc<dyn EncodedPdu>> },  // 长短信分段
}
```

- **key**：`account` 参数，必须和 endpoint ID 一致（即认证通过后的账号名）
- **单消息**：`MessageItem::Single(Arc::new(RawPdu::from_vec(bytes)))`
- **长短信**：`MessageItem::Group { items: vec![...] }`，框架保证同组帧走同一连接顺序发出

### AccountPool

按账号隔离连接和配置。每个账号独立管理：
- 连接列表（`AccountConnections`）
- QPS 限流器（令牌桶）
- 配置（`AccountConfig`：`max_connections`、`max_qps`、`window_size` 等）

```rust
// 获取账号连接池
let acc = account_pool.get_or_create("900001").await;

// 动态更新配置
account_pool.update_config("900001", AccountConfig::new()
    .with_max_connections(5)
    .with_max_qps(2500)
).await?;
```

### IdGenerator —— 账号维度的 ID 生成

框架按**账号**生成 `msg_id` 与 `sequence_id`：`AccountConnections` 各持一个独立的
`IdGenerator`（默认 `SimpleIdGenerator`，原子单调自增），账号之间物理隔离、互不串号；
同账号的多条连接**共用同一实例**（保证账号内不重复）。

```rust
pub trait IdGenerator: Send + Sync {
    fn next_msg_id(&self) -> u64;       // 8B MsgId（服务端出站 MO/回执用）
    fn next_sequence_id(&self) -> u32;  // 4B 协议请求序号
}
```

- **服务端**业务侧经 `InboundContext.id_generator`（`Option<Arc<dyn IdGenerator>>`，鉴权后才有值）获取；
- 也可由 `account_connections.id_generator()` 取该账号的实例。

**sequence_id 契约（重要）**：`send_request` 的 sequence_id 直接取自你构造的 PDU 头（框架不代生成）。
请求/响应匹配的滑动窗口按连接隔离，但**交付回调链路的 `TransactionManager` 按账号共享、以 sequence_id
为键**——因此**同账号多连接时 sequence_id 必须账号内唯一**，否则会互相覆盖事务、导致回执/回调错配。
最简做法：所有连接都用该账号共享的生成器取值：

```rust
let seq = account_connections.id_generator().next_sequence_id();
```

> 长短信的 reference 唯一性与入站合包的发送方分桶，另见 [长短信处理 · 拼接正确性与串号防护](03-longmessage.md#拼接正确性与串号防护)。

### BusinessHandler

服务端业务处理器。框架在协议层解析完成后调用 `on_inbound`。

```rust
#[async_trait]
pub trait BusinessHandler: Send + Sync {
    fn name(&self) -> &'static str;
    async fn on_inbound(&self, ctx: &InboundContext, frame: &Frame) -> Result<()>;
}
```

**注意**：框架不会自动发送 SubmitResp / SubmitSmResp，业务方需要自己构造并调用 `ctx.conn.write_frame()` 发送。

### ClientHandler

客户端收到服务端消息时的回调。

```rust
#[async_trait]
pub trait ClientHandler: Send + Sync {
    fn name(&self) -> &'static str;
    async fn on_inbound(&self, ctx: &ClientContext<'_>, frame: &Frame) -> Result<()>;
}
```

### AuthHandler

服务端认证处理器。根据协议不同，`credentials` 参数不同。

```rust
#[async_trait]
pub trait AuthHandler: Send + Sync {
    fn name(&self) -> &'static str;
    async fn authenticate(
        &self,
        client_id: &str,
        credentials: AuthCredentials,
        conn_info: &ConnectionInfo,
    ) -> Result<AuthResult>;
}
```

## 服务端最小示例

```rust
use rsms_connector::{
    ServerBuilder, AuthHandler, AuthCredentials, AuthResult,
    AccountConfig, AccountConfigProvider,
};
use rsms_business::{BusinessHandler, InboundContext};
use rsms_core::{ConnectionInfo, EndpointConfig, Frame, Protocol, Result};
use rsms_codec_cmpp::adapter::CmppAdapter;
use rsms_model::{ProtocolAdapter, UnifiedMessage};

// 1. 认证
struct MyAuth;
#[async_trait]
impl AuthHandler for MyAuth {
    fn name(&self) -> &'static str { "my-auth" }
    async fn authenticate(
        &self,
        _: &str,
        credentials: AuthCredentials,
        _: &ConnectionInfo,
    ) -> Result<AuthResult> {
        match credentials {
            AuthCredentials::Cmpp { source_addr, .. } => {
                // 校验 source_addr 和 authenticator_source
                Ok(AuthResult::success(source_addr))
            }
            _ => Ok(AuthResult::failure(1, "unsupported")),
        }
    }
}

// 2. 业务处理（窄腰统一模型：解码为 UnifiedMessage，按枚举分支处理）
struct MyBiz;
#[async_trait]
impl BusinessHandler for MyBiz {
    fn name(&self) -> &'static str { "my-biz" }
    async fn on_inbound(&self, ctx: &InboundContext, frame: &Frame) -> Result<()> {
        match CmppAdapter.decode(frame)? {
            UnifiedMessage::Submit(_s) => {
                // 框架不会自动回 SubmitResp，业务方自行构造并发送：
                // let resp = UnifiedMessage::SubmitResp(..);
                // let bytes = CmppAdapter.encode(&resp, CmppAdapter.sequence_of(frame))?;
                // ctx.conn.write_frame(&bytes).await?;
            }
            _ => {}
        }
        Ok(())
    }
}

// 3. 启动
#[tokio::main]
async fn main() -> Result<()> {
    let config = Arc::new(EndpointConfig::new("cmpp-gateway", "0.0.0.0", 7890, 500, 60)
        .with_protocol(Protocol::Cmpp));

    let server = ServerBuilder::new(config)
        .handler(Arc::new(MyBiz))
        .auth_handler(Arc::new(MyAuth))
        // .message_source(s)          // MessageSource（可选）
        // .account_config_provider(p) // AccountConfigProvider（可选）
        // .event_handler(e)           // ServerEventHandler（可选）
        // .account_pool_config(c)     // AccountPoolConfig（可选）
        .serve().await?;

    server.run().await
}
```

## 客户端最小示例

```rust
use rsms_connector::{ClientBuilder, CmppDecoder, ClientHandler, ClientConfig};
use rsms_core::{EndpointConfig, Frame, Result};

struct MyClient;
#[async_trait]
impl ClientHandler for MyClient {
    fn name(&self) -> &'static str { "my-client" }
    async fn on_inbound(&self, ctx: &ClientContext<'_>, frame: &Frame) -> Result<()> {
        // 用 CmppAdapter.decode(frame)? 解为 UnifiedMessage 后分支处理
        // （SubmitResp / Deliver / Report / PingResp 等）
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let endpoint = Arc::new(EndpointConfig::new("client", "127.0.0.1", 7890, 500, 60));

    let conn = ClientBuilder::new(endpoint, Arc::new(MyClient), CmppDecoder)
        .client_config(ClientConfig::default())
        // .message_source(s)   // MessageSource（可选）
        // .event_handler(e)    // ClientEventHandler（可选）
        .connect().await?;

    // 发送消息：构造 UnifiedMessage 经适配器编码（CMPP 序列用 Sequence::Plain）
    let msg = UnifiedMessage::Submit(/* UnifiedSubmit { .. } */);
    let pdu_bytes = CmppAdapter.encode(&msg, Sequence::Plain(seq))?;
    conn.write_frame(&pdu_bytes).await?;

    // 或者通过 window 等待响应
    // conn.send_request(pdu_bytes).await?;

    Ok(())
}
```

## 切换协议

只需改 3 处：

```rust
// 1. EndpointConfig 的 protocol（需 use rsms_core::Protocol; 或 use rsms_connector::Protocol;）
.with_protocol(Protocol::Smpp)   // Protocol::Cmpp | Smgp | Smpp | Sgip

// 2. 客户端 Decoder（ClientBuilder 第三参）
SmppDecoder   // CmppDecoder | SmgpDecoder | SmppDecoder | SgipDecoder

// 3. 协议适配器（收发统一走它，不直接碰裸 codec PDU 类型）
use rsms_codec_smpp::adapter::SmppAdapter;   // CmppAdapter | SmgpAdapter | SmppAdapter | SgipAdapter
```

业务/收发代码本身因为只依赖协议无关的 `UnifiedMessage`，**换协议时基本不用动**——
把上面用到的 `<Proto>Adapter` 换成目标协议的即可（编码差异如 UCS2 的 dcs/msg_fmt、
SGIP 复合序列等都已在各 Adapter 内部处理）。`.with_protocol(...)` **必须**正确设置，
否则 SMPP/SGIP 的 16/20 字节头部会按默认 CMPP 的 12 字节错位解析序列号。

## EndpointConfig 配置

```rust
EndpointConfig::new(id, host, port, max_channels, idle_time_sec)
    .with_protocol(Protocol::Cmpp)   // 协议类型（需 use rsms_core::Protocol;）
    .with_window_size(2048)          // 滑动窗口大小
    .with_timeout(Duration::from_secs(30))  // 请求超时
    .with_reconnect_interval(5)      // 客户端重连间隔（秒）
    .with_log_level(tracing::Level::WARN)   // 框架日志级别
```

| 参数 | 说明 | 默认值 |
|------|------|--------|
| `id` | 端点唯一标识 | 必填 |
| `host` | 监听/连接地址 | 必填 |
| `port` | 端口 | 必填 |
| `max_channels` | 最大并发连接数 | 500 |
| `idle_time_sec` | 空闲检测周期（秒） | 60 |
| `protocol` | 协议类型 | `"cmpp"` |
| `window_size` | 滑动窗口大小 | 16 |
| `timeout` | 请求超时 | 5s |
| `reconnect_interval_sec` | 客户端重连间隔（秒） | 5 |
| `log_level` | 框架日志级别 | `None`（继承全局） |

## AccountConfig 配置

```rust
AccountConfig::new()
    .with_max_connections(5)         // 账号最大连接数
    .with_max_qps(2500)              // 账号最大 QPS
    .with_window_size(2048)          // 滑动窗口大小
    .with_window_size_ms(1000)       // 窗口超时毫秒
    .with_fetch_interval(500)        // MessageSource fetch 间隔（毫秒）
    .with_max_fetch_threads(1)       // fetch 并发线程数
```

| 参数 | 说明 | 默认值 |
|------|------|--------|
| `max_connections` | 账号最大连接数 | 1 |
| `max_qps` | 账号最大 QPS | 100 |
| `window_size` | 滑动窗口大小 | 16 |
| `window_size_ms` | 窗口超时毫秒 | 1000 |
| `fetch_interval_ms` | fetch 间隔 | 500 |
| `max_fetch_threads` | fetch 并发线程 | 1 |
| `submit_resp_timeout_secs` | SubmitResp 超时秒 | 30 |

## 完整参考示例

项目提供四协议（CMPP/SMGP/SGIP/SMPP）各一个 server + client 完整示例，包含认证、限流、MessageSource 队列、错误处理的端到端参考实现。

### 运行方式

```bash
# 1. 启动服务端（任选一协议）
cargo run -p cmpp-server-example    # 端口 7890
cargo run -p smgp-server-example    # 端口 8890
cargo run -p sgip-server-example    # 端口 7891
cargo run -p smpp-server-example    # 端口 7893

# 2. 启动客户端（另开终端）
cargo run -p cmpp-client-example
cargo run -p smgp-client-example
cargo run -p sgip-client-example
cargo run -p smpp-client-example
```

### 示例目录

| 协议 | 服务端 | 客户端 | 说明 |
|------|--------|--------|------|
| CMPP | `examples/cmpp_server/` | `examples/cmpp_client/` | MD5 认证，Report 通过 Deliver 承载 |
| SMGP | `examples/smgp_server/` | `examples/smgp_client/` | MD5 认证，Login/LoginResp |
| SGIP | `examples/sgip_server/` | `examples/sgip_client/` | 明文认证，独立 Report 命令 |
| SMPP | `examples/smpp_server/` | `examples/smpp_client/` | 明文认证，Report 通过 DeliverSm(esm_class=0x04) |

每个示例目录包含：
- `src/main.rs` -- 完整源码
- `accounts.conf` -- 服务端账号配置
- `messages.conf` -- 消息数据
- `README.md` -- 详细说明文档
