/// 连接级会话状态，对齐 SMSGate `SessionState`（Connect / DisConnect）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SessionState {
    /// 已断开或尚未完成 pipeline
    Disconnected = 0,
    /// 编解码与业务链就绪，可参与 `fetch`
    Connected = 1,
    /// 认证成功
    Logined = 2,
}
