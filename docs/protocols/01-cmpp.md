# CMPP 协议专项

中国移动短信协议，支持 V2.0 和 V3.0 两个版本。

## 协议概览

| 项目 | 值 |
|------|-----|
| 标准组织 | 中国移动 |
| 版本 | V2.0 / V3.0 |
| Header 长度 | 12 字节 |
| 协议标识 | `"cmpp"` |
| 默认端口 | 7890 |

## Header 结构

```
Offset  Length  Field
0       4       Total_Length（整个 PDU 长度，含 Header）
4       4       Command_Id
8       4       Sequence_Id
```

## 认证方式

CMPP 使用 MD5 摘要认证。认证流程：

1. 客户端发送 `Connect` 命令，携带 `authenticator_source`（MD5 摘要）
2. 服务端验证后返回 `ConnectResp`

### compute_connect_auth

```rust
use rsms_codec_cmpp::auth::compute_connect_auth;

let authenticator = compute_connect_auth(
    source_addr,   // &str，企业代码，如 "900001"
    password,      // &str，密码
    timestamp,     // u32，时间戳 MMDDHHMMSS 格式的数字
);
// 返回 [u8; 16] MD5 摘要
```

### AuthCredentials::Cmpp

```rust
AuthCredentials::Cmpp {
    source_addr: String,              // 企业代码
    authenticator_source: [u8; 16],   // MD5 摘要
    version: u8,                      // 0x20 = V2.0, 0x30 = V3.0
    timestamp: u32,                   // 认证时间戳
}
```

### 服务端认证示例

```rust
struct CmppAuth {
    accounts: HashMap<String, String>,
}

#[async_trait]
impl AuthHandler for CmppAuth {
    fn name(&self) -> &'static str { "cmpp-auth" }
    async fn authenticate(&self, _: &str, credentials: AuthCredentials) -> Result<AuthResult> {
        match credentials {
            AuthCredentials::Cmpp { source_addr, authenticator_source, version, timestamp } => {
                if let Some(pw) = self.accounts.get(&source_addr) {
                    let expected = compute_connect_auth(&source_addr, pw, timestamp);
                    if authenticator_source == expected {
                        return Ok(AuthResult::success(source_addr));
                    }
                }
                Ok(AuthResult::failure(1, "auth failed"))
            }
            _ => Ok(AuthResult::failure(1, "unsupported")),
        }
    }
}
```

## 消息类型

### MT 提交：Submit

```rust
use rsms_codec_cmpp::{Submit, Pdu};

let submit = Submit {
    msg_id: [0u8; 8],
    pk_total: 1,
    pk_number: 1,
    registered_delivery: 1,    // 需要状态报告
    msg_level: 0,
    service_id: "SMS".to_string(),
    fee_user_type: 0,
    fee_terminal_id: "".to_string(),
    fee_terminal_type: 0,
    tppid: 0,
    tpudhi: 0,
    msg_fmt: 15,               // 15 = GBK, 8 = UCS2, 0 = ASCII
    msg_src: "900001".to_string(),
    fee_type: "01".to_string(),
    fee_code: "0".to_string(),
    valid_time: "".to_string(),
    at_time: "".to_string(),
    src_id: "10086".to_string(),
    dest_usr_tl: 1,
    dest_terminal_ids: vec!["13800138000".to_string()],
    dest_terminal_type: 0,
    msg_content: "Hello".as_bytes().to_vec(),
    link_id: "".to_string(),
};
let pdu: Pdu = submit.into();
let bytes = pdu.to_pdu_bytes(sequence_id);
```

### MT 响应：SubmitResp

```rust
// 在 MessageHandler::on_message 中收到 UnifiedMessage::Submit 后，业务方用 ctx.reply 回 SubmitResp
// （框架按请求帧序列编码并写回，业务不再手剥序列/手拼字节）。
ctx.reply(UnifiedMessage::SubmitResp(rsms_model::UnifiedSubmitResp {
    msg_id: MessageId::Binary(msg_id.to_vec()),
    status: 0,
}))
.await?;
```

### MO 上行和状态报告：Deliver

