# SMGP 客户端示例

## 功能简介

基于 rsms 框架的 SMGP 客户端参考实现，包含以下功能：

- 连接 SMGP 服务端
- MD5 认证（基于 client_id + password + timestamp）
- 通过 MessageSource 发送 Submit 短信
- 长短信拆包/合包（基于 UDH header 检测）
- 接收 SubmitResp、Deliver（状态报告和 MO 上行）

## 运行方式

先启动 SMGP 服务端：

```bash
cargo run -p smgp-server-example
```

再启动客户端：

```bash
cargo run -p smgp-client-example
```

客户端默认连接 `127.0.0.1:8890`，使用账号 `900001` / `password123`。

## 配置文件

### messages.conf（待发送短信）

每行一条短信，格式为 `目标号码 短信内容`，以 `#` 开头的行和空行被忽略：

```
# 目标号码 短信内容
13800138000 Hello World from SMGP Client
13800138001 这是第二条测试短信
13800138002 Test Message 3
```

客户端启动时加载这些消息到 `ClientMessageSource` 队列，认证成功后由框架自动 fetch 并发送。

## 核心流程

1. 从 `messages.conf` 读取待发送短信列表
2. 调用 `connect()` 建立 TCP 连接，框架启动读循环、keepalive、outbound fetcher
3. 通过 `send_request()` 发送 Login 认证请求（MD5），等待 LoginResp
4. 认证成功后，`MessageSource.fetch()` 被框架 outbound fetcher 循环调用，自动发出 Submit
5. `ClientHandler.on_inbound()` 处理所有服务端响应：SubmitResp、Deliver（Report/MO）

## 代码结构

| 组件 | 职责 |
|---|---|
| `ClientMessageSource` | 实现 `MessageSource` trait，维护待发送的 Submit PDU 队列。认证前 `fetch()` 返回空，认证后按批次返回消息。框架的 `run_outbound_fetcher` 循环调用 `fetch(endpoint_id, 16)` 取出消息通过 `write_frame` 直接发出 |
| `SmgpClientHandler` | 实现 `ClientHandler` trait，处理服务端下发的所有帧：LoginResp（认证结果）、SubmitResp（提交结果）、Deliver（状态报告或 MO 上行）、ActiveTestResp（心跳响应）。收到 Deliver 后回 DeliverResp |
| `main()` | 组装各组件：创建 EndpointConfig、构建 MessageSource 和 Handler、调用 `connect()` 建立连接、通过 `send_request()` 发送 Login 认证 |

## 与 CMPP 客户端的差异

- **认证方式**：SMGP 使用 `compute_login_auth(client_id, password, timestamp)` 计算 MD5 认证值，CMPP 使用 `compute_connect_auth()`
- **认证命令**：SMGP 使用 Login/LoginResp，CMPP 使用 Connect/ConnectResp
- **need_report 字段**：SMGP Submit 设置 `submit.need_report = 1` 请求状态报告，CMPP 使用 `registered_delivery`
- **is_report 判断**：SMGP 通过 `deliver.is_report == 1` 判断 Deliver 是状态报告还是 MO 上行，CMPP 通过 `deliver.registered_delivery == 1` 判断
- **Report 解析**：SMGP 使用 `SmgpReport::parse(&deliver.msg_content)` 解析状态报告内容
- **MsgId 类型**：SMGP 的 `SmgpMsgId` 是 10 字节结构，CMPP 是 8 字节数组

## 长短信

SMGP 协议没有 `tpudhi` 字段，长短信检测通过直接检查 `msg_content` 中的 UDH header 实现。

### 发送长短信

客户端发送长短信时，使用 `LongMessageSplitter.split()` 将内容拆分为多个带 UDH header 的分段，每段构造一个 Submit PDU，通过 `MessageItem::Group` 推送到队列，框架保证同组分段走同一连接顺序发出：

```rust
let frames = splitter.split(content.as_bytes(), alphabet);
let mut items = Vec::new();
for frame in frames {
    let submit = Submit::new().with_message(ACCOUNT, phone, &frame.content);
    let pdu: Pdu = submit.into();
    items.push(Arc::new(pdu.to_pdu_bytes(seq)) as Arc<dyn EncodedPdu>);
}
queue.push_back(MessageItem::Group { items });
```

SMGP 的 Submit 没有 `tpudhi` 字段，长短信分段通过 `msg_content` 内嵌的 UDH header 标识。

### 接收长短信 MO

客户端收到 Deliver（MO 上行）时，通过 `UdhParser::extract_udh()` 检测 `msg_content` 中是否包含 UDH header：

```rust
} else if let Some((udh, _)) = UdhParser::extract_udh(&deliver.msg_content) {
    let frame = LongMessageFrame::new(
        udh.reference_id, udh.total_segments, udh.segment_number,
        deliver.msg_content.clone(), true, Some(udh),
    );
    match merger.add_frame(frame) {
        Ok(Some(complete)) => { /* 合包完成 */ }
        Ok(None) => { /* 等待更多分段 */ }
        Err(e) => { /* 合包错误 */ }
    }
}
```

注意：SMGP 先判断 `deliver.is_report` 是否为状态报告，再判断是否为长短信 MO。

### 使用的 rsms-longmsg 组件

| 组件 | 用途 |
|------|------|
| `LongMessageSplitter` | 将长内容拆分为多个带 UDH 的分段 |
| `LongMessageMerger` | 将多个分段按 reference_id 合并为完整消息 |
| `UdhParser::extract_udh()` | 从 `msg_content` 中提取 UDH header |
| `LongMessageFrame` | 长短信分段帧，包含 reference_id、分段信息 |
