//! Window error types

use thiserror::Error;

#[derive(Debug, Error, Clone)]
pub enum WindowError {
    #[error("Window is full, cannot offer new request")]
    WindowFull,

    #[error("Offer timeout - waited too long for available slot")]
    OfferTimeout,

    #[error("Request timeout - no response received within timeout period")]
    Timeout,

    #[error("Request cancelled")]
    Cancelled,

    #[error("Key already exists: {0}")]
    DuplicateKey(String),

    #[error("Key not found: {0}")]
    NotFound(String),

    #[error("Window closed")]
    Closed,

    #[error("Internal error: {0}")]
    Internal(String),
}

pub type WindowResult<T> = Result<T, WindowError>;