```rust
use rsms_codec_cmpp::{Deliver, Pdu};

// 状态报告（registered_delivery = 1）
let deliver = Deliver {
    msg_id: [0u8; 8],
    dest_id: "10086".to_string(),
    service_id: "SMS".to_string(),
    tppid: 0,
    tpudhi: 0,
    msg_fmt: 15,
    src_terminal_id: "13800138000".to_string(),
    src_terminal_type: 0,
    registered_delivery: 1,    // 标记为状态报告
    msg_content: report_content.as_bytes().to_vec(),
    link_id: "".to_string(),
};
```

### 心跳：ActiveTest

框架自动处理，无需业务方干预。

### 关闭连接：Terminate

框架在 `Connection::close()` 时自动发送 Terminate 包，然后关闭 TCP。

## V2.0 vs V3.0 差异

| 差异点 | V2.0 | V3.0 |
|--------|------|------|
| 版本号 | `0x20` | `0x30` |
| Submit 结构 | `SubmitV20`（无 `link_id`/`dest_terminal_type`/`fee_terminal_type`，有 `reserve: [u8;8]`） | `Submit`（有额外字段） |
| `fee_terminal_id` 最大长度 | 21 | 32 |
| 底层解码原语 | `decode_message_with_version(pdu, Some(0x20))` | `decode_message_with_version(pdu, Some(0x30))` 或 `decode_message(pdu)` |

窄腰模型下，**业务无需手动按版本解码**：框架按连接协商版本（`conn.protocol_version()`）调用 `CmppAdapter::decode_with_version`，业务在 `MessageHandler::on_message` 收到的已是解码完成的 `UnifiedMessage`。上表的 `decode_message_with_version` 仅是框架内部使用的底层原语，等价于：

```rust
// 框架内部按协商版本自动解码（业务侧通常不直接调用）
let version = conn.protocol_version().await.unwrap_or(0x30);
let message = decode_message_with_version(pdu, Some(version))?;
```

## 应答与回执的版本宽度（端到端版本感知）

V2.0 与 V3.0 的应答包/状态报告字段宽度不同，服务端按连接协商版本编码（`CmppVersion::from_wire(version_byte)`，见 `rsms-connector/src/handlers/cmpp.rs`），框架提供独立的 `SubmitRespV20`/`DeliverRespV20`/`ConnectRespV20` 类型：

| PDU | V2.0 | V3.0 |
|-----|------|------|
| ConnectResp / SubmitResp / DeliverResp 的 Result/Status | 1 字节 | 4 字节 |
| 状态报告（Deliver 承载）定长正文 | 60B（Dest_terminal_Id 21B） | 71B（Dest_terminal_Id 32B） |
| DeliverV20 尾部 | 含 `Reserved(8)` | （无） |

> 客户端解码也须版本感知（`decode_with_version(frame, 本连接版本)`），否则按默认 V3.0 解 V2.0 应答会失败。

## CMPP 2.0 / 3.0 对接示例

同一套服务端可同时对接 V2.0 与 V3.0 客户端：版本由客户端 Connect 的 `version` 字节协商，服务端据此对**每条连接**分别按版本编/解码。下面是最小骨架（完整可跑见 `examples/cmpp_server/src/main.rs`、`tests/cmpp/cmpp20_test.rs` 与 `tests/cmpp/stress_test.rs` 的 `cmpp20_*`/`cmpp30_*`）。

### 客户端：以指定版本接入

版本只体现在 `UnifiedBind.version` 一个字节，其余收发代码两版本通用。**唯一要点：入站帧必须按本连接版本解码**。

