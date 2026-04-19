# SMPP 服务端示例

基于 rsms 框架的 SMPP 服务端完整参考实现，演示认证、限流、MessageSource 队列、长短信拆包/合包和错误处理。

## 运行方式

```bash
cargo run -p smpp-server-example
```

服务端将在 `0.0.0.0:7893` 监听 SMPP 连接。

## 配置文件

### accounts.conf — 账号配置

格式：每行一个账号，`system_id` 和 `password` 以空格分隔。

```
# 格式: system_id password
900001 passwd12
900002 passwd34
```

**注意**：SMPP 协议规范要求 password 最大长度为 8 字节。

### messages.conf — 预定义 MO 消息

格式：每行一条，`目标账号 手机号 短信内容` 以空格分隔。

```
# 格式: 目标账号 手机号 短信内容
900001 13800138000 你好，这是一条测试短信
900001 13800138001 这是第二条测试消息
```

这些消息会在服务端启动时加载到 MessageSource 队列中，框架通过 `run_outbound_fetcher` 自动投递给对应账号的连接。

## 核心流程

1. 启动时从 `accounts.conf` 读取账号配置，从 `messages.conf` 加载预定义 MO 消息
2. 客户端连接后，框架自动完成 SMPP 协议握手（BindTransmitter/Resp）
3. 客户端发送 SubmitSm 时，`BusinessHandler.on_inbound()` 收到请求
4. 业务方自行解码 SubmitSm、回复 SubmitSmResp、处理业务逻辑
5. 需要状态报告时（`registered_delivery == 1`），通过 MessageSource 异步发送 DeliverSm Report
6. 框架的 `run_outbound_fetcher` 循环调用 `MessageSource.fetch(account, batch_size)` 投递消息

## SMPP 与 CMPP 的关键差异

### 1. 认证方式

SMPP 使用明文认证（无 MD5 计算），通过 `AuthCredentials::Smpp` 匹配：

```rust
AuthCredentials::Smpp {
    system_id,
    password,
    interface_version,
}
```

### 2. 16 字节 Header

SMPP 的 PDU Header 为 16 字节（比 CMPP/SMGP 的 12 字节多了 4 字节 `command_status`）：

| 字段 | 字节偏移 | 长度 |
|------|----------|------|
| command_length | 0-3 | 4 |
| command_id | 4-7 | 4 |
| command_status | 8-11 | 4 |
| sequence_number | 12-15 | 4 |

### 3. Report 承载方式

SMPP 通过 `DeliverSm` 承载状态报告，通过 `esm_class & 0x04` 区分：

```rust
DeliverSm {
    esm_class: 0x04,  // 标识为状态报告
    short_message: "id:xxx sub:001 dlvrd:001 submit date:done date:stat:DELIVRD err:000",
    tlvs: vec![
        Tlv::new(tags::RECEIPTED_MESSAGE_ID, msg_id.as_bytes().to_vec()),
        Tlv::new(tags::MESSAGE_STATE, vec![1]),
    ],
}
```

而 CMPP/SMGP 通过独立的 Report PDU 承载。

### 4. 长短信检测方式

SMPP 使用 `esm_class` 字段代替 CMPP/SGIP 的 `tpudhi`：

```rust
// 服务端检测长短信分段
if submit.esm_class & 0x40 != 0 {
    // 包含 UDH，为长短信分段
}

// 区分 Report 和长短信 MO
if esm_class & 0x04 != 0 {
    // 状态报告
} else if esm_class & 0x40 != 0 {
    // 长短信 MO 分段
} else {
    // 普通 MO 上行
}
```

SMPP 的消息内容字段为 `short_message`（不是 `msg_content`）。

### 5. MsgId 类型

SMPP 的 message_id 是 `String` 类型（如 `"0000000001"`），而 CMPP 是 `[u8; 8]`，SMGP 是 `SmgpMsgId`（10 字节）。

### 5. 版本参数

解码 SMPP 消息时需要传入版本参数：

