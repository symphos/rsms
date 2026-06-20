use thiserror::Error;

#[derive(Debug, Error)]
pub enum RsmsError {
    #[error("IO: {0}")]
    Io(#[from] std::io::Error),

    #[error("codec: {0}")]
    Codec(String),

    #[error("endpoint closed")]
    EndpointClosed,

    #[error("no ready connection for fetch")]
    NoReadyConnection,

    #[error("max channels reached ({0})")]
    MaxChannels(u16),

    #[error("sync request timed out")]
    SyncTimeout,

    #[error("sync response channel closed")]
    SyncClosed,

    #[error("not implemented: {0}")]
    NotImplemented(&'static str),

    #[error("other: {0}")]
    Other(String),

    #[error("connection closed: {0}")]
    ConnectionClosed(String),

    #[error("rate limited: {0}")]
    RateLimited(String),
}

pub type Result<T> = std::result::Result<T, RsmsError>;

/// 错误粗分类:供上层一行决定**重试 / 重连 / 丢弃**,无需逐个 match `RsmsError` 变体。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// 数据不完整,需更多字节(半包)。当前帧层自管粘包,业务层一般不产生。
    Incomplete,
    /// PDU 格式非法 / 解码失败 —— 该帧不可恢复,应丢弃而非重试。
    Malformed,
    /// 被限流拒绝 —— 退避后可重试。
    RateLimited,
    /// 请求超时 —— 可重试。
    Timeout,
    /// 连接已断开 —— 重连后可重试(配合重投)。
    Closed,
    /// 传输 / IO 错误 —— 通常重连后可重试。
    Transport,
    /// 容量受限(无可用连接 / 连接数满)—— 背压,稍后可重试。
    Capacity,
    /// 端点已关闭 —— 终态,不可重试。
    Terminal,
    /// 其它 / 未分类 —— 默认不重试(保守)。
    Other,
}

impl RsmsError {
    /// 返回该错误的粗分类。
    pub fn kind(&self) -> ErrorKind {
        match self {
            RsmsError::Io(_) => ErrorKind::Transport,
            RsmsError::Codec(_) => ErrorKind::Malformed,
            RsmsError::EndpointClosed => ErrorKind::Terminal,
            RsmsError::NoReadyConnection => ErrorKind::Capacity,
            RsmsError::MaxChannels(_) => ErrorKind::Capacity,
            RsmsError::SyncTimeout => ErrorKind::Timeout,
            RsmsError::SyncClosed => ErrorKind::Closed,
            RsmsError::NotImplemented(_) => ErrorKind::Other,
            RsmsError::Other(_) => ErrorKind::Other,
            RsmsError::ConnectionClosed(_) => ErrorKind::Closed,
            RsmsError::RateLimited(_) => ErrorKind::RateLimited,
        }
    }

    /// 是否值得重试(重连 / 退避后再发)。坏帧、端点终态、未实现、未分类均不重试。
    pub fn is_retryable(&self) -> bool {
        matches!(
            self.kind(),
            ErrorKind::RateLimited
                | ErrorKind::Timeout
                | ErrorKind::Closed
                | ErrorKind::Transport
                | ErrorKind::Capacity
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_maps_each_variant() {
        use std::io::{Error as IoError, ErrorKind as IoKind};
        assert_eq!(RsmsError::Io(IoError::from(IoKind::BrokenPipe)).kind(), ErrorKind::Transport);
        assert_eq!(RsmsError::Codec("bad".into()).kind(), ErrorKind::Malformed);
        assert_eq!(RsmsError::EndpointClosed.kind(), ErrorKind::Terminal);
        assert_eq!(RsmsError::NoReadyConnection.kind(), ErrorKind::Capacity);
        assert_eq!(RsmsError::MaxChannels(500).kind(), ErrorKind::Capacity);
        assert_eq!(RsmsError::SyncTimeout.kind(), ErrorKind::Timeout);
        assert_eq!(RsmsError::SyncClosed.kind(), ErrorKind::Closed);
        assert_eq!(RsmsError::NotImplemented("x").kind(), ErrorKind::Other);
        assert_eq!(RsmsError::Other("x".into()).kind(), ErrorKind::Other);
        assert_eq!(RsmsError::ConnectionClosed("x".into()).kind(), ErrorKind::Closed);
        assert_eq!(RsmsError::RateLimited("x".into()).kind(), ErrorKind::RateLimited);
    }

    #[test]
    fn is_retryable_classification() {
        // 可重试：限流/超时/连接断开/传输/容量（重连或退避后可再试）。
        assert!(RsmsError::RateLimited("x".into()).is_retryable());
        assert!(RsmsError::SyncTimeout.is_retryable());
        assert!(RsmsError::ConnectionClosed("x".into()).is_retryable());
        assert!(RsmsError::SyncClosed.is_retryable());
        assert!(RsmsError::NoReadyConnection.is_retryable());
        assert!(RsmsError::MaxChannels(1).is_retryable());
        // 不可重试：坏帧（重试也不会变好）、端点终态、未实现、未分类。
        assert!(!RsmsError::Codec("bad".into()).is_retryable());
        assert!(!RsmsError::EndpointClosed.is_retryable());
        assert!(!RsmsError::NotImplemented("x").is_retryable());
        assert!(!RsmsError::Other("x".into()).is_retryable());
    }
}
