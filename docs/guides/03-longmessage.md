# 长短信处理

## 概述

长短信（超过 140 字节 / 70 个 Unicode 字符）需要拆分为多段发送，接收端合包还原。框架通过 `rsms-longmsg` crate 提供拆分/合包能力，通过 `MessageItem::Group` 保证同组帧走同一连接。

## MessageItem

```rust
pub enum MessageItem {
    // 普通短消息
    Single(Arc<dyn EncodedPdu>),

    // 长短信分段组（框架保证同组帧走同一连接、顺序发出）
    Group { items: Vec<Arc<dyn EncodedPdu>> },
}
```

## rsms-longmsg

### LongMessageSplitter - 拆分

```rust
use rsms_longmsg::{LongMessageSplitter, SmsAlphabet, LongMessageFrame};

let mut splitter = LongMessageSplitter::new();
// content 必须是“线路字节”（UCS2 需先转 UTF-16BE），split 按字节上限拆段
let frames: Vec<LongMessageFrame> = splitter.split(&content, SmsAlphabet::UCS2);

for frame in &frames {
    frame.reference_id;    // u16：同组分段相同的级联参考号
    frame.total_segments;  // u8：总段数
    frame.segment_number;  // u8：当前段序号（从 1 开始）
    &frame.content;        // Vec<u8>：含 UDH 头的分段字节
    frame.has_udhi;        // bool：是否带 UDH（未超长的单段消息为 false）
}
// 窄腰用法：通常把每个 frame 转成 (Concat, 纯载荷) 交给各协议 Adapter 重建 UDH，
// 业务不直接拼 UDH 字节（参见 examples/*_client 的 frame_to_concat）。
```

### LongMessageMerger - 合包

```rust
use rsms_longmsg::{LongMessageMerger, LongMessageFrame};

let mut merger = LongMessageMerger::new(); // 默认 60s TTL，超时未收齐的分片自动回收（防 OOM）

// 据收到的分段重建帧（reference_id/total/seq + 含 UDH 的分段字节）
let frame = LongMessageFrame::new(reference_id, total_segments, segment_number, content, true, None);

// ⚠️ 关键：第一个参数是发送方标识（入站 MO 即原始终端号）。merger 按发送方分桶，
// 不同发送方即便 reference 撞号也不会串合。详见下文「拼接正确性与串号防护」。
match merger.add_frame(sender, frame)? {
    Some(full_content) => { /* 已收齐：full_content 是去 UDH 后按序拼接的完整内容 */ }
    None => { /* 仍在等待后续分段；或收到重复段被忽略 */ }
}
```

## UDH 格式

框架使用 GSM 03.40 标准的 UDH（User Data Header）格式：

### 8-bit 参考号

```
Offset  Value   说明
0       0x05    UDHL（UDH 长度，固定 5）
1       0x00    IEI（Information Element Identifier = Concatenated SMS 8-bit）
2       0x03    IEDL（IED 长度，固定 3）
3       ref     参考号（u8，同组分段相同）
4       total   总段数
5       seq     当前段序号（从 1 开始）
```

总长度：6 字节

### 16-bit 参考号

```
Offset  Value   说明
0       0x06    UDHL（UDH 长度，固定 6）
1       0x08    IEI（Information Element Identifier = Concatenated SMS 16-bit）
2       0x04    IEDL（IED 长度，固定 4）
3-4     ref     参考号（u16，BE）
5       total   总段数
6       seq     当前段序号（从 1 开始）
```

总长度：7 字节

## 各协议长短信字段差异

| 特性 | CMPP | SMGP | SMPP | SGIP |
|------|------|------|------|------|
| UDH 标志字段 | `tpudhi` 固定字段 | TLV `TP_UDHI`(0x0002) | `esm_class & 0x40` | `tpudhi` 固定字段 |
| 分段信息 | `pk_total` / `pk_number` 固定字段 | TLV `PK_TOTAL` / `PK_NUMBER` | UDH 头部 | UDH 头部 |

## 使用方式

### 服务端：接收长短信

