//! Pipeline error types

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PipelineError {
    #[error("Decoding error: {0}")]
    Decoding(String),

    #[error("Encoding error: {0}")]
    Encoding(String),

    #[error("Session error: {0}")]
    Session(String),

    #[error("Not logged in")]
    NotLogined,

    #[error("Invalid session state: {0}")]
    InvalidState(String),

    #[error("Rate limited: {0}")]
    RateLimited(String),

    #[error("Handler error: {0}")]
    Handler(String),

    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error("Service unavailable")]
    Unavailable,

    #[error("Connection closed")]
    ConnectionClosed,

    #[error("Timeout")]
    Timeout,
}

pub type PipelineResult<T> = Result<T, PipelineError>;
