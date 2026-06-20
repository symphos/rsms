//! 可观测性指标钩子。
//!
//! 框架在连接生命周期与帧处理的关键点调用 [`Metrics`] 的方法；调用方实现该 trait，把事件
//! 桥接到自己的指标后端（Prometheus / OpenTelemetry / 日志等）。所有方法均为**同步、no-op
//! 默认**——只实现关心的指标即可，且不得阻塞（应只做计数/原子更新等廉价操作，处于热路径上）。
//!
//! 与 `ServerEventHandler` 等异步事件回调不同：事件回调面向"业务动作"，本 trait 面向"度量"，
//! 故为同步且零成本默认，避免污染收发热路径。

use crate::Protocol;

/// 指标记录器：实现并经 `ServerBuilder::metrics` 注入，框架在关键点回调。
///
/// 所有方法默认空实现，可按需覆写。实现须 `Send + Sync` 且廉价非阻塞。
pub trait Metrics: Send + Sync {
    /// 新连接建立（服务端 accept 并启动连接处理时）。
    fn connection_opened(&self) {}

    /// 连接关闭（连接处理循环结束、注销时）。
    fn connection_closed(&self) {}

    /// 连接认证成功并注册到账号池。
    fn connection_authenticated(&self, account: &str) {
        let _ = account;
    }

    /// 成功解码一帧入站 PDU（`command_id` 为协议命令字）。
    fn inbound_frame(&self, protocol: Protocol, command_id: u32) {
        let _ = (protocol, command_id);
    }

    /// 入站帧解码失败（坏帧等，连接将被关闭）。
    fn decode_error(&self, protocol: Protocol) {
        let _ = protocol;
    }
}

/// 不记录任何指标的默认实现（框架未注入 `Metrics` 时使用）。
pub struct NoopMetrics;

impl Metrics for NoopMetrics {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[derive(Default)]
    struct CountingMetrics {
        opened: AtomicU64,
        closed: AtomicU64,
        inbound: AtomicU64,
        decode_err: AtomicU64,
        authed: AtomicU64,
    }

    impl Metrics for CountingMetrics {
        fn connection_opened(&self) {
            self.opened.fetch_add(1, Ordering::Relaxed);
        }
        fn connection_closed(&self) {
            self.closed.fetch_add(1, Ordering::Relaxed);
        }
        fn connection_authenticated(&self, _account: &str) {
            self.authed.fetch_add(1, Ordering::Relaxed);
        }
        fn inbound_frame(&self, _protocol: Protocol, _command_id: u32) {
            self.inbound.fetch_add(1, Ordering::Relaxed);
        }
        fn decode_error(&self, _protocol: Protocol) {
            self.decode_err.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn noop_metrics_is_inert() {
        // NoopMetrics 用默认空实现，调用任意方法不应 panic、无副作用。
        let m = NoopMetrics;
        m.connection_opened();
        m.connection_authenticated("900001");
        m.inbound_frame(Protocol::Cmpp, 4);
        m.decode_error(Protocol::Smpp);
        m.connection_closed();
    }

    #[test]
    fn counting_metrics_only_overrides_what_it_wants() {
        let m = CountingMetrics::default();
        m.connection_opened();
        m.inbound_frame(Protocol::Cmpp, 4);
        m.inbound_frame(Protocol::Cmpp, 4);
        m.connection_authenticated("900001");
        m.connection_closed();
        assert_eq!(m.opened.load(Ordering::Relaxed), 1);
        assert_eq!(m.inbound.load(Ordering::Relaxed), 2);
        assert_eq!(m.authed.load(Ordering::Relaxed), 1);
        assert_eq!(m.closed.load(Ordering::Relaxed), 1);
        assert_eq!(m.decode_err.load(Ordering::Relaxed), 0);
    }
}
