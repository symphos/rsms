# SMGP 协议专项

中国电信短信协议（Short Message Gateway Protocol）v3.0.3。

## 协议概览

| 项目 | 值 |
|------|-----|
| 标准组织 | 中国电信 |
| 版本 | V3.0.3 |
| Header 长度 | 12 字节 |
| 协议标识 | `"smgp"` |
| 默认端口 | 7892 |

## Header 结构

```
Offset  Length  Field
0       4       Total_Length
4       4       Command_Id
8       4       Sequence_Id
```

## 认证方式

SMGP 使用 MD5 摘要认证，类似 CMPP。

### compute_login_auth

```rust
use rsms_codec_smgp::compute_login_auth;

let authenticator = compute_login_auth(
    client_id,   // &str，如 "106900"
    password,    // &str
    timestamp,   // u32，通常传 0
);
// 返回 [u8; 16] MD5 摘要
```

### AuthCredentials::Smgp

```rust
AuthCredentials::Smgp {
    client_id: String,          // 客户端 ID
    authenticator: [u8; 16],    // MD5 摘要
    version: u8,                // 通常 0x30
}
```

### 服务端认证示例

```rust
struct SmgpAuth {
    accounts: HashMap<String, String>,
}

#[async_trait]
impl AuthHandler for SmgpAuth {
    fn name(&self) -> &'static str { "smgp-auth" }
    async fn authenticate(&self, _: &str, credentials: AuthCredentials) -> Result<AuthResult> {
        match credentials {
            AuthCredentials::Smgp { client_id, authenticator, version: _ } => {
                if let Some(pw) = self.accounts.get(&client_id) {
                    let expected = compute_login_auth(client_id.as_str(), pw.as_str(), 0);
                    if authenticator == expected {
                        return Ok(AuthResult::success(client_id));
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
use rsms_codec_smgp::{Submit, Pdu};
use rsms_codec_smgp::datatypes::tlv::OptionalParameters;

let submit = Submit {
    msg_type: 6,                    // 6 = 普通短信
    need_report: 1,                 // 需要状态报告
    priority: 0,
    service_id: "SMS".to_string(),
    fee_type: "01".to_string(),
    fee_code: "0".to_string(),
    fixed_fee: "0".to_string(),
    msg_fmt: 15,                    // 15 = GBK
    valid_time: "".to_string(),
    at_time: "".to_string(),
    src_term_id: "106900".to_string(),
    charge_term_id: "".to_string(),
    dest_term_id_count: 1,
    dest_term_ids: vec!["13800138000".to_string()],
    msg_content: "Hello".as_bytes().to_vec(),
    reserve: [0u8; 8],
    optional_params: OptionalParameters::new(),
};
let pdu: Pdu = submit.into();
let bytes = pdu.to_pdu_bytes(sequence_id);
```

> **注意**：SMGP Submit 使用 `msg_type` 和 `need_report` 而非 `registered_delivery`。

### TLV 扩展字段

SMGP 使用 TLV（Type-Length-Value）承载扩展字段：

```rust
use rsms_codec_smgp::datatypes::tlv::OptionalParameters;

let mut params = OptionalParameters::new();
// TP_UDHI - UDH 标志（长短信）
params.set_tp_udhi(1);
// PK_TOTAL / PK_NUMBER - 分段总数/序号
params.set_pk_total(3);
params.set_pk_number(1);
```

| TLV Tag | 名称 | 用途 |
|---------|------|------|
| 0x0001 | `PK_TOTAL` | 长短信分段总数 |
| 0x0002 | `TP_UDHI` | UDH 标志 |
| 0x0003 | `PK_NUMBER` | 长短信分段序号 |

### 状态报告：Deliver(is_report=1)

SMGP 的状态报告通过 `Deliver` 命令承载，通过 `is_report` 字段区分 MO 上行和状态报告。

### 关闭连接：Exit

Exit 包有 1 字节 `reserve` body（总长 21 字节，比其他协议多 1 字节）。

## 服务端完整示例

参考：`examples/smgp-endpoint/src/server.rs`

```rust
let config = Arc::new(EndpointConfig::new("smgp-gateway", "0.0.0.0", 7892, 500, 60)
    .with_protocol("smgp"));

let server = serve(
    config,
    vec![Arc::new(MyBizHandler)],
    Some(Arc::new(SmgpAuth::new())),
    None,
    None, None, None,
).await?;
```

## 客户端完整示例

参考：`examples/smgp-endpoint/src/client.rs`

```rust
let endpoint = Arc::new(EndpointConfig::new("smgp-client", "127.0.0.1", port, 500, 60)
    .with_protocol("smgp"));

let conn = connect(
    endpoint,
    Arc::new(MyClientHandler),
    SmgpDecoder,
    Some(ClientConfig::default()),
    None, None,
).await?;

// 发送 Login
let login_pdu = build_login_pdu("106900", "password");
conn.write_frame(login_pdu.as_bytes()).await?;
```

## 参考测试

| 测试文件 | 说明 |
|----------|------|
| `examples/smgp-endpoint/tests/mod.rs` | 集成测试（9 个） |
| `examples/smgp-endpoint/tests/stress_test.rs` | 单账号压测 |
| `examples/smgp-endpoint/tests/multi_account_stress_test.rs` | 多账号压测（5×5，300s） |
| `examples/smgp-endpoint/tests/smgp_longmsg_test.rs` | 长短信测试 |
| `examples/smgp-endpoint/tests/dynamic_connection_test.rs` | 动态连接数调整测试 |
