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
