# SMPP 协议专项

通用短信协议（Short Message Peer-to-Peer），支持 V3.4 和 V5.0 两个版本。

## 协议概览

| 项目 | 值 |
|------|-----|
| 标准组织 | SMPP.org |
| 版本 | V3.4 / V5.0 |
| Header 长度 | **16 字节**（比 CMPP/SMGP 多 4 字节） |
| 协议标识 | `"smpp"` |
| 默认端口 | 7893 |

## Header 结构

```
Offset  Length  Field
0       4       Command_Length（整个 PDU 长度）
4       4       Command_Id
8       4       Command_Status（SMPP 特有）
12      4       Sequence_Number
```

> **关键差异**：SMPP Header 多了 4 字节 `Command_Status`，因此 sequence_id 在 bytes 12-15（而非 CMPP/SMGP 的 bytes 8-11）。客户端 `EndpointConfig` 必须加 `.with_protocol(Protocol::Smpp)`，否则 sequence_id 提取位置错误。

## 认证方式

SMPP 使用**明文密码认证**（非 MD5），通过 `BindTransmitter` 命令。

### BindTransmitter

```rust
use rsms_codec_smpp::{BindTransmitter, Pdu};

let bind = BindTransmitter::new(
    system_id,       // &str，如 "SMPP"
    password,        // &str，明文密码
    addr_ton,        // &str，如 "CMT"
    interface_version, // u8，0x34 = V3.4, 0x50 = V5.0
);
let pdu: Pdu = bind.into();
let bytes = pdu.to_pdu_bytes(sequence_number);
```

### AuthCredentials::Smpp

```rust
AuthCredentials::Smpp {
    system_id: String,            // 系统 ID
    password: String,             // 明文密码
    interface_version: u8,        // 0x34 或 0x50
}
```

### 服务端认证示例

```rust
struct SmppAuth {
    accounts: HashMap<String, String>,
}

#[async_trait]
impl AuthHandler for SmppAuth {
    fn name(&self) -> &'static str { "smpp-auth" }
    async fn authenticate(&self, _: &str, credentials: AuthCredentials) -> Result<AuthResult> {
        match credentials {
            AuthCredentials::Smpp { system_id, password, interface_version: _ } => {
                if let Some(pw) = self.accounts.get(&system_id) {
                    if password == *pw {
                        return Ok(AuthResult::success(system_id));
                    }
                }
                Ok(AuthResult::failure(1, "auth failed"))
            }
            _ => Ok(AuthResult::failure(1, "unsupported")),
        }
    }
}
```

### V3.4 vs V5.0 字段长度差异

| 字段 | V3.4 最大长度 | V5.0 最大长度 |
|------|-------------|-------------|
| `system_id` | 16 | 16 |
| `password` | **9** | **65** |
| `source_addr` | **21** | **65** |
| `destination_addr` | **21** | **65** |

> **注意**：V3.4 密码最大 8 字节（C-string 含末尾 `\0` 为 9），超过会编码失败。

## 消息类型

### MT 提交：SubmitSm

```rust
use rsms_codec_smpp::{SubmitSm, Pdu};

let mut submit = SubmitSm::new();
submit.source_addr = "10086".to_string();
submit.dest_addr_ton = 1;
submit.dest_addr_npi = 1;
submit.destination_addr = "13800138000".to_string();
submit.esm_class = 0;
submit.registered_delivery = 1;    // 需要状态报告
submit.data_coding = 0x03;         // 0x03 = Latin1, 0x08 = UCS2
submit.short_message = "Hello".as_bytes().to_vec();

let pdu: Pdu = submit.into();
let bytes = pdu.to_pdu_bytes(sequence_number);
```

### MT 响应：SubmitSmResp

```rust
use rsms_codec_smpp::SubmitSmResp;

let resp = SubmitSmResp {
    command_status: 0,
    message_id: "msg123".to_string(),   // SMPP 的 message_id 是 String（C-string）
};
```

> **注意**：SMPP 的 `message_id` 是 **String 类型**（C-string），不是 CMPP 的 `[u8; 8]` 或 SMGP 的 `SmgpMsgId`。

### 状态报告：DeliverSm(esm_class=0x04)

SMPP 的状态报告通过 `DeliverSm` 承载，通过 `esm_class & 0x04` 区分 MO 上行和状态报告。

### 心跳：EnquireLink

框架自动处理。服务端收到 EnquireLink 返回 EnquireLinkResp。

### 关闭连接：Unbind

框架在 `Connection::close()` 时自动发送 Unbind 包。

## 服务端版本检测

SMPP 版本差异（V3.4/V5.0）仅是字段长度限制，统一模型由 `SmppAdapter` 内部封装：框架将收到的 PDU 解码为协议无关的 `UnifiedMessage` 交给 `MessageHandler::on_message()`，业务无需显式判断版本或调用裸 codec 解码。如需在客户端限定版本，可在 `EndpointConfig` 侧通过配置层声明。

## 服务端完整示例

参考：`examples/smpp_server/src/main.rs`

```rust
use rsms_core::Protocol;
let config = Arc::new(EndpointConfig::new("smpp-gateway", "0.0.0.0", 7893, 500, 60)
    .with_protocol(Protocol::Smpp));   // 必须设置！

// MyBizHandler 实现 MessageHandler trait（on_message 按 UnifiedMessage 分支，用 ctx.reply 回执）
let server = ServerBuilder::new(config)
    .message_handlers(vec![Arc::new(MyBizHandler)])
    .auth_handler(Arc::new(SmppAuth::new()))
    .serve().await?;
```

## 客户端完整示例

参考：`examples/smpp_client/src/main.rs`

```rust
let endpoint = Arc::new(EndpointConfig::new("smpp-client", "127.0.0.1", port, 500, 60)
    .with_protocol(Protocol::Smpp));   // 必须设置！

// MyClientHandler 实现 MessageHandler trait（ClientBuilder 第二参为 Arc<dyn MessageHandler>，
// on_message 按 UnifiedMessage 分支，用 ctx.reply 回 DeliverResp）
let conn = ClientBuilder::new(endpoint, Arc::new(MyClientHandler), SmppDecoder)
    .connect().await?;

// 发送 Bind：SMPP DUPLEX 端点须 BindTransceiver，构造统一 Bind 经 SmppAdapter 编码后发送
let bind = UnifiedMessage::Bind(UnifiedBind {
    client_id: "SMPP".to_string(),
    authenticator: "pwd12345".as_bytes().to_vec(), // SMPP 明文口令（非 MD5）
    timestamp: 0,
    version: 0x34,
    system_type: Some("CMT".to_string()),
    mode: BindMode::Transceiver,
    login_mode: None,
});
let bind_bytes = SmppAdapter.encode(&bind, Sequence::Plain(1))?;
conn.send_request(bind_bytes).await?;
```

## 参考测试

| 测试文件 | 说明 |
|----------|------|
| `tests/smpp/integration.rs`（`smpp-integration`） | 集成测试（9 个） |
| `tests/smpp/stress_test.rs`（`smpp-stress-test`） | 单账号压测（V3.4 + V5.0） |
| `tests/smpp/multi_account_stress_test.rs`（`smpp-multi-account-stress-test`） | 多账号压测（5×5，300s） |
| `tests/smpp/smpp_longmsg_test.rs`（`smpp-longmsg-test`） | 长短信测试（V3.4 + V5.0） |
| `tests/smpp/dynamic_connection_test.rs`（`smpp-dynamic-connection-test`） | 动态连接数调整测试 |