```rust
decode_message_with_version(data, Some(SmppVersion::V34))
```

## 长短信

SMPP 协议通过 `esm_class` 字段标识长短信分段，代替 CMPP/SGIP 的 `tpudhi`。消息内容字段为 `short_message`。

### 检测方式

服务端通过 `submit.esm_class & 0x40 != 0` 判断消息是否包含 UDH header：

```rust
if submit.esm_class & 0x40 != 0 {
    if let Some((udh, _)) = UdhParser::extract_udh(&submit.short_message) {
        let frame = LongMessageFrame::new(
            udh.reference_id, udh.total_segments, udh.segment_number,
            submit.short_message.clone(), true, Some(udh),
        );
        merger.add_frame(frame);
    }
}
```

### 服务端处理流程

1. **接收 SubmitSm**：检查 `submit.esm_class & 0x40`
2. **提取 UDH**：调用 `UdhParser::extract_udh()` 从 `short_message` 中提取 UDH header
3. **合包**：通过 `LongMessageMerger.add_frame()` 合并分段
4. **完成回调**：所有分段到齐后返回完整内容
5. **超时清理**：`LongMessageMerger` 内置 TTL（默认 60 秒），自动清理过期分段

### MO 下发长短信

服务端下发 MO 消息时，若内容超过单条限制，使用 `LongMessageSplitter.split()` 拆分。每个分段设置 `esm_class = 0x40`，通过 `MessageItem::Group` 推送到队列：

```rust
let frames = splitter.split(mo.content.as_bytes(), alphabet);
if frames.len() == 1 && !frames[0].has_udhi {
    let pdu = build_deliver_sm_mo(&account, &phone, &content, 0);
    source.push_sync(&account, pdu);
} else {
    let items: Vec<Arc<dyn EncodedPdu>> = frames.into_iter().map(|frame| {
        let pdu = build_deliver_sm_mo_raw(&account, &phone, 0x40, &frame.content);
        Arc::new(pdu) as Arc<dyn EncodedPdu>
    }).collect();
    source.push_group_sync(&account, items);
}
```

### Report 与长短信 MO 的区分

SMPP 的 Report 和长短信 MO 都通过 `DeliverSm` 承载，通过 `esm_class` 区分：

- `esm_class & 0x04 != 0`：状态报告
- `esm_class & 0x40 != 0`：长短信 MO 分段（含 UDH）
- 其他：普通 MO 上行

### 使用的 rsms-longmsg 组件

| 组件 | 用途 |
|------|------|
| `UdhParser::extract_udh()` | 从 `short_message` 中提取 UDH header |
| `UdhParser::strip_udh()` | 去除 UDH header，返回纯消息内容 |
| `LongMessageSplitter` | 将长内容拆分为多个带 UDH 的分段 |
| `LongMessageMerger` | 将多个分段按 reference_id 合并为完整消息 |
| `LongMessageFrame` | 长短信分段帧，包含 reference_id、分段信息 |

## 代码结构

```
src/main.rs
├── 配置读取
│   ├── load_accounts()        — 从 accounts.conf 读取账号密码
│   └── load_mo_messages()     — 从 messages.conf 读取预定义 MO 消息
├── SmppAuthHandler            — 明文认证处理器（实现 AuthHandler trait）
├── FileMessageSource          — 内存消息队列（按账号隔离，实现 MessageSource trait）
├── SmppBusinessHandler        — 业务处理器（实现 BusinessHandler trait）
│   ├── handle_submit()        — 处理 SubmitSm，回 SubmitSmResp，推送 Report
│   └── on_inbound()           — 分发 SubmitSm/DeliverSm/EnquireLink
├── 辅助函数
│   ├── build_deliver_sm_report() — 构建状态报告 DeliverSm
│   └── build_deliver_sm_mo()     — 构建上行 DeliverSm
├── SimpleAccountConfigProvider  — 账号配置（限流、最大连接数）
├── SmppServerEventHandler       — 连接事件回调
└── main()                       — 组装各组件并启动服务
```
