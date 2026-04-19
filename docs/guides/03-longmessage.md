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

let splitter = LongMessageSplitter::new();
let frames: Vec<LongMessageFrame> = splitter.split(content, SmsAlphabet::ASCII);

for frame in &frames {
    // frame.udh_bytes() - UDH 头部字节
    // frame.payload() - 分段 payload
    // frame.segment_total - 总段数
    // frame.segment_number - 当前段序号（从 1 开始）
    // frame.reference_number - 分段组参考号
}
```

### LongMessageMerger - 合包

```rust
use rsms_longmsg::LongMessageMerger;

let mut merger = LongMessageMerger::new();
merger.add_segment(reference_number, segment_number, segment_total, payload);

if merger.is_complete(reference_number, segment_total) {
    let full_content = merger.get_merged(reference_number).unwrap();
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
// 在 BusinessHandler::on_inbound 中
let version = conn.protocol_version().await.unwrap_or(0x30);
let message = decode_message_with_version(pdu, Some(version))?;

match message {
    CmppMessage::Submit { submit, .. } => {
        if submit.pk_total > 1 {
            // 长短信分段
            merger.add_segment(ref, submit.pk_number, submit.pk_total, &submit.msg_content);
            if merger.is_complete(ref, submit.pk_total) {
                let full = merger.get_merged(ref).unwrap();
                // 处理完整长短信
            }
        }
    }
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

## 参考测试

| 协议 | 测试文件 | 测试数 |
|------|----------|--------|
| CMPP | `examples/cmpp-endpoint/tests/cmpp_longmsg_test.rs` | 11（V2.0 + V3.0） |
| SMGP | `examples/smgp-endpoint/tests/smgp_longmsg_test.rs` | 7 |
| SMPP | `examples/smpp-endpoint/tests/smpp_longmsg_test.rs` | 11（V3.4 + V5.0） |
| SGIP | `examples/sgip-endpoint/tests/sgip_longmsg_test.rs` | 7 |
