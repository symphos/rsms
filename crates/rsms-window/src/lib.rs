//! # rsms-window
//! 
//! Sliding window implementation for async request/response handling.

pub mod error;
pub mod future;
pub mod window;

pub use error::{WindowError, WindowResult};
pub use future::WindowFuture;
pub use window::{Window, WindowConfig, SimpleWindowShutdownHandle};