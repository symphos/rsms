# SMGP 服务端示例

## 功能简介

基于 rsms 框架的 SMGP 服务端参考实现，包含以下功能：

- SMGP 协议 MD5 认证
- 按账号限流（可配置 QPS 上限）
- 通过 MessageSource 队列异步下发 Deliver（MO 消息和状态报告）
- 长短信拆包/合包（基于 UDH header 检测）
- 完整的错误处理与日志记录

## 前置条件

- Rust 编译环境（stable）
- rsms 框架完整工作区已就绪

## 运行方式

```bash
cargo run -p smgp-server-example
```

服务端将在 `0.0.0.0:8890` 启动监听。

## 配置文件

### accounts.conf（账号配置）

每行一个账号，格式为 `client_id password`，以 `#` 开头的行和空行被忽略：

```
# client_id password
900001 password123
900002 password456
```

### messages.conf（MO 消息配置）

预定义的模拟 MO 上行消息，每行一条，格式为 `目标账号 手机号 短信内容`：

```
# 目标账号 手机号 短信内容
900001 13800138000 你好，这是一条测试短信
900001 13800138001 这是第二条测试消息
```

服务端启动时会加载这些消息到 MessageSource 队列，框架通过 `run_outbound_fetcher` 自动按账号 fetch 并下发。

## 核心流程

1. 从 `accounts.conf` 读取账号配置
2. 客户端连接后，框架自动完成 SMGP 协议握手（Login/LoginResp）
3. 客户端发送 Submit，由 `BusinessHandler.on_inbound()` 接收处理
4. 业务方在 handler 中解码 Submit、构造 SubmitResp 并回写、处理业务逻辑
5. 通过 MessageSource 异步发送 Deliver（MO 上行消息和状态报告）

## 代码结构

| 组件 | 职责 |
|---|---|
| `SmgpAuthHandler` | 实现 `AuthHandler` trait，校验客户端账号是否存在。SMGP 的完整 MD5 认证由框架 smgp handler 完成，此处做账号存在性校验 |
| `SmgpBusinessHandler` | 实现 `BusinessHandler` trait，处理收到的 Submit 请求：解码消息、回 SubmitResp、若需要状态报告则通过 MessageSource 异步推送 Report |
| `FileMessageSource` | 实现 `MessageSource` trait，维护按账号隔离的内存消息队列。支持从文件预加载 MO 消息，也支持运行时动态 push Report/MO。框架的 `run_outbound_fetcher` 循环调用 `fetch(account, batch_size)` 批量取出消息并下发 |
| `SimpleAccountConfigProvider` | 实现 `AccountConfigProvider` trait，返回每个账号的连接数上限和 QPS 限制 |
| `SmgpServerEventHandler` | 实现 `ServerEventHandler` trait，处理连接/断开/认证成功等事件通知 |

## 与 CMPP 的差异

SMGP 协议与 CMPP 协议在框架中的使用方式类似，主要差异如下：

- **认证命令**：SMGP 使用 Login/LoginResp（CMPP 使用 Connect/ConnectResp）
- **认证方式**：SMGP 使用 `compute_login_auth()` 进行 MD5 认证，基于 client_id + password + timestamp 计算
- **need_report 字段**：SMGP Submit 中使用 `need_report` 字段（值为 1 表示需要状态报告），对应 CMPP 的 `registered_delivery`
- **状态报告**：SMGP 通过 `Deliver(is_report=1)` 承载状态报告，Report 内容使用 `SmgpReport` 结构体编码
- **MsgId 类型**：SMGP 使用 `SmgpMsgId`（10 字节），CMPP 使用 `[u8; 8]`

## 长短信

SMGP 协议没有 `tpudhi` 字段，长短信检测不依赖协议头标志位，而是直接检查 `msg_content` 中的 UDH（User Data Header）。

### 检测方式

通过 `UdhParser::extract_udh(&submit.msg_content)` 直接检查消息内容的首字节，判断是否包含 UDH header：

```rust
if let Some((udh, _)) = UdhParser::extract_udh(&submit.msg_content) {
    // 包含 UDH，为长短信分段
    let frame = LongMessageFrame::new(
        udh.reference_id, udh.total_segments, udh.segment_number,
        submit.msg_content.clone(), true, Some(udh),
    );
    merger.add_frame(frame);
}
```

### 服务端处理流程

1. **接收 Submit**：收到客户端提交的短信
2. **检测 UDH**：调用 `UdhParser::extract_udh()` 检查 `msg_content` 是否包含 UDH header
3. **合包**：将分段构造为 `LongMessageFrame`，通过 `LongMessageMerger.add_frame()` 合并
4. **完成回调**：所有分段到齐后 `add_frame()` 返回完整内容，业务方处理完整消息
5. **超时清理**：`LongMessageMerger` 内置 TTL 机制（默认 60 秒），自动清理过期分段

### MO 下发长短信

服务端下发 MO 消息时，若内容超过单条限制（ASCII 160 字节 / UCS2 70 字节），使用 `LongMessageSplitter.split()` 拆分，并通过 `MessageItem::Group` 推送到队列，框架保证同组分段走同一连接顺序发出：

```rust
let frames = splitter.split(mo.content.as_bytes(), alphabet);
let mut items = Vec::new();
for frame in frames {
    let pdu = build_deliver_mo_with_udh(&account, &phone, &frame.content);
    items.push(Arc::new(pdu) as Arc<dyn EncodedPdu>);
}
source.push_sync(&account, MessageItem::Group { items });
```

### 使用的 rsms-longmsg 组件

| 组件 | 用途 |
|------|------|
| `UdhParser::extract_udh()` | 从 `msg_content` 中提取 UDH header，返回 `UdhHeader` 和 UDH 长度 |
| `UdhParser::strip_udh()` | 去除 UDH header，返回纯消息内容 |
| `LongMessageSplitter` | 将长内容拆分为多个带 UDH 的分段 |
| `LongMessageMerger` | 将多个分段按 reference_id 合并为完整消息 |
| `LongMessageFrame` | 长短信分段帧，包含 reference_id、total_segments、segment_number |
