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
    async fn authenticate(&self, client_id: &str, credentials: AuthCredentials) -> Result<AuthResult>;
}
```

## 服务端最小示例

```rust
use rsms_connector::{
    serve, AuthHandler, AuthCredentials, AuthResult,
    AccountConfig, AccountConfigProvider,
};
use rsms_business::BusinessHandler;
use rsms_core::{EndpointConfig, Frame, Result};

// 1. 认证
struct MyAuth;
#[async_trait]
impl AuthHandler for MyAuth {
    fn name(&self) -> &'static str { "my-auth" }
    async fn authenticate(&self, _: &str, credentials: AuthCredentials) -> Result<AuthResult> {
        match credentials {
            AuthCredentials::Cmpp { source_addr, .. } => {
                // 校验 source_addr 和 authenticator_source
                Ok(AuthResult::success(source_addr))
            }
            _ => Ok(AuthResult::failure(1, "unsupported")),
        }
    }
}

// 2. 业务处理
struct MyBiz;
#[async_trait]
impl BusinessHandler for MyBiz {
    fn name(&self) -> &'static str { "my-biz" }
    async fn on_inbound(&self, ctx: &InboundContext, frame: &Frame) -> Result<()> {
        // frame.command_id 区分消息类型
        // frame.data_as_slice() 获取原始 PDU 字节
        // ctx.conn.write_frame() 发送响应
        Ok(())
    }
}

// 3. 启动
#[tokio::main]
async fn main() -> Result<()> {
    let config = Arc::new(EndpointConfig::new("cmpp-gateway", "0.0.0.0", 7890, 500, 60)
        .with_protocol("cmpp"));

    let server = serve(
        config,
        vec![Arc::new(MyBiz)],
        Some(Arc::new(MyAuth)),
        None,   // MessageSource（可选）
        None,   // AccountConfigProvider（可选）
        None,   // ServerEventHandler（可选）
        None,   // AccountPoolConfig（可选）
    ).await?;

    server.run().await
}
```

## 客户端最小示例

```rust
use rsms_connector::{connect, CmppDecoder, ClientHandler, ClientConfig};
use rsms_core::{EndpointConfig, Frame, Result};

struct MyClient;
#[async_trait]
impl ClientHandler for MyClient {
    fn name(&self) -> &'static str { "my-client" }
    async fn on_inbound(&self, ctx: &ClientContext<'_>, frame: &Frame) -> Result<()> {
        // 处理服务端发来的 SubmitResp / Deliver 等
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
        Some(ClientConfig::default()),
        None,   // MessageSource（可选）
        None,   // ClientEventHandler（可选）
    ).await?;

    // 发送消息
    let pdu_bytes = build_some_pdu();
    conn.write_frame(&pdu_bytes).await?;

    // 或者通过 window 等待响应
    // conn.send_request(pdu_bytes).await?;

    Ok(())
}
```

## 切换协议

只需改 3 处：

```rust
// 1. EndpointConfig 的 protocol
.with_protocol("smpp")   // "cmpp" | "smgp" | "smpp" | "sgip"

// 2. Decoder
SmppDecoder   // CmppDecoder | SmgpDecoder | SmppDecoder | SgipDecoder

// 3. Codec 的 PDU 类型
use rsms_codec_smpp::{BindTransmitter, SubmitSm, ...};  // 替换为对应协议的 codec
```

## EndpointConfig 配置

```rust
EndpointConfig::new(id, host, port, max_channels, idle_time_sec)
    .with_protocol("cmpp")           // 协议类型
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
