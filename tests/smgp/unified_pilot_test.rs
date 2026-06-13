//! P3 窄腰试点验证：业务侧通过 UnifiedMessage 读取 SMGP Submit，无需感知 SMGP 字段。

use rsms_codec_smgp::adapter::SmgpAdapter;
use rsms_codec_smgp::codec::Pdu;
use rsms_codec_smgp::datatypes::Submit;
use rsms_core::{Frame, RawPdu};
use rsms_model::{ProtocolAdapter, UnifiedMessage};

/// 验证：把一段 SMGP Submit 字节通过 SmgpAdapter 解码为 UnifiedMessage::Submit，
/// 业务侧完全无需了解 SMGP 字段即可读出 src/dest/content/encoding/want_report。
#[test]
fn business_reads_unified_submit_without_smgp_knowledge() {
    let submit = Submit::new().with_message("1065900000", "13800138000", b"Hi");
    let bytes = Pdu::from(submit).to_pdu_bytes(1).to_vec();
    // Frame::new(command_id, sequence_id, raw) — adapter 内部重新解码，头部字段不影响结果
    let frame = Frame::new(0, 0, RawPdu::from_vec(bytes));

    let unified = SmgpAdapter.decode(&frame).unwrap();

    // 业务侧代码完全协议无关：只依赖 UnifiedMessage，不碰任何 SMGP 结构体
    if let UnifiedMessage::Submit(s) = unified {
        assert_eq!(s.src.number, "1065900000", "发送方号码应被正确提取");
        assert_eq!(s.dests.len(), 1, "应有一个目标号码");
        assert_eq!(s.dests[0].number, "13800138000", "目标号码应被正确提取");
        assert_eq!(s.content, b"Hi", "消息内容应被原样保留");
        // want_report 字段可读取，无需了解 SMGP need_report 语义
        let _ = s.want_report;
    } else {
        panic!("期望 UnifiedMessage::Submit，实际得到其他变体");
    }
}
