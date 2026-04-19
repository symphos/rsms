# 四协议差异速查

## Header 结构对比

| 协议 | Header 长度 | 结构 |
|------|-------------|------|
| **CMPP** | 12 字节 | `Total_Length(4) + Command_Id(4) + Sequence_Id(4)` |
| **SMGP** | 12 字节 | `Total_Length(4) + Command_Id(4) + Sequence_Id(4)` |
| **SMPP** | **16 字节** | `Command_Length(4) + Command_Id(4) + Command_Status(4) + Sequence_Number(4)` |
| **SGIP** | **20 字节** | `Total_Length(4) + Command_Id(4) + SgipSequence(12)` |

> SMPP 多了 4 字节 `command_status`；SGIP 使用 12 字节的自定义序列号（`node_id + timestamp + number`）。

## 认证方式对比

| 协议 | 认证命令 | 认证参数 | 认证方式 |
|------|----------|----------|----------|
| **CMPP** | `Connect` | `source_addr` + `authenticator_source`(16字节MD5) + `version` + `timestamp` | MD5 摘要 |
| **SMGP** | `Login` | `client_id` + `authenticator`(16字节) + `login_mode` + `timestamp` + `version` | MD5 摘要 |
| **SMPP** | `BindTransmitter` | `system_id` + `password` + `interface_version` | **明文密码** |
| **SGIP** | `Bind` | `login_type` + `login_name` + `login_password` + `reserve` | **明文密码** |

## AuthCredentials 变体

```rust
pub enum AuthCredentials {
    Cmpp  { source_addr: String, authenticator_source: [u8; 16], version: u8, timestamp: u32 },
    Smgp  { client_id: String, authenticator: [u8; 16], version: u8 },
    Smpp  { system_id: String, password: String, interface_version: u8 },
    Sgip  { login_name: String, login_password: String },
}
```

## 消息类型对比

| 协议 | MT 提交 | MT 响应 | MO 上行 | 状态报告 | 心跳 | 关闭连接 |
|------|---------|---------|---------|----------|------|----------|
| **CMPP** | `Submit` | `SubmitResp` | `Deliver` | `Deliver`(registered_delivery=1) | `ActiveTest` | `Terminate` |
| **SMGP** | `Submit` | `SubmitResp` | `Deliver` | `Deliver`(is_report=1) | `ActiveTest` | `Exit` |
| **SMPP** | `SubmitSm` | `SubmitSmResp` | `DeliverSm` | `DeliverSm`(esm_class=0x04) | `EnquireLink` | `Unbind` |
| **SGIP** | `Submit` | `SubmitResp` | `Deliver` | **独立 `Report` 命令** | 无 | `Unbind` |

> **SGIP 特殊**：状态报告不通过 Deliver 承载，而是使用独立的 `Report` 命令。

## MsgId 格式对比

| 协议 | MsgId 类型 | 长度 | 说明 |
|------|-----------|------|------|
| **CMPP** | `[u8; 8]` / `u64` | 8 字节 | 二进制 |
| **SMGP** | `SmgpMsgId` | 10 字节 | 自定义结构 |
| **SMPP** | `String` (C-string) | 可变 | ASCII 字符串 |
| **SGIP** | 通过 `SgipSequence` | 12 字节 | 隐式，通过 sequence 匹配 |

## 长短信字段对比

| 特性 | CMPP | SMGP | SMPP | SGIP |
|------|------|------|------|------|
| UDH 标志 | `tpudhi` 固定字段 | TLV `TP_UDHI`(0x0002) | `esm_class & 0x40` | `tpudhi` 固定字段 |
| 分段信息 | `pk_total` / `pk_number` | TLV `PK_TOTAL` / `PK_NUMBER` | UDH 头部 | UDH 头部 |

## 压测性能参考

所有协议均在 30 秒单账号压测中达到 TPS 2,500+，300 秒多账号压测中零丢失：

| 指标 | CMPP | SMGP | SMPP | SGIP |
|------|------|------|------|------|
| 单账号 1 连接 TPS | 2,500 | 2,500 | 2,500 | 2,500 |
| 单账号 5 连接 TPS | 2,500 | 2,567 | 2,500 | 2,500 |
| 5 账号 25 连接总 TPS | 12,500+ | 12,500+ | 12,500+ | 12,500+ |
| 300 秒消息丢失 | 0 | 0 | 0 | 0 |
