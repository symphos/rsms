# CMPP 服务端示例

基于 rsms 框架的 CMPP 服务端完整参考实现，演示认证、限流、MessageSource 队列、长短信合包和错误处理的集成方式。

## 前置条件

- Rust 编译环境（stable 版本）
- rsms 框架依赖已就绪（项目根目录执行 `cargo build`）

## 运行方式

```bash
cargo run -p cmpp-server-example
```

服务端默认监听 `0.0.0.0:7890`。

## 配置文件

### accounts.conf -- 账号密码

```
# 格式: 账号 密码
# 每行一个账号，# 开头为注释
900001 password123
900002 password456
```

客户端连接时使用这些账号进行 MD5 认证。

### messages.conf -- 预定义 MO 消息

```
# 格式: 目标账号 手机号 短信内容
900001 13800138000 你好，这是一条测试短信
```

服务端启动时加载这些消息到 MessageSource 队列，连接认证后自动下发给对应账号的客户端。

## 核心流程

1. 从 `accounts.conf` 读取账号配置，从 `messages.conf` 读取预定义 MO 消息
2. 客户端连接后，框架自动完成 CMPP 协议握手（Connect / ConnectResp）
3. 客户端发送 Submit 时，`BusinessHandler.on_inbound()` 收到并处理
4. 业务方在 `on_inbound` 中解码 Submit、构造 SubmitResp 并通过 `write_frame` 回复
5. 如果 Submit 带状态报告请求（`registered_delivery=1`），通过 `MessageSource` 异步推送 Report

## 代码结构

### AuthHandler -- CmppAuthHandler

负责 MD5 认证。从 `AuthCredentials::Cmpp` 中提取 `source_addr`、`authenticator_source` 和 `timestamp`，用 `compute_connect_auth()` 计算期望值并比对。认证成功返回 `AuthResult::success(source_addr)`，失败返回具体错误码。

### BusinessHandler -- CmppBusinessHandler

处理客户端上行消息的核心逻辑。收到 Submit 后：

- 解码消息内容，记录日志
- 构造 `SubmitResp` 并通过 `ctx.conn.write_frame()` 回复客户端
- 若 `tpudhi == 1`，通过 `UdhParser::extract_udh()` 提取 UDH 头信息，构造 `LongMessageFrame` 并交给 `LongMessageMerger` 进行合包缓存
- 若 `registered_delivery == 1`，构造 Report 类型的 Deliver PDU 并推送到 `MessageSource` 队列

框架不自动回 SubmitResp，完全由业务方控制。

### MessageSource -- FileMessageSource

内存消息队列，按账号隔离。实现了 `MessageSource` trait 的 `fetch(account, batch_size)` 方法。框架的 `run_outbound_fetcher` 循环调用 `fetch` 取出消息并通过 `write_frame` 发出。支持两种消息来源：

- 启动时从 `messages.conf` 加载的预定义 MO 消息
- 运行时 `BusinessHandler` 动态推送的 Report

### ServerEventHandler -- CmppServerEventHandler

服务端事件回调，记录连接、断开、认证成功等事件日志，用于监控和调试。

### AccountConfigProvider -- SimpleAccountConfigProvider

提供账号级配置（最大连接数、QPS 限制等），当前使用统一默认值：最大 10 连接、5000 QPS。

## 关键代码说明

### on_inbound 中回复 SubmitResp

```rust
// 构造 SubmitResp
let resp = SubmitResp {
    msg_id: *msg_id,
    result: 0,
};
let resp_pdu: Pdu = resp.into();
ctx.conn.write_frame(resp_pdu.to_pdu_bytes(sequence_id).as_bytes_ref()).await?;
```

### on_inbound 中推送 Report

```rust
if registered_delivery == 1 {
    if let Some(account) = ctx.conn.authenticated_account().await {
        let report = build_deliver_report(&account, msg_id, phone);
        self.msg_source.push(&account, report).await;
    }
}
```

Report 以 Deliver PDU（`registered_delivery=1`）的形式推送，内容包含 `MsgId`、`Stat`、`SubmitTime`、`DoneTime` 等字段。

## 长短信

### MO 合包（接收端）

当客户端发送的 Submit 中 `tpudhi == 1` 时，表示消息内容包含 UDH（User Data Header），属于长短信分段。服务端处理流程：

1. 通过 `UdhParser::extract_udh(&msg_content)` 从消息内容中提取 UDH 头，获取 `reference_id`（分片组标识）、`total_segments`（总分段数）、`segment_number`（当前分段序号）
2. 将 UDH 信息封装为 `LongMessageFrame`，调用 `LongMessageMerger::add_frame()` 缓存分段
3. `add_frame()` 返回 `Ok(Some(complete))` 表示所有分段收齐，`complete` 即为合包后的完整消息内容；返回 `Ok(None)` 表示仍有分段未到达

```rust
if tpudhi == 1 {
    if let Some((udh, _)) = UdhParser::extract_udh(msg_content) {
        let frame = LongMessageFrame::new(
            udh.reference_id, udh.total_segments, udh.segment_number,
            msg_content.to_vec(), true, Some(udh),
        );
        match merger.add_frame(frame) {
            Ok(Some(complete)) => { /* 合包完成 */ }
            Ok(None) => { /* 等待更多分段 */ }
            Err(e) => { /* 错误处理 */ }
        }
    }
}
```

关键类：`LongMessageMerger`（分段缓存与合包）、`UdhParser`（UDH 解析）、`LongMessageFrame`（分帧数据结构）。

`LongMessageMerger` 内部使用 `HashMap<String, PendingEntry>` 按 `reference_id-total_segments` 分组缓存，支持 TTL 过期清理（默认 60 秒），自动去重。

### MO 下发（发送端）

`FileMessageSource` 的 `load_from_file` 方法加载预定义 MO 消息时，会自动检测消息长度进行长短信拆分：

1. 根据 `detect_alphabet()` 判断编码类型：纯 ASCII 按 160 字节为单条上限，UCS2 按 70 字节
2. 超过单条上限时，调用 `LongMessageSplitter::split()` 将内容拆分为多个分段，每段自动添加 UDH 头（含 `reference_id`、`total_segments`、`segment_number`）
3. 将同一组分段封装为 `MessageItem::Group { items }`，框架保证同组分段走同一连接、按顺序发送
4. 每个 Deliver PDU 的 `tpudhi` 设置为 1，表示内容包含 UDH 头

```rust
if content_bytes.len() > single_max {
    let frames = splitter.split(content_bytes, alphabet);
    let mut items = Vec::new();
    for frame in frames {
        let pdu = build_deliver_mo_with_udh(&account, &phone, &frame.content, frame.has_udhi);
        items.push(Arc::new(pdu) as Arc<dyn EncodedPdu>);
    }
    source.push_group_sync(&account, MessageItem::Group { items });
}
```

`MessageItem::Single` 用于普通短消息，`MessageItem::Group` 用于长短信分段组。

## 依赖 crate

| crate | 用途 |
|-------|------|
| rsms-core | 核心类型（EndpointConfig、Frame、RawPdu） |
| rsms-connector | 服务端框架（serve、MessageSource、AuthHandler） |
| rsms-business | BusinessHandler trait |
| rsms-codec-cmpp | CMPP 协议编解码（compute_connect_auth、decode_message、Pdu） |
| rsms-longmsg | 长短信拆分/合包（LongMessageSplitter、LongMessageMerger、UdhParser） |
