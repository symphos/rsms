//! 交付链路自动驱动：把读循环收到的帧 / 出站发出的帧自动接到 [`TransactionManager`]，
//! 用户只需 `impl MessageCallback` 即可获得 Submit→Resp→Report→MO 全链路 + 超时/失败回调，
//! 无需在自己的 handler 里手工解析帧再手工调 TM。
//!
//! 仅在连接配置了 TM（即用户设置了 `MessageCallback`）时调用；未启用时这两个函数不会被触及，
//! 因此对“不关心交付结果”的高吞吐路径零成本。

use std::time::{Duration, Instant};

use rsms_codec_cmpp::CommandId as CmppCommandId;
use rsms_codec_sgip::CommandId as SgipCommandId;
use rsms_codec_smgp::CommandId as SmgpCommandId;
use rsms_codec_smpp::CommandId as SmppCommandId;
use rsms_core::{Frame, Protocol};
use rsms_model::{MessageId, ProtocolAdapter as _, UnifiedMessage};

use super::{MoInfo, ReportInfo, SubmitInfo, TransactionManager};
use crate::adapter_registry::adapter_for;

/// 统一 `MessageId` → `String`。Resp 与 Report 两个方向**必须共用此函数**，
/// 否则按 msg_id 关联会失败（Binary 形态须产生同一十六进制串）。
pub(crate) fn msg_id_to_string(id: &MessageId) -> String {
    match id {
        MessageId::Text(s) => s.clone(),
        MessageId::Binary(b) => b.iter().map(|x| format!("{x:02x}")).collect(),
    }
}

/// 出站 command_id 是否为该协议的 Submit（发侧登记只需识别 Submit，不必整包解码，
/// 从而避开 CMPP V2.0/V3.0 版本感知解码的脆弱性）。
fn is_submit(protocol: Protocol, command_id: u32) -> bool {
    match protocol {
        Protocol::Cmpp => command_id == CmppCommandId::Submit as u32,
        Protocol::Smgp => command_id == SmgpCommandId::Submit as u32,
        Protocol::Smpp => command_id == SmppCommandId::SUBMIT_SM as u32,
        Protocol::Sgip => command_id == SgipCommandId::Submit as u32,
    }
}

/// 收侧自动驱动：一帧经 adapter 解码后路由到 TM 的对应回调。
/// 只识别 SubmitResp/Report/Deliver(MO)，其余（BindResp/Ping/...）忽略。
/// 解码失败静默跳过——与 `unified-shadow` 同源解码，错误隔离不影响主收包路径。
pub(crate) async fn drive_inbound(tm: &TransactionManager, protocol: Protocol, frame: &Frame) {
    let Ok(msg) = adapter_for(protocol).decode(frame) else {
        return;
    };
    match msg {
        UnifiedMessage::SubmitResp(r) => {
            tm.on_submit_resp(frame.sequence_id, Some(msg_id_to_string(&r.msg_id)), r.status)
                .await;
        }
        UnifiedMessage::Report(rep) => {
            tm.on_report(ReportInfo {
                msg_id: msg_id_to_string(&rep.msg_id),
                src_terminal_id: rep.src.number,
                stat: format!("{:?}", rep.status),
            })
            .await;
        }
        UnifiedMessage::Deliver(d) => {
            tm.on_mo(MoInfo {
                dest_id: d.dest.number,
                src_terminal_id: d.src.number,
                content: d.content,
            })
            .await;
        }
        _ => {}
    }
}

