# CMPP 客户端示例

基于 rsms 框架的 CMPP 客户端完整参考实现，演示连接服务端、MD5 认证、发送短信（含长短信拆包）、接收状态报告和 MO 合包的集成方式。

## 运行方式

先启动服务端：

```bash
cargo run -p cmpp-server-example
```

再启动客户端：

```bash
cargo run -p cmpp-client-example
```

客户端默认连接 `127.0.0.1:7890`，使用账号 `900001` / `password123` 认证。

## 配置文件

### messages.conf -- 待发送短信

```
# 格式: 目标号码 短信内容
13800138000 Hello World from CMPP Client
13800138001 这是第二条测试短信
13800138002 Test Message 3
```

每行一条消息，客户端启动时加载到 `MessageSource` 队列，认证成功后自动发送。

## 核心流程

1. 从 `messages.conf` 读取待发送短信列表
2. 调用 `connect()` 建立 TCP 连接，框架启动读循环、心跳和 outbound fetcher
3. 通过 `send_request()` 发送 Connect 认证（MD5），等待 ConnectResp 确认
4. 认证成功后，`MessageSource.fetch()` 被 outbound fetcher 循环调用，自动发出 Submit
5. `ClientHandler.on_inbound()` 接收并处理所有服务端响应（SubmitResp、Deliver Report/MO）

## 代码结构

### MessageSource -- ClientMessageSource

客户端消息发送队列。启动时将 `messages.conf` 中的每条消息构造为 Submit PDU 并入队，支持长短信自动拆分。实现了 `MessageSource` trait：

- `fetch(account, batch_size)` -- 框架的 `run_outbound_fetcher` 循环调用此方法取出消息，通过 `write_frame` 直接发出（不走 window 机制）
- 认证前 `fetch` 返回空，认证通过后才开始发送
- 超过单条上限的消息自动拆分为多段，封装为 `MessageItem::Group` 保证顺序发送

注意：`fetch` 的 `account` 参数就是 `EndpointConfig.id`，必须和 `connect()` 时设置的 endpoint id 一致。

### ClientHandler -- CmppClientHandler

处理服务端下发的所有帧，在 `on_inbound` 中根据 `command_id` 分发处理：

- **ConnectResp** -- 判断认证结果，成功后设置 `authenticated` 标志，触发 MessageSource 开始发送
- **SubmitResp** -- 记录提交结果和 msg_id，累计计数
- **Deliver（Report）** -- `registered_delivery=1` 时解析 `CmppReport`（包含 MsgId、Stat、DestTerminalId 等），累计报告计数
- **Deliver（MO 上行）** -- `registered_delivery=0` 时为上行短信，记录内容，并回复 DeliverResp
- **ActiveTestResp** -- 心跳响应，debug 级别日志

## 关键代码说明

### MD5 认证

```rust
let timestamp = 0u32;
let authenticator = compute_connect_auth(ACCOUNT, PASSWORD, timestamp);
let connect_pdu = Connect {
    source_addr: ACCOUNT.to_string(),
    authenticator_source: authenticator,
    version: 0x30,
    timestamp,
};
let pdu: Pdu = connect_pdu.into();
conn.send_request(pdu.to_pdu_bytes(1)).await?;
```

CMPP 使用 MD5 认证：`MD5(SourceAddr + 9字节的0 + Password + Timestamp)`，由 `compute_connect_auth()` 完成。

### 接收状态报告并匹配 msgId

```rust
if deliver.registered_delivery == 1 {
    let report_str = String::from_utf8_lossy(&deliver.msg_content);
    if let Some(report) = CmppReport::parse(&report_str) {
        // report.msg_id 可与 SubmitResp 返回的 msg_id 匹配
        // 匹配完成后应立即移除映射，避免内存泄漏
    }
}
```

状态报告以 Deliver PDU（`registered_delivery=1`）承载，内容为文本格式，使用 `CmppReport::parse()` 解析。实际业务中需维护 msgId 到业务信息的映射，匹配完成后立即移除。

## 长短信

### MT 拆包（发送端）

`ClientMessageSource::from_messages()` 加载消息时，会自动检测消息长度并决定是否拆分：

1. 通过 `detect_alphabet()` 判断编码类型：纯 ASCII 按 160 字节为单条上限，UCS2 按 70 字节
2. 超过单条上限时，调用 `LongMessageSplitter::split()` 将内容拆分为多个分段，每段自动添加 UDH 头（含 `reference_id`、`total_segments`、`segment_number`）
3. 为每个分段构造独立的 Submit PDU，设置 `tpudhi = 1` 表示内容包含 UDH 头，同时填写 `pk_total` 和 `pk_number` 字段
4. 同一消息的所有分段封装为 `MessageItem::Group { items }`，框架保证同组分段走同一连接、按顺序发送

```rust
if content_bytes.len() > single_max {
    let frames = splitter.split(content_bytes, alphabet);
    let mut items = Vec::new();
    for (i, frame) in frames.into_iter().enumerate() {
        let mut submit = Submit::new().with_message(ACCOUNT, phone, &frame.content);
        submit.pk_total = total as u8;
        submit.pk_number = (i + 1) as u8;
        if frame.has_udhi { submit.tpudhi = 1; }
        items.push(Arc::new(pdu.to_pdu_bytes(seq)) as Arc<dyn EncodedPdu>);
    }
    queue.push_back(MessageItem::Group { items });
}
```

`MessageItem::Single` 用于普通短消息，`MessageItem::Group` 用于长短信分段组。

### MO 合包（接收端）

当客户端收到服务端下发的 Deliver 且 `tpudhi == 1` 时，表示该 MO 消息属于长短信分段。客户端处理流程：

1. 通过 `UdhParser::extract_udh(&deliver.msg_content)` 提取 UDH 头，获取 `reference_id`、`total_segments`、`segment_number`
2. 封装为 `LongMessageFrame`，调用 `LongMessageMerger::add_frame()` 缓存分段
3. `add_frame()` 返回 `Ok(Some(complete))` 表示所有分段收齐，`complete` 为完整消息内容

```rust
if deliver.tpudhi == 1 {
    if let Some((udh, _)) = UdhParser::extract_udh(&deliver.msg_content) {
        let frame = LongMessageFrame::new(
            udh.reference_id, udh.total_segments, udh.segment_number,
            deliver.msg_content.clone(), true, Some(udh),
        );
        match merger.add_frame(frame) {
            Ok(Some(complete)) => { /* 合包完成 */ }
            Ok(None) => { /* 等待更多分段 */ }
            Err(e) => { /* 错误处理 */ }
        }
    }
}
```

关键类：`LongMessageSplitter`（长短信拆分）、`LongMessageMerger`（分段缓存与合包）、`UdhParser`（UDH 解析）、`LongMessageFrame`（分帧数据结构）。

## 依赖 crate

| crate | 用途 |
|-------|------|
| rsms-core | 核心类型（EndpointConfig、Frame、RawPdu） |
| rsms-connector | 客户端框架（connect、ClientHandler、MessageSource、CmppDecoder） |
| rsms-codec-cmpp | CMPP 协议编解码（compute_connect_auth、decode_message、Submit、CmppReport） |
| rsms-longmsg | 长短信拆分/合包（LongMessageSplitter、LongMessageMerger、UdhParser） |