```rust
use rsms_codec_cmpp::CmppVersion;
use rsms_codec_cmpp::adapter::CmppAdapter;
use rsms_model::{ProtocolAdapter, Sequence, UnifiedBind, UnifiedMessage};

// 1) 连接：把握手版本写进 version 字节（0x20 = V2.0 / 0x30 = V3.0）
let ver: u8 = 0x20; // 改成 0x30 即接入 V3.0
let bind = UnifiedMessage::Bind(UnifiedBind {
    version: ver, // ← 版本只体现在这一个字节
    client_id: account.to_string(),
    authenticator: auth.to_vec(), // 已算好的 16B MD5
    timestamp,
    system_type: None,
    mode: rsms_model::BindMode::default(),
    login_mode: None,
});
conn.write_frame(&CmppAdapter.encode(&bind, Sequence::Plain(seq))?).await?;

// 2) 入站解码按本连接版本：V2.0 的 ConnectResp/SubmitResp/回执字段宽度与 V3.0 不同，
//    用默认（V3.0）解码会令 V2.0 应答解码失败。
let cv = CmppVersion::from_wire(ver).unwrap_or(CmppVersion::V30);
match CmppAdapter.decode_with_version(frame, cv)? {
    UnifiedMessage::BindResp(r) if r.status == 0 => { /* 连接成功 */ }
    UnifiedMessage::SubmitResp(r) => { /* 用 r.msg_id 关联业务 */ }
    UnifiedMessage::Report(r) => { /* 用 r.msg_id 关联回执 */ }
    _ => {}
}
```

### 服务端：按连接版本应答

服务端无需为两版本写两套逻辑——握手时框架已记录协商版本，业务取出后用 `encode_with_version` 编码应答即可。ConnectResp 由框架版本感知自动回，SubmitResp/状态报告由业务回。

```rust
// 取本连接协商版本（未握手为 None，回落 V3.0）
let cv = ctx.conn.protocol_version().await
    .and_then(|b| CmppVersion::from_wire(b).ok())
    .unwrap_or(CmppVersion::V30);

// 回 SubmitResp：按版本编码（V2.0 产 9B body / V3.0 产 12B body）
let resp = UnifiedMessage::SubmitResp(/* UnifiedSubmitResp { msg_id, status } */);
let bytes = CmppAdapter.encode_with_version(&resp, CmppAdapter.sequence_of(frame), cv)?;
ctx.conn.write_frame(&bytes).await?;
```

> 状态报告同理：`encode_with_version` 按版本产 60B(V2.0)/71B(V3.0) 定长回执。不带版本的 `encode` 默认按 V3.0 编码，仅适用于纯 V3.0 链路。

## 服务端完整示例

参考：`examples/cmpp_server/src/main.rs`

```rust
use rsms_core::Protocol;
let config = Arc::new(EndpointConfig::new("cmpp-gateway", "0.0.0.0", 7890, 500, 60)
    .with_protocol(Protocol::Cmpp));

let server = ServerBuilder::new(config)
    .message_handlers(vec![Arc::new(MyBizHandler)])
    .auth_handler(Arc::new(CmppAuth::new()))
    .account_config_provider(Arc::new(MyConfigProvider))
    .serve().await?;

let port = server.local_addr.port();
let account_pool = server.account_pool();
tokio::spawn(async move { let _ = server.run().await; });
```

## 客户端完整示例

参考：`examples/cmpp_client/src/main.rs`

```rust
let endpoint = Arc::new(EndpointConfig::new("cmpp-client", "127.0.0.1", port, 500, 60));

// 第二参为 Arc<dyn MessageHandler>（MyClientHandler 实现 MessageHandler trait 的 on_message）。
let conn = ClientBuilder::new(endpoint, Arc::new(MyClientHandler), CmppDecoder)
    .client_config(ClientConfig::default())
    .connect().await?;

// 发送 Connect
let connect_pdu = build_connect_pdu("900001", "password", 0x30);
conn.write_frame(connect_pdu.as_bytes()).await?;
```

## 参考测试

统一在 `rsms-tests` 包，按 `cargo test -p rsms-tests --test <目标名>` 运行：

| 测试文件 | 目标名 | 说明 |
|----------|--------|------|
| `tests/cmpp/cmpp_test.rs` | `cmpp-integration` | 集成测试 |
| `tests/cmpp/stress_test.rs` | `cmpp-stress-test` | 单账号压测（1连接 + 5连接） |
| `tests/cmpp/multi_account_stress_test.rs` | `cmpp-multi-account-stress-test` | 多账号压测（5×5，300s） |
| `tests/cmpp/cmpp_longmsg_test.rs` | `cmpp-longmsg-test` | 长短信测试（V2.0 + V3.0） |
| `tests/cmpp/dynamic_connection_test.rs` | `cmpp-dynamic-connection-test` | 动态连接数调整测试 |