/// 发侧自动登记：出站 PDU 若为该协议的 Submit，则按 sequence_id 登记到 TM。
///
/// `content` 存**原始 PDU 字节**，便于超时后原样重发（at-least-once 的基础）；
/// 业务号码（src/dest）留空——业务侧按 sequence_id 关联自有映射，框架只负责
/// seq↔msg_id 关联、报告匹配与超时。返回是否已登记（供测试与计数）。
pub(crate) async fn register_outbound(tm: &TransactionManager, protocol: Protocol, raw: &[u8]) -> bool {
    let seq_offset = protocol.seq_offset();
    if raw.len() < 8 || raw.len() < seq_offset + 4 {
        return false;
    }
    let command_id = u32::from_be_bytes([raw[4], raw[5], raw[6], raw[7]]);
    if !is_submit(protocol, command_id) {
        return false;
    }
    let sequence_id = u32::from_be_bytes([
        raw[seq_offset],
        raw[seq_offset + 1],
        raw[seq_offset + 2],
        raw[seq_offset + 3],
    ]);
    tm.add_submit_transaction(SubmitInfo::new(
        sequence_id,
        String::new(),
        String::new(),
        raw.to_vec(),
        protocol.as_str(),
    ))
    .await;
    true
}

/// 优雅停机 drain 原语：轮询等待 TM 在途 Submit（Pending，即已发未收 Resp）降为 0。
/// 返回 `true` 表示已全部收到 Resp；`false` 表示在 `timeout` 内未追平（超时）。
/// 客户端停机时先停发、再调用本函数等在途 Resp 追平，最后才断连，实现零丢。
pub(crate) async fn drain_pending(tm: &TransactionManager, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if tm.pending_count().await == 0 {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transaction::MessageCallback;
    use rsms_core::RawPdu;
    use rsms_model::{
        Address, CmppExtra, DeliveryStatus, Encoding, ProtocolExtra, Sequence, UnifiedReport,
        UnifiedSubmit, UnifiedSubmitResp,
    };
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct Spy {
        resp_ok: AtomicUsize,
        resp_err: AtomicUsize,
        reports: AtomicUsize,
        mos: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl MessageCallback for Spy {
        async fn on_submit_resp_ok(&self, _: &SubmitInfo) {
            self.resp_ok.fetch_add(1, Ordering::Relaxed);
        }
        async fn on_submit_resp_error(&self, _: &SubmitInfo, _: u32) {
            self.resp_err.fetch_add(1, Ordering::Relaxed);
        }
        async fn on_submit_resp_timeout(&self, _: &SubmitInfo) {}
        async fn on_send_failed(&self, _: u32, _: &str) {}
        async fn on_report(&self, _: &ReportInfo, _: &SubmitInfo) {
            self.reports.fetch_add(1, Ordering::Relaxed);
        }
        async fn on_mo(&self, _: &MoInfo) {
            self.mos.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 用 adapter encode 造真实 PDU 字节，再封装成读循环看到的 `Frame`（data 含整包，含 12B 头）。
    fn frame_of(raw: Vec<u8>) -> Frame {
        let cmd = u32::from_be_bytes([raw[4], raw[5], raw[6], raw[7]]);
        let seq = u32::from_be_bytes([raw[8], raw[9], raw[10], raw[11]]);
        Frame::new(cmd, seq, RawPdu::from_vec(raw))
    }

    fn cmpp_encode(msg: &UnifiedMessage, seq: u32) -> Vec<u8> {
        adapter_for(Protocol::Cmpp)
            .encode(msg, Sequence::Plain(seq))
            .expect("cmpp encode")
    }

    const MID: [u8; 8] = [0, 0, 0, 0, 0, 0, 0, 9];

    fn cmpp_submit(seq: u32) -> Vec<u8> {
        cmpp_encode(
            &UnifiedMessage::Submit(UnifiedSubmit {
                src: Address::plain("1065900000"),
                dests: vec![Address::plain("13800138000")],
                content: b"hi".to_vec(),
                encoding: Encoding::Gbk,
                want_report: true,
                concat: None,
                extra: ProtocolExtra::Cmpp(CmppExtra::default()),
                tlvs: vec![],
            }),
            seq,
        )
    }

    /// 发侧：Submit 应被登记；非 Submit（如 SubmitResp）不应被登记。
    #[tokio::test]
    async fn register_outbound_only_registers_submit() {
        let tm = TransactionManager::new(10);
        assert!(register_outbound(&tm, Protocol::Cmpp, &cmpp_submit(7)).await);
        assert_eq!(tm.transaction_count().await, 1);

        let resp = cmpp_encode(
            &UnifiedMessage::SubmitResp(UnifiedSubmitResp {
                msg_id: MessageId::Binary(MID.to_vec()),
                status: 0,
            }),
            7,
        );
        assert!(!register_outbound(&tm, Protocol::Cmpp, &resp).await, "SubmitResp 不应被登记");
        assert_eq!(tm.transaction_count().await, 1);
    }

    /// 收侧：登记 Submit 后，收到 SubmitResp 与 Report 应自动驱动对应回调，且报告匹配后清理。
    #[tokio::test]
    async fn inbound_drives_resp_then_report() {
        let tm = TransactionManager::new(10);
        let spy = Arc::new(Spy::default());
        tm.set_callback(Some(spy.clone() as Arc<dyn MessageCallback>)).await;

        register_outbound(&tm, Protocol::Cmpp, &cmpp_submit(7)).await;

        let resp = cmpp_encode(
            &UnifiedMessage::SubmitResp(UnifiedSubmitResp {
                msg_id: MessageId::Binary(MID.to_vec()),
                status: 0,
            }),
            7,
        );
        drive_inbound(&tm, Protocol::Cmpp, &frame_of(resp)).await;
        assert_eq!(spy.resp_ok.load(Ordering::Relaxed), 1, "SubmitResp(ok) 应触发 on_submit_resp_ok");

        let report = cmpp_encode(
            &UnifiedMessage::Report(UnifiedReport {
                msg_id: MessageId::Binary(MID.to_vec()),
                status: DeliveryStatus::Delivered,
                src: Address::plain("13800138000"),
                dest: Address::plain("1065900000"),
                raw: vec![],
            }),
            99,
        );
        drive_inbound(&tm, Protocol::Cmpp, &frame_of(report)).await;
        assert_eq!(spy.reports.load(Ordering::Relaxed), 1, "Report 应按 msg_id 关联到原 Submit");
        assert_eq!(tm.transaction_count().await, 0, "报告匹配后应清理事务条目");
    }

    /// drain：有未收 Resp 的在途 Submit 时超时返回 false；收到 Resp 追平后快速返回 true。
    #[tokio::test]
    async fn drain_pending_times_out_then_drains() {
        let tm = TransactionManager::new(10);
        register_outbound(&tm, Protocol::Cmpp, &cmpp_submit(7)).await; // pending=1
        assert!(
            !drain_pending(&tm, Duration::from_millis(80)).await,
            "未收 Resp 时应超时返回 false"
        );

        let resp = cmpp_encode(
            &UnifiedMessage::SubmitResp(UnifiedSubmitResp {
                msg_id: MessageId::Binary(MID.to_vec()),
                status: 0,
            }),
            7,
        );
        drive_inbound(&tm, Protocol::Cmpp, &frame_of(resp)).await; // pending=0
        assert!(
            drain_pending(&tm, Duration::from_millis(200)).await,
            "Resp 追平后应返回 true"
        );
    }

    /// 收侧：status!=0 的 SubmitResp 应走 error 回调。
    #[tokio::test]
    async fn inbound_resp_error_path() {
        let tm = TransactionManager::new(10);
        let spy = Arc::new(Spy::default());
        tm.set_callback(Some(spy.clone() as Arc<dyn MessageCallback>)).await;

        register_outbound(&tm, Protocol::Cmpp, &cmpp_submit(5)).await;
        let resp = cmpp_encode(
            &UnifiedMessage::SubmitResp(UnifiedSubmitResp {
                msg_id: MessageId::Binary(MID.to_vec()),
                status: 9,
            }),
            5,
        );
        drive_inbound(&tm, Protocol::Cmpp, &frame_of(resp)).await;
        assert_eq!(spy.resp_err.load(Ordering::Relaxed), 1);
        assert_eq!(spy.resp_ok.load(Ordering::Relaxed), 0);
    }
}
