use rsms_core::{ConnectionInfo, EncodedPdu, Frame, Result};
use std::net::SocketAddr;
use std::sync::Arc;
use async_trait::async_trait;

pub const RESPONSE_COMMAND_MASK: u32 = 0x80000000;

#[async_trait]
pub trait SubmitLimiter: Send + Sync {
    async fn try_acquire_submit(&self) -> bool;
    async fn acquire_submit(&self, timeout: std::time::Duration) -> bool;
    async fn release_submit(&self);
}

pub trait FrameDecoder: Send + Sync {
    fn decode_frames(&self, buf: &mut Vec<u8>) -> Result<Vec<Frame>>;
}

impl<D: FrameDecoder> FrameDecoder for Box<D> {
    fn decode_frames(&self, buf: &mut Vec<u8>) -> Result<Vec<Frame>> {
        (**self).decode_frames(buf)
    }
}

#[async_trait]
pub trait ProtocolConnection: Send + Sync {
    fn id(&self) -> u64;
    async fn write_frame(&self, data: &[u8]) -> Result<()>;
    async fn set_authenticated_account(&self, account: String);
    async fn authenticated_account(&self) -> Option<String>;
    async fn submit_limiter(&self) -> Option<Arc<dyn SubmitLimiter>>;
    async fn protocol_version(&self) -> Option<u8>;
    async fn set_protocol_version(&self, version: u8);
    async fn replace_decoder(&self, decoder: Box<dyn FrameDecoder>);
    async fn peer_addr(&self) -> Option<SocketAddr>;
    async fn local_addr(&self) -> Option<SocketAddr>;
    async fn connection_info(&self) -> ConnectionInfo;

    fn remote_ip(&self) -> String;
    fn remote_port(&self) -> u16;
    fn should_log(&self, level: tracing::Level) -> bool;
}

#[async_trait]
pub trait ProtocolHandler: Send + Sync {
    fn name(&self) -> &'static str;
    async fn handle_frame(&self, frame: &Frame, conn: Arc<dyn ProtocolConnection>) -> Result<HandleResult>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleResult {
    Continue,
    Stop,
}

impl HandleResult {
    pub fn should_continue(&self) -> bool {
        matches!(self, HandleResult::Continue)
    }
}

#[async_trait]
pub trait AuthHandler: Send + Sync {
    fn name(&self) -> &'static str;

    async fn authenticate(
        &self,
        client_id: &str,
        credentials: AuthCredentials,
        conn_info: &ConnectionInfo,
    ) -> Result<AuthResult>;
}

#[derive(Debug, Clone)]
pub struct AuthResult {
    pub status: u32,
    pub account: String,
    pub message: Option<String>,
}

impl AuthResult {
    pub fn success(account: impl Into<String>) -> Self {
        Self {
            status: 0,
            account: account.into(),
            message: None,
        }
    }

    pub fn failure(status: u32, message: impl Into<String>) -> Self {
        Self {
            status,
            account: String::new(),
            message: Some(message.into()),
        }
    }
}

#[derive(Debug, Clone)]
pub enum AuthCredentials {
    Smgp { client_id: String, authenticator: [u8; 16], version: u8 },
    Sgip { login_name: String, login_password: String },
    Cmpp { source_addr: String, authenticator_source: [u8; 16], version: u8, timestamp: u32 },
    Smpp { system_id: String, password: String, interface_version: u8 },
}

#[derive(Debug, Clone, Default)]
pub struct AccountConfig {
    pub max_connections: u8,
    pub window_size: u16,
    pub max_fetch_threads: u8,
    pub fetch_interval_ms: u32,
    pub enabled: bool,
    pub max_qps: u64,
    pub window_size_ms: u64,
    pub submit_resp_timeout_secs: u64,
}

impl AccountConfig {
    pub fn new() -> Self {
        Self {
            max_connections: 1,
            window_size: 16,
            max_fetch_threads: 1,
            fetch_interval_ms: 500,
            enabled: true,
            max_qps: 100,
            window_size_ms: 1000,
            submit_resp_timeout_secs: 30,
        }
    }

    pub fn with_max_connections(mut self, max: u8) -> Self {
        self.max_connections = max;
        self
    }

    pub fn with_window_size(mut self, size: u16) -> Self {
        self.window_size = size;
        self
    }

    pub fn with_max_fetch_threads(mut self, threads: u8) -> Self {
        self.max_fetch_threads = threads;
        self
    }

    pub fn with_fetch_interval(mut self, interval_ms: u32) -> Self {
        self.fetch_interval_ms = interval_ms;
        self
    }

    pub fn with_max_qps(mut self, qps: u64) -> Self {
        self.max_qps = qps;
        self
    }

    pub fn with_window_size_ms(mut self, ms: u64) -> Self {
        self.window_size_ms = ms;
        self
    }
}

#[derive(Debug, Clone, Default)]
pub struct AccountPoolConfig {
    pub check_interval_ms: u32,
}

impl AccountPoolConfig {
    pub fn new() -> Self {
        Self {
            check_interval_ms: 5000,
        }
    }

    pub fn with_check_interval(mut self, interval_ms: u32) -> Self {
        self.check_interval_ms = interval_ms;
        self
    }
}

#[async_trait]
pub trait AccountConfigProvider: Send + Sync {
    async fn get_config(&self, account: &str) -> Result<AccountConfig>;
}

pub enum MessageItem {
    Single(Arc<dyn EncodedPdu>),
    Group { items: Vec<Arc<dyn EncodedPdu>> },
}

#[async_trait]
pub trait MessageSource: Send + Sync {
    async fn fetch(&self, account: &str, batch_size: usize) -> Result<Vec<MessageItem>>;
}

#[async_trait]
pub trait ServerEventHandler: Send + Sync {
    async fn on_connected(&self, conn: &Arc<dyn ProtocolConnection>);
    async fn on_disconnected(&self, conn_id: u64, account: Option<&str>);
    async fn on_authenticated(&self, conn: &Arc<dyn ProtocolConnection>, account: &str);
}

#[async_trait]
pub trait ClientEventHandler: Send + Sync {
    async fn on_connected(&self, conn: &Arc<dyn ProtocolConnection>);
    async fn on_disconnected(&self, conn_id: u64);
}

pub trait Protocol: Send + Sync + 'static {
    type Submit: Send + Sync + Clone + 'static;
    type SubmitResp: Send + Sync + Clone + 'static;
    type MsgId: Send + Sync + 'static;
    type Deliver: Send + Sync + Clone + 'static;

    fn name(&self) -> &'static str;

    fn next_msg_id(&self) -> u64;

    fn encode_submit_resp(
        &self,
        sequence_id: u32,
        msg_id: &Self::MsgId,
        result: u32,
    ) -> Vec<u8>;
}
