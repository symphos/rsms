use rsms_core::{ConnectionInfo, EncodedPdu, Frame, Result};
use std::net::SocketAddr;
use std::sync::Arc;
use async_trait::async_trait;

pub const RESPONSE_COMMAND_MASK: u32 = 0x80000000;

#[async_trait]
pub trait SubmitLimiter: Send + Sync {
    async fn try_acquire_submit(&self) -> bool;
    async fn acquire_submit(&self, timeout: std::time::Duration) -> bool;
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

/// 待发送消息的单元，由 `MessageSource::fetch` 返回。
pub enum MessageItem {
    /// 单条普通短信，已序列化为完整 PDU 字节（含协议头）。
    Single(Arc<dyn EncodedPdu>),
    /// 长短信分组：`items` 中的各段 PDU 必须按分段顺序排列。
    ///
    /// 框架保证同一 `Group` 的所有帧在**同一连接**上按顺序连续发出，
    /// 确保对端能正确重组长短信。
    Group { items: Vec<Arc<dyn EncodedPdu>> },
}

/// 出站消息来源，由用户实现并注册到客户端连接池。
///
/// 框架通过 `run_outbound_fetcher` 周期性调用 `fetch` 批量拉取待发消息，
/// 每批最多 16 条，直接通过 `write_frame` 写入连接（不经过滑动窗口）。
///
/// **关键约定**：`account` 参数的值等于 `EndpointConfig.id`（即端点 ID），
/// 而非原始账号字符串。`fetch` 的 key 必须与此一致，否则消息无法被拉取。
#[async_trait]
pub trait MessageSource: Send + Sync {
    /// 为指定账号（端点 ID）拉取一批待发消息，最多 `batch_size` 条。
    ///
    /// 若当前无待发消息，返回空 `Vec`；框架会在下一个周期重试。
    /// 返回 `Err` 时框架记录错误并跳过本批次，连接不受影响。
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
