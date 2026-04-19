# SGIP 服务端示例

## 功能简介

本示例实现了完整的 SGIP 服务端，具备以下功能：

- 明文认证（Bind.login_name + Bind.login_password），无 MD5
- 按账号限流
- MessageSource 内存消息队列，按账号隔离
- 长短信拆包/合包（通过 tpudhi 字段检测）
- 独立 Report 命令（不通过 Deliver 承载）

## 运行方式

```bash
cargo run -p sgip-server-example
```

启动后服务端监听 `0.0.0.0:7891`，等待客户端连接。

## 配置文件

### accounts.conf -- 账号密码配置

```
# 格式: login_name login_password
106900 password123
106901 password456
```

SGIP 使用明文认证，无需 MD5 计算。每行定义一个账号及其密码。

### messages.conf -- 预定义 MO 上行消息

```
# 格式: 目标账号 手机号 短信内容
106900 13800138000 你好，这是一条SGIP测试短信
106900 13800138001 这是第二条测试消息
```

服务端启动时加载这些消息到 MessageSource 队列，当客户端连接并通过认证后，框架自动通过 `run_outbound_fetcher` 调用 `fetch()` 将消息下发给客户端。

## 核心流程

1. **启动**：从 `accounts.conf` 读取账号配置，从 `messages.conf` 加载预定义 MO 消息到 MessageSource 队列
2. **连接建立**：客户端连接后，框架自动完成 SGIP 协议握手（Bind/BindResp）
3. **认证**：`SgipAuthHandler` 使用明文方式校验 login_name 和 login_password
4. **接收 Submit**：`SgipBusinessHandler.on_inbound()` 收到客户端提交的短信
5. **回 SubmitResp**：业务方解码 Submit 后，通过 `write_frame()` 发送 SubmitResp（result=0 表示成功）
6. **推送 Report**：如果 Submit 的 report_flag=1，构建 Report PDU 推送到 MessageSource 队列
7. **异步下发**：框架通过 MessageSource.fetch() 自动将 Report 和 Deliver 下发给客户端

## SGIP 与 CMPP 的关键差异

| 特性 | SGIP | CMPP |
|------|------|------|
| 认证方式 | 明文（login_name + login_password） | MD5(source_addr + password + authenticator) |
| Header 大小 | 20 字节（含 12 字节 SgipSequence） | 12 字节（含 4 字节 sequence_id） |
| SgipSequence 结构 | node_id(4) + timestamp(4) + number(4) | 无此概念 |
| 状态报告 | 独立 Report 命令 | 通过 Deliver（msg_fmt=report）承载 |
| to_pdu_bytes 参数 | (node_id, timestamp, number) 三个参数 | (sequence_id) 一个参数 |
| SubmitResp | 只有 result 字段，没有 msg_id | 有 msg_id 和 result |

## 长短信

SGIP 协议通过 `submit.tpudhi` 字段标识长短信分段，与 CMPP 类似。消息内容字段为 `message_content`（注意不是 `msg_content`）。

### 检测方式

服务端通过 `submit.tpudhi == 1` 判断消息是否包含 UDH header：

```rust
if submit.tpudhi == 1 {
    if let Some((udh, _)) = UdhParser::extract_udh(&submit.message_content) {
        let frame = LongMessageFrame::new(
            udh.reference_id, udh.total_segments, udh.segment_number,
            submit.message_content.clone(), true, Some(udh),
        );
        merger.add_frame(frame);
    }
}
```

### 服务端处理流程

1. **接收 Submit**：检查 `submit.tpudhi` 是否为 1
2. **提取 UDH**：调用 `UdhParser::extract_udh()` 从 `message_content` 中提取 UDH header
3. **合包**：通过 `LongMessageMerger.add_frame()` 合并分段
4. **完成回调**：所有分段到齐后返回完整内容
5. **超时清理**：`LongMessageMerger` 内置 TTL（默认 60 秒），自动清理过期分段

### MO 下发长短信

服务端下发 MO 消息时，若内容超过单条限制（ASCII 160 字节 / UCS2 70 字节），使用 `LongMessageSplitter.split()` 拆分。每个分段设置 `tpudhi = 1`，通过 `MessageItem::Group` 推送到队列：

```rust
let frames = splitter.split(content_bytes, alphabet);
let mut items = Vec::new();
for frame in frames {
    let pdu = build_deliver_mo_with_udh(&account, &phone, &frame.content, frame.has_udhi);
    items.push(Arc::new(pdu) as Arc<dyn EncodedPdu>);
}
source.push_group_sync(&account, MessageItem::Group { items });
```

### 使用的 rsms-longmsg 组件

| 组件 | 用途 |
|------|------|
| `UdhParser::extract_udh()` | 从 `message_content` 中提取 UDH header |
| `UdhParser::strip_udh()` | 去除 UDH header，返回纯消息内容 |
| `LongMessageSplitter` | 将长内容拆分为多个带 UDH 的分段 |
| `LongMessageMerger` | 将多个分段按 reference_id 合并为完整消息 |
| `LongMessageFrame` | 长短信分段帧，包含 reference_id、分段信息 |

## 代码结构

```
sgip_server/
  Cargo.toml              # 包名: sgip-server-example
  accounts.conf           # 账号密码配置
  messages.conf           # 预定义 MO 消息
  src/
    main.rs               # 完整服务端实现
```

### main.rs 核心组件

- `load_accounts()` / `load_mo_messages()` -- 从配置文件加载账号和消息
- `extract_sgip_sequence()` -- 从原始 PDU 字节中提取 SgipSequence（bytes 8-19）
- `SgipAuthHandler` -- SGIP 明文认证处理器，实现 `AuthHandler` trait
- `FileMessageSource` -- 内存消息队列，实现 `MessageSource` trait，按账号隔离
- `SgipBusinessHandler` -- 业务处理器，实现 `BusinessHandler` trait
  - 处理 Submit：解码、回 SubmitResp、推送 Report 到队列
  - 处理 Report：记录日志、回 ReportResp
  - 处理 Deliver（MO 上行）：记录日志、回 DeliverResp
- `build_report()` -- 构建 Report PDU
- `build_deliver_mo()` -- 构建 Deliver MO 上行 PDU
- `SgipServerEventHandler` -- 连接/断开/认证事件回调
- `SimpleAccountConfigProvider` -- 账号配置提供者（最大连接数、QPS）
