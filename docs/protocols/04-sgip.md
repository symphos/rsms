# SGIP 协议专项

中国联通短信协议（Short Message Gateway Interface Protocol）V1.2。

## 协议概览

| 项目 | 值 |
|------|-----|
| 标准组织 | 中国联通 |
| 版本 | V1.2 |
| Header 长度 | **20 字节** |
| 协议标识 | `"sgip"` |
| 默认端口 | 7891 |

## Header 结构

```
Offset  Length  Field
0       4       Total_Length
4       4       Command_Id
8       4       SgipSequence.node_id
12      4       SgipSequence.timestamp
16      4       SgipSequence.number
```

> **关键差异**：SGIP 使用 12 字节的 `SgipSequence`（`node_id + timestamp + number`）而非简单的 u32 sequence_id。编解码时需要传递这三个参数。

```rust
// 编码 PDU（注意 to_pdu_bytes 需要 3 个参数）
pdu.to_pdu_bytes(node_id, timestamp, number)

// 其他协议只需
pdu.to_pdu_bytes(sequence_id)
```

## 认证方式

SGIP 使用**明文密码认证**。

### AuthCredentials::Sgip

```rust
AuthCredentials::Sgip {
    login_name: String,         // 登录名
    login_password: String,     // 明文密码
}
```

### 服务端认证示例

```rust
struct SgipAuth {
    accounts: HashMap<String, String>,
}

#[async_trait]
impl AuthHandler for SgipAuth {
    fn name(&self) -> &'static str { "sgip-auth" }
    async fn authenticate(&self, _: &str, credentials: AuthCredentials) -> Result<AuthResult> {
        match credentials {
            AuthCredentials::Sgip { login_name, login_password } => {
                if let Some(pw) = self.accounts.get(&login_name) {
                    if login_password == *pw {
                        return Ok(AuthResult::success(login_name));
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
use rsms_codec_sgip::{Submit, Encodable};

let mut submit = Submit::new();
submit.sp_number = "106900".to_string();
submit.charge_number = "106900".to_string();
submit.user_count = 1;
submit.user_numbers = vec!["13800138000".to_string()];
submit.corp_id = "".to_string();
submit.service_type = "SMS".to_string();
submit.fee_type = 2;
submit.fee_value = "000000".to_string();
submit.given_value = "000000".to_string();
submit.agent_flag = 0;
submit.morelate_to_mt_flag = 0;
submit.priority = 0;
submit.expire_time = "".to_string();
submit.schedule_time = "".to_string();
submit.report_flag = 1;             // 需要状态报告
submit.tppid = 0;
submit.tpudhi = 0;
submit.msg_fmt = 15;
submit.message_type = 0;
submit.message_content = "Hello".as_bytes().to_vec();
submit.reserve = [0u8; 8];

// 编码（注意 SGIP 需要 node_id + timestamp + number）
let body_bytes = {
    let mut buf = bytes::BytesMut::new();
    submit.encode(&mut buf).unwrap();
    buf.to_vec()
};
let total_len = (20 + body_bytes.len()) as u32;
let mut pdu = Vec::new();
pdu.extend_from_slice(&total_len.to_be_bytes());
pdu.extend_from_slice(&(CommandId::Submit as u32).to_be_bytes());
pdu.extend_from_slice(&node_id.to_be_bytes());
pdu.extend_from_slice(&timestamp.to_be_bytes());
pdu.extend_from_slice(&seq_number.to_be_bytes());
pdu.extend(body_bytes);
```

> **注意**：SGIP Submit 的 `message_content` 长度前缀是 **u32**（4 字节），而 CMPP/SMGP 是 u8（1 字节）。

### 状态报告：独立 Report 命令

**SGIP 与其他协议的最大差异**：状态报告不通过 `Deliver` 承载，而是使用独立的 `Report` 命令。

```rust
use rsms_codec_sgip::Report;

// Report 包含 submit_sequence_number（原始 Submit 的序列号）
// 服务端通过 Submit 的 SgipSequence 匹配 Report
```

### 心跳

SGIP 协议**没有心跳命令**。

### 关闭连接：Unbind

框架在 `Connection::close()` 时自动发送 Unbind 包。

## SgipSequence 常量

在测试和示例中，通常使用固定值：

```rust
const SGIP_NODE_ID: u32 = 1;
const SGIP_TIMESTAMP: u32 = 0x04051200;
```

## Bind 命令

```rust
use rsms_codec_sgip::Bind;

let bind = Bind {
    login_type: 1,                         // 1 = 发送
    login_name: "900001".to_string(),
    login_password: "password".to_string(),
    reserve: [0u8; 8],
};
let pdu_bytes = bind.to_pdu_bytes(node_id, timestamp, seq_number);
```

## 服务端完整示例

参考：`examples/sgip-endpoint/src/server.rs`

```rust
let config = Arc::new(EndpointConfig::new("sgip-gateway", "0.0.0.0", 7891, 500, 60)
    .with_protocol("sgip"));

let server = serve(
    config,
    vec![Arc::new(MyBizHandler)],
    Some(Arc::new(SgipAuth::new())),
    None,
    None, None, None,
).await?;
```

## 客户端完整示例

参考：`examples/sgip-endpoint/src/client.rs`

```rust
let endpoint = Arc::new(EndpointConfig::new("sgip-client", "127.0.0.1", port, 500, 60)
    .with_protocol("sgip"));

let conn = connect(
    endpoint,
    Arc::new(MyClientHandler),
    SgipDecoder,
    Some(ClientConfig::default()),
    None, None,
).await?;

// 发送 Bind
let bind = Bind {
    login_type: 1,
    login_name: "900001".to_string(),
    login_password: "password".to_string(),
    reserve: [0u8; 8],
};
conn.write_frame(bind.to_pdu_bytes(1, 0, seq).as_slice()).await?;
```

## 参考测试

| 测试文件 | 说明 |
|----------|------|
| `examples/sgip-endpoint/tests/mod.rs` | 集成测试（8 个） |
| `examples/sgip-endpoint/tests/stress_test.rs` | 单账号压测 |
| `examples/sgip-endpoint/tests/multi_account_stress_test.rs` | 多账号压测（5×5，300s） |
| `examples/sgip-endpoint/tests/sgip_longmsg_test.rs` | 长短信测试 |
| `examples/sgip-endpoint/tests/dynamic_connection_test.rs` | 动态连接数调整测试 |