```rust
// 在 BusinessHandler::on_inbound 中（窄腰：Adapter 已把级联 UDH 剥进 submit.concat、
// content 为纯载荷）。据 concat 重建含 UDH 的分段帧喂 merger：
if let Some(c) = submit.concat {
    let mut seg = c.to_udh_prefix();          // 据 concat 重建 UDH 头
    seg.extend_from_slice(&submit.content);    // + 纯载荷
    let frame = LongMessageFrame::new(c.reference, c.total, c.sequence, seg, true, None);
    // sender 用目标手机号 phone（区分发往不同号码的并发长短信；客户端收 MO 则用原始终端号 src）
    if let Some(full) = merger.add_frame(&phone, frame)? {
        // 已收齐：full 为完整内容，按 submit.encoding 解码为文本即可
    }
} else {
    // 单段：submit.content 即完整内容
}
```

### 客户端：发送长短信

```rust
let splitter = LongMessageSplitter::new();
let frames = splitter.split(content, SmsAlphabet::UCS2);

let mut items = Vec::new();
for frame in &frames {
    let pdu = build_segment_pdu(frame);  // 构造各协议的 Submit PDU
    items.push(Arc::new(RawPdu::from_vec(pdu)) as Arc<dyn EncodedPdu>);
}

// 使用 MessageItem::Group 发送，框架保证同组帧走同一连接
let message = MessageItem::Group { items };
```

### 通过 MessageSource 批量推送

```rust
// push_group 保证同组帧走同一连接
msg_source.push_group("900001", items).await;
```

## 拼接正确性与串号防护

长短信靠 (发送方, reference, total) 在接收端分桶重组。reference 仅 16-bit，并发场景下有撞号风险，使用方须注意三点：

### 1. 入站合包：必须按发送方分桶（强约束）

`LongMessageMerger::add_frame(sender, frame)` 的**第一个参数是发送方标识**，框架据此分桶。
**绝不要对所有发送方共用一个空 sender**：不同手机各自的 reference 从小值起、极易撞号，
若不按发送方区分，两条长短信的同号分段会串入同一组——第二条的分段被当重复丢弃、内容串台。

- 服务端收 MT：传**目标手机号** `phone`（区分发往不同号码的并发长短信）；
- 客户端收 MO：传**原始终端号** `src`（区分不同手机发来的长短信）。

> merger 还内置 60s TTL，超时未收齐的分片自动回收，避免丢段导致内存无界。

### 2. 出站 reference：建议每账号一个持久生成器

`LongMessageSplitter::new()` 内部各持一个 `ReferenceIdGenerator`。框架已保证「每条长短信新建一个
splitter」时各生成器**起始 reference 互不相同**（进程级种子分发）。但要让同账号的 reference 长期单调、
零撞号，**推荐每账号持有一个持久生成器**并复用：

```rust
use rsms_longmsg::{LongMessageSplitter, ReferenceIdGenerator};
use std::sync::Arc;

// 每账号建一次，长期复用（而非每条长短信都 new）
let gen = Arc::new(ReferenceIdGenerator::new());
let mut splitter = LongMessageSplitter::with_generator(gen.clone());
```

### 3. sequence_id：同账号多连接须账号内唯一

请求/响应匹配的滑动窗口按连接隔离，**但交付回调链路的 `TransactionManager` 按账号共享、以 sequence_id
为键**。因此同账号多连接场景下 sequence_id 必须在账号内唯一，否则两连接用相同 seq 会互相覆盖事务、
导致回执/回调错配。框架已提供正解——同账号多连接**共用一个 `IdGenerator`**，取值即天然唯一：

```rust
let seq = account_connections.id_generator().next_sequence_id();
```

## 参考测试

| 协议 | 测试文件（统一在 `rsms-tests` 包） | 测试数 |
|------|----------|--------|
| CMPP | `tests/cmpp/cmpp_longmsg_test.rs` | 11（V2.0 + V3.0） |
| SMGP | `tests/smgp/smgp_longmsg_test.rs` | 7 |
| SMPP | `tests/smpp/smpp_longmsg_test.rs` | 11（V3.4 + V5.0） |
| SGIP | `tests/sgip/sgip_longmsg_test.rs` | 7 |

运行：`cargo test -p rsms-tests --test cmpp-longmsg-test -- --nocapture`（各协议 `<proto>-longmsg-test`）。
