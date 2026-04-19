# SGIP 客户端示例

## 功能简介

本示例实现了完整的 SGIP 客户端，具备以下功能：

- 连接 SGIP 服务端
- 明文认证（Bind.login_name + Bind.login_password）
- 通过 MessageSource 队列发送短信
- 接收独立 Report 状态报告（SGIP 特有）
- 长短信拆包/合包（通过 tpudhi 字段检测）
- 接收 Deliver MO 上行短信

## 运行方式

先启动服务端：

```bash
cargo run -p sgip-server-example
```

再启动客户端：

```bash
cargo run -p sgip-client-example
```

客户端连接到 `127.0.0.1:7891`，完成认证后自动发送 `messages.conf` 中的短信。

## 核心流程

1. **加载消息**：从 `messages.conf` 读取待发送短信列表
2. **建立连接**：调用 `connect()` 建立 TCP 连接，框架启动读循环、keepalive、outbound fetcher
3. **发送 Bind**：通过 `write_frame()` 发送 Bind 认证请求（明文方式）
4. **认证成功**：`SgipClientHandler.on_inbound()` 收到 BindResp 后设置认证标志
5. **自动发送 Submit**：框架的 `run_outbound_fetcher` 循环调用 `MessageSource.fetch()`，认证后自动将 Submit 发出
6. **处理响应**：`SgipClientHandler.on_inbound()` 处理所有服务端下发的帧：
   - SubmitResp -- 提交结果
   - Report -- 独立状态报告（SGIP 特有），回 ReportResp
   - Deliver -- MO 上行短信，回 DeliverResp
   - UnbindResp -- 断连响应

## 配置文件

### messages.conf -- 待发送短信

```
# 格式: 目标号码 短信内容
13800138000 Hello World from SGIP Client
13800138001 这是第二条测试短信
13800138002 Test Message 3
```

每行定义一条待发送短信。启动时所有消息被预构建为 Submit PDU 并放入 MessageSource 队列。

## SGIP 关键差异

### 1. 明文认证

SGIP 使用 Bind PDU 中的 login_name 和 login_password 明文传输，无需 MD5 计算。

### 2. 必须用 write_frame() 发送所有 PDU

SGIP 客户端不能使用 `send_request()`，必须使用 `write_frame()` 发送所有 PDU。原因是 `send_request()` 中 sequence_id 提取偏移量硬编码为 CMPP 的 bytes 8-11，而 SGIP 的序列号是 SgipSequence（node_id + timestamp + number），位于 bytes 8-19，结构完全不同。

```rust
// 正确方式：使用 write_frame()
conn.write_frame(&bind.to_pdu_bytes(node_id, timestamp, number)).await?;
```

### 3. 独立 Report 命令

SGIP 的状态报告是独立的 Report 命令，不通过 Deliver 承载。这与 CMPP/SMGP 不同（CMPP/SMGP 通过 Deliver 的 msg_fmt 字段区分普通 MO 和 Report）。

### 4. to_pdu_bytes 需三个参数

SGIP 的 PDU 编码需要三个参数：`to_pdu_bytes(node_id, timestamp, number)`，而非 CMPP 的单个 sequence_id。

## 代码结构

```
sgip_client/
  Cargo.toml              # 包名: sgip-client-example
  messages.conf           # 待发送短信列表
  src/
    main.rs               # 完整客户端实现
```

### main.rs 核心组件

- `load_messages()` -- 从 messages.conf 读取待发送短信
- `ClientMessageSource` -- 内存消息队列，实现 `MessageSource` trait
  - 启动时预构建所有 Submit PDU
  - 认证成功后 `fetch()` 返回待发送消息
  - `fetch()` 的 account 参数是 EndpointConfig.id
- `SgipClientHandler` -- 客户端处理器，实现 `ClientHandler` trait
  - 处理 BindResp：检查认证结果
  - 处理 SubmitResp：记录提交计数和结果
  - 处理 Report：独立状态报告，回 ReportResp
  - 处理 Deliver：MO 上行短信，回 DeliverResp
- `extract_sgip_sequence()` -- 从原始 PDU 字节中提取 SgipSequence（bytes 8-19）
- `sgip_timestamp()` -- 生成 SGIP 格式的时间戳

### 关键注意事项

- EndpointConfig 必须设置 `.with_protocol("sgip")`，否则框架无法正确识别协议类型
- MessageSource 中 fetch 的 key 必须和 EndpointConfig.id 一致
- 所有 PDU 响应都通过 `write_frame()` 发送，不使用 window 机制

## 长短信

SGIP 协议通过 `tpudhi` 字段标识长短信分段，与 CMPP 类似。消息内容字段为 `message_content`（注意不是 `msg_content`）。

### 发送长短信

客户端发送长短信时，使用 `LongMessageSplitter.split()` 拆分为多个带 UDH 的分段。每个分段设置 `tpudhi = 1`，通过 `MessageItem::Group` 推送到队列，框架保证同组分段走同一连接顺序发出：

```rust
let frames = splitter.split(content_bytes, alphabet);
let mut items = Vec::new();
for frame in frames {
    let mut submit = Submit::new().with_message(SP_NUMBER, phone, &frame.content);
    submit.report_flag = 1;
    if frame.has_udhi {
        submit.tpudhi = 1;
    }
    let bytes = submit.to_pdu_bytes(NODE_ID, ts, seq);
    items.push(Arc::new(RawPdu::from(bytes.to_vec())) as Arc<dyn EncodedPdu>);
}
queue.push_back(MessageItem::Group { items });
```

### 接收长短信 MO

客户端收到 Deliver（MO 上行）时，通过 `deliver.tpudhi == 1` 判断是否为长短信分段：

```rust
if deliver.tpudhi == 1 {
    if let Some((udh, _)) = UdhParser::extract_udh(&deliver.message_content) {
        let frame = LongMessageFrame::new(
            udh.reference_id, udh.total_segments, udh.segment_number,
            deliver.message_content.clone(), true, Some(udh),
        );
        match merger.add_frame(frame) {
            Ok(Some(complete)) => { /* 合包完成 */ }
            Ok(None) => { /* 等待更多分段 */ }
            Err(e) => { /* 合包错误 */ }
        }
    }
}
```

### 使用的 rsms-longmsg 组件

| 组件 | 用途 |
|------|------|
| `LongMessageSplitter` | 将长内容拆分为多个带 UDH 的分段 |
| `LongMessageMerger` | 将多个分段按 reference_id 合并为完整消息 |
| `UdhParser::extract_udh()` | 从 `message_content` 中提取 UDH header |
| `LongMessageFrame` | 长短信分段帧，包含 reference_id、分段信息 |
