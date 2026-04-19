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
// 在 BusinessHandler::on_inbound 中收到 Submit 后，业务方构造 SubmitResp
let resp_bytes = build_submit_resp(sequence_id, msg_id, result);
ctx.conn.write_frame(&resp_bytes).await?;
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
| 解码入口 | `decode_message_with_version(pdu, Some(0x20))` | `decode_message_with_version(pdu, Some(0x30))` 或 `decode_message(pdu)` |

```rust
// 服务端根据客户端连接时的 version 字段选择解码路径
let version = conn.protocol_version().await.unwrap_or(0x30);
let message = decode_message_with_version(pdu, Some(version))?;
```

## 服务端完整示例

参考：`examples/cmpp-endpoint/src/server.rs`

```rust
let config = Arc::new(EndpointConfig::new("cmpp-gateway", "0.0.0.0", 7890, 500, 60)
    .with_protocol("cmpp"));

let server = serve(
    config,
    vec![Arc::new(MyBizHandler)],
    Some(Arc::new(CmppAuth::new())),
    None,
    Some(Arc::new(MyConfigProvider)),
    None,
    None,
).await?;

let port = server.local_addr.port();
let account_pool = server.account_pool();
tokio::spawn(async move { let _ = server.run().await; });
```

## 客户端完整示例

参考：`examples/cmpp-endpoint/src/client.rs`

```rust
let endpoint = Arc::new(EndpointConfig::new("cmpp-client", "127.0.0.1", port, 500, 60));

let conn = connect(
    endpoint,
    Arc::new(MyClientHandler),
    CmppDecoder,
    Some(ClientConfig::default()),
    None,
    None,
).await?;

// 发送 Connect
let connect_pdu = build_connect_pdu("900001", "password", 0x30);
conn.write_frame(connect_pdu.as_bytes()).await?;
```

## 参考测试

| 测试文件 | 说明 |
|----------|------|
| `examples/cmpp-endpoint/tests/mod.rs` | 集成测试（37 个） |
| `examples/cmpp-endpoint/tests/stress_test.rs` | 单账号压测（1连接 + 5连接） |
| `examples/cmpp-endpoint/tests/multi_account_stress_test.rs` | 多账号压测（5×5，300s） |
| `examples/cmpp-endpoint/tests/cmpp_longmsg_test.rs` | 长短信测试（V2.0 + V3.0） |
| `examples/cmpp-endpoint/tests/dynamic_connection_test.rs` | 动态连接数调整测试 |
