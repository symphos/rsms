/// 端点配置，对齐 SMSGate `EndpointEntity` 的核心字段子集（监听地址、连接上限、空闲时间等）。
#[derive(Debug, Clone)]
pub struct EndpointConfig {
    /// 端点唯一标识（对应 `EndpointEntity#getId`）
    pub id: String,
    /// 监听地址，如 `0.0.0.0` 或 `127.0.0.1`
    pub host: String,
    pub port: u16,
    /// 最大并发连接数；`0` 表示不限制（由实现解释，本仓库连接器中与「仅受系统限制」对齐）
    pub max_channels: u16,
    /// 连接空闲检测周期（秒），供业务/会话层使用
    pub idle_time_sec: u16,
    /// 客户端重连间隔（秒），默认5秒
    pub reconnect_interval_sec: u16,
    /// 滑窗大小，控制单个连接的并发请求数
    pub window_size: u16,
    /// 协议类型: "cmpp", "smgp", "sgip", "smpp"
    pub protocol: String,
    /// 请求超时时间（秒），用于滑窗内未匹配响应的超时检测
    pub timeout: std::time::Duration,
    /// 框架日志级别，`None` 表示使用全局默认，`Some(Level)` 表示框架只输出该级别及以上的日志
    pub log_level: Option<tracing::Level>,
}

impl EndpointConfig {
    pub fn new(
        id: impl Into<String>,
        host: impl Into<String>,
        port: u16,
        max_channels: u16,
        idle_time_sec: u16,
    ) -> Self {
        Self {
            id: id.into(),
            host: host.into(),
            port,
            max_channels,
            idle_time_sec,
            reconnect_interval_sec: 5,
            window_size: 16,
            protocol: "cmpp".to_string(),
            timeout: std::time::Duration::from_secs(5),
            log_level: None,
        }
    }

    pub fn with_window_size(mut self, window_size: u16) -> Self {
        self.window_size = window_size;
        self
    }

    pub fn with_protocol(mut self, protocol: impl Into<String>) -> Self {
        self.protocol = protocol.into();
        self
    }

    pub fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_reconnect_interval(mut self, interval: u16) -> Self {
        self.reconnect_interval_sec = interval;
        self
    }

    pub fn with_log_level(mut self, level: tracing::Level) -> Self {
        self.log_level = Some(level);
        self
    }
}
