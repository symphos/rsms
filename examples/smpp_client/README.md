# SMPP 客户端示例

基于 rsms 框架的 SMPP 客户端完整参考实现，演示连接 SMPP 服务端、明文认证、发送短信、长短信拆包/合包和接收状态报告。

## 运行方式

先启动服务端：

```bash
cargo run -p smpp-server-example
```

再启动客户端：

```bash
cargo run -p smpp-client-example
```

## 配置文件

### messages.conf — 待发送短信

格式：每行一条，`目标号码 短信内容` 以空格分隔。

```
# 格式: 目标号码 短信内容
13800138000 Hello World from SMPP Client
13800138001 这是第二条测试短信
13800138002 Test Message 3
```

启动时加载到 `ClientMessageSource` 队列中，认证成功后框架自动通过 `MessageSource.fetch()` 投递。

## 核心流程

1. 启动时从 `messages.conf` 读取待发送短信列表
2. `connect()` 建立 TCP 连接，框架启动读循环、keepalive、outbound fetcher
3. `write_frame()` 发送 BindTransmitter 认证（明文，无 MD5）
4. 认证成功后 `MessageSource.fetch()` 被 outbound fetcher 循环调用，自动发出 SubmitSm
5. `ClientHandler.on_inbound()` 处理所有服务端响应：
   - **SubmitSmResp** — 短信提交结果，包含 String 类型的 message_id
   - **DeliverSm (esm_class & 0x04)** — 状态报告
   - **DeliverSm (esm_class == 0)** — 上行短信（MO）
   - **EnquireLinkResp** — 心跳响应

## SMPP 关键差异

### 明文认证

SMPP 使用 `BindTransmitter` 明文认证，不需要 MD5 计算：

```rust
let bind = BindTransmitter::new(system_id, password, system_type, interface_version);
```

### EndpointConfig 必须设置 protocol

客户端的 `EndpointConfig` 必须加 `.with_protocol("smpp")`，否则框架会用 CMPP 的偏移量（bytes 8-11）提取 sequence_id，导致请求/响应匹配失败：

```rust
EndpointConfig::new(ACCOUNT, host, port, 100, 60)
    .with_protocol("smpp")  // 必须设置！sequence_id 在 bytes 12-15
```

### Report 判断方式

状态报告通过 `DeliverSm` 承载，通过 `esm_class` 字段区分：

```rust
if deliver.esm_class & 0x04 != 0 {
    // 状态报告
} else if deliver.esm_class & 0x40 != 0 {
    // 长短信 MO 分段（含 UDH）
} else {
    // 上行短信（MO）
}
```

### MsgId 类型

SMPP 的 message_id 是 `String` 类型（如 `"0000000001"`），非定长字节数组。

### 心跳机制

SMPP 使用 `EnquireLink`/`EnquireLinkResp` 替代 CMPP 的 `ActiveTest`/`ActiveTestResp`。

## 注意事项

- **password 最大 8 字节**：SMPP 协议规范限制
- **EndpointConfig.id 与 MessageSource fetch key 一致**：`run_outbound_fetcher` 使用 `conn.authenticated_account()`（即 endpoint.id）作为 fetch 的 key
- **MessageSource 在认证前不会返回消息**：`ClientMessageSource.fetch()` 检查 `authenticated` 标志，认证成功后才投递

## 长短信

SMPP 协议通过 `esm_class` 字段标识长短信分段，代替 CMPP/SGIP 的 `tpudhi`。消息内容字段为 `short_message`。

### 发送长短信

客户端发送长短信时，使用 `LongMessageSplitter.split()` 拆分为多个带 UDH 的分段。每个分段设置 `esm_class = 0x40`，通过 `MessageItem::Group` 推送到队列，框架保证同组分段走同一连接顺序发出：

```rust
let frames = splitter.split(content.as_bytes(), alphabet);
if frames.len() == 1 && !frames[0].has_udhi {
    // 普通短消息
    let submit = SubmitSm::new().with_message(phone, ACCOUNT, &frames[0].content);
    queue.push_back(MessageItem::Single(Arc::new(pdu.to_pdu_bytes(0))));
} else {
    // 长短信分段
    let items: Vec<Arc<dyn EncodedPdu>> = frames.into_iter().map(|frame| {
        let mut submit = SubmitSm::new().with_message(phone, ACCOUNT, &frame.content);
        submit.esm_class = 0x40;
        let pdu: Pdu = submit.into();
        Arc::new(pdu.to_pdu_bytes(0)) as Arc<dyn EncodedPdu>
    }).collect();
    queue.push_back(MessageItem::Group { items });
}
```

### 接收长短信 MO

客户端收到 `DeliverSm` 时，通过 `esm_class` 区分状态报告、长短信 MO 和普通 MO：

```rust
if esm_class & 0x04 != 0 {
    // 状态报告
} else if esm_class & 0x40 != 0 {
    // 长短信 MO 分段，用 merger 合包
    if let Some((udh, _)) = UdhParser::extract_udh(&deliver.short_message) {
        let frame = LongMessageFrame::new(
            udh.reference_id, udh.total_segments, udh.segment_number,
            deliver.short_message.clone(), true, Some(udh),
        );
        match merger.add_frame(frame) {
            Ok(Some(complete)) => { /* 合包完成 */ }
            Ok(None) => { /* 等待更多分段 */ }
            Err(e) => { /* 合包错误 */ }
        }
    }
} else {
    // 普通 MO 上行
}
```

### 使用的 rsms-longmsg 组件

| 组件 | 用途 |
|------|------|
| `LongMessageSplitter` | 将长内容拆分为多个带 UDH 的分段 |
| `LongMessageMerger` | 将多个分段按 reference_id 合并为完整消息 |
| `UdhParser::extract_udh()` | 从 `short_message` 中提取 UDH header |
| `LongMessageFrame` | 长短信分段帧，包含 reference_id、分段信息 |

## 代码结构

```
src/main.rs
├── load_messages()            — 从 messages.conf 读取待发送短信
├── ClientMessageSource        — 客户端消息队列（实现 MessageSource trait）
│   └── fetch()                — 认证后返回待发送 SubmitSm PDU
├── SmppClientHandler          — 客户端处理器（实现 ClientHandler trait）
│   └── on_inbound()           — 处理 BindResp/SubmitSmResp/DeliverSm/EnquireLinkResp
└── main()                     — 组装各组件并启动
    ├── connect()              — 建立 TCP 连接
    ├── send_request()         — 发送 BindTransmitter 认证
    └── MessageSource          — 框架自动 fetch + write_frame
```
