//! 客户端连接器
//!
//! 客户端架构：
//! - `ClientConnection` - 单条 TCP 连接（客户端视角）
//! - `connect()` - 连接到服务器并启动读循环
//! - 支持多种协议（CMPP/SGIP/SMGP/SMPP）

use async_trait::async_trait;
use rsms_codec_cmpp::CommandId as CmppCommandId;
use rsms_codec_sgip::CommandId as SgipCommandId;
use rsms_codec_smgp::CommandId as SmgpCommandId;
use rsms_codec_smpp::CommandId as SmppCommandId;
use rsms_business::{
    MessageContext, MessageHandler, ProtocolConnection as BusinessProtocolConnection,
    RateLimiter as BusinessRateLimiter,
};
use rsms_core::{ConnectionInfo, Metrics, NoopMetrics, Protocol, RawPdu, EndpointConfig, Frame, Result, RsmsError, SessionState};
use rsms_session::ConnectionContext;
use rsms_window::{Window, WindowConfig};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use std::sync::Mutex as StdMutex;
use std::task::{Context, Poll};
use std::pin::Pin;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio::sync::{mpsc, Mutex, oneshot};
use tracing::{error, info, instrument};

use crate::protocol::{ClientEventHandler, FrameDecoder, MessageItem, MessageSource, ProtocolConnection, SubmitLimiter};
use crate::pool::AccountConnections;
use crate::transaction::{MessageCallback, TransactionManager, drain_pending, drive_inbound, register_outbound};
use tokio::task::JoinHandle;

/// ResponseFuture - 用于等待服务端响应的 Future
/// 封装 oneshot::Receiver，实现 Future trait
pub struct ResponseFuture {
    receiver: Pin<Box<oneshot::Receiver<Result<Vec<u8>>>>>,
}

impl ResponseFuture {
    pub fn new(receiver: oneshot::Receiver<Result<Vec<u8>>>) -> Self {
        Self {
            receiver: Box::pin(receiver),
        }
    }
}

impl std::fmt::Debug for ResponseFuture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResponseFuture").finish()
    }
}

impl Future for ResponseFuture {
    type Output = Result<Vec<u8>>;
    
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.receiver.as_mut().poll(cx) {
            Poll::Ready(Ok(result)) => Poll::Ready(result),
            Poll::Ready(Err(e)) => Poll::Ready(Err(RsmsError::Other(format!("receiver dropped: {}", e)))),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// CMPP 帧解码器 (12字节头)
pub struct CmppDecoder;

impl FrameDecoder for CmppDecoder {
    fn decode_frames(&self, buf: &mut Vec<u8>) -> Result<Vec<Frame>> {
        decode_frames(buf, 12, extract_cmpp_header)
    }
}

/// SGIP 帧解码器 (20字节头)
pub struct SgipDecoder;

impl FrameDecoder for SgipDecoder {
    fn decode_frames(&self, buf: &mut Vec<u8>) -> Result<Vec<Frame>> {
        decode_frames(buf, 20, extract_sgip_header)
    }
}

/// SMGP 帧解码器 (12字节头)
pub struct SmgpDecoder;

impl FrameDecoder for SmgpDecoder {
    fn decode_frames(&self, buf: &mut Vec<u8>) -> Result<Vec<Frame>> {
        decode_frames(buf, 12, extract_smgp_header)
    }
}

/// SMPP 帧解码器 (16字节头)
pub struct SmppDecoder;

impl FrameDecoder for SmppDecoder {
    fn decode_frames(&self, buf: &mut Vec<u8>) -> Result<Vec<Frame>> {
        decode_frames(buf, 16, extract_smpp_header)
    }
}

/// 提取 CMPP 帧头: command_id 在 bytes 4-7, sequence_id 在 bytes 8-11
fn extract_cmpp_header(data: &[u8]) -> (u32, u32) {
    let command_id = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    let sequence_id = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);
    (command_id, sequence_id)
}

/// 提取 SMGP 帧头: 同 CMPP
fn extract_smgp_header(data: &[u8]) -> (u32, u32) {
    extract_cmpp_header(data)
}

/// 提取 SGIP 帧头: command_id 在 bytes 4-7, sequence_id 在 bytes 12-15
fn extract_sgip_header(data: &[u8]) -> (u32, u32) {
    let command_id = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    let sequence_id = u32::from_be_bytes([data[12], data[13], data[14], data[15]]);
    (command_id, sequence_id)
}

/// 提取 SMPP 帧头: command_id 在 bytes 4-7, sequence_id 在 bytes 12-15
fn extract_smpp_header(data: &[u8]) -> (u32, u32) {
    extract_sgip_header(data)
}

fn decode_frames(buf: &mut Vec<u8>, min_len: usize, extract_header: fn(&[u8]) -> (u32, u32)) -> Result<Vec<Frame>> {
    let mut out = Vec::new();
    // 用读游标 off 解析，结束后一次性 drain，避免每帧 drain 的 O(N·buflen) 搬移。
    let mut off = 0usize;
    while buf.len() - off >= 4 {
        let total =
            u32::from_be_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]) as usize;
        if total < min_len || total > 100_000 {
            off += 1;
            continue;
        }
        if total > buf.len() - off {
            break;
        }
        let frame_data = buf[off..off + total].to_vec();
        off += total;
        let (command_id, sequence_id) = extract_header(&frame_data);
        out.push(Frame::new(command_id, sequence_id, RawPdu::from_vec(frame_data)));
    }
    if off > 0 {
        buf.drain(..off);
    }
    Ok(out)
}

/// 连接向池发送的事件
#[derive(Debug, Clone)]
pub enum ConnectionEvent {
    Disconnected(u64),
    HeartbeatTimeout(u64),
}

impl ConnectionEvent {
    pub fn disconnected(id: u64) -> Self {
        ConnectionEvent::Disconnected(id)
    }

    pub fn heartbeat_timeout(id: u64) -> Self {
        ConnectionEvent::HeartbeatTimeout(id)
    }
}

static NEXT_CLIENT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub can_submit: bool,
    pub window_size_ms: u64,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            can_submit: true,
            window_size_ms: 1000,
        }
    }
}

impl ClientConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_can_submit(mut self, can_submit: bool) -> Self {
        self.can_submit = can_submit;
        self
    }

    pub fn with_window_size_ms(mut self, window_size_ms: u64) -> Self {
        self.window_size_ms = window_size_ms;
        self
    }
}

/// 客户端连接（对应服务器端的 `Connection`）。
pub struct ClientConnection {
    pub id: u64,
    pub endpoint: Arc<EndpointConfig>,
    read: Mutex<OwnedReadHalf>,
    write: Mutex<OwnedWriteHalf>,
    ready: AtomicBool,
    ctx: Mutex<ConnectionContext>,
    window: Option<Window<u32, Vec<u8>, Vec<u8>>>,
    config: ClientConfig,
    last_active: StdMutex<std::time::Instant>,
    last_sent: StdMutex<std::time::Instant>,
    event_tx: Mutex<Option<mpsc::Sender<ConnectionEvent>>>,
    pending_responses: Mutex<HashMap<u32, oneshot::Sender<Result<Vec<u8>>>>>,
    decoder: Arc<tokio::sync::Mutex<Box<dyn FrameDecoder>>>,
    pending_queue: Mutex<VecDeque<RawPdu>>,
    account_connections: Mutex<Option<Arc<AccountConnections>>>,
    remote_addr: Option<std::net::SocketAddr>,
    /// 交付链路管理器。设置 `MessageCallback` 时由 `ClientBuilder` 注入并自动驱动
    /// （读循环按帧分发 Resp/Report/MO，发侧登记 Submit）；未设置时为 `None`，相关路径零成本。
    tm: Option<Arc<TransactionManager>>,
    /// 是否允许出站 fetcher 取新消息。优雅停机第一步置 false（先停发、后停收），
    /// 与 `ready` 区分：停发后读循环仍在跑，以便在途 Resp 继续回来被 drain 等到。
    sending: AtomicBool,
    /// 本连接派生的后台 task 句柄（读循环 / keepalive / 出站 fetcher），供 `shutdown` 收尾 await。
    tasks: Mutex<Vec<JoinHandle<()>>>,
    /// 该连接的 ID/序列号生成器。窄腰 `MessageContext` 要求非 Option；客户端无连接池语义，自持一个。
    pub(crate) id_generator: Arc<dyn rsms_core::IdGenerator>,
}

impl ClientConnection {
    async fn new(stream: TcpStream, endpoint: Arc<EndpointConfig>, config: ClientConfig, decoder: Arc<tokio::sync::Mutex<Box<dyn FrameDecoder>>>, tm: Option<Arc<TransactionManager>>) -> Arc<Self> {
        let remote_addr = stream.peer_addr().ok();
        let (read_half, write_half) = stream.into_split();

        let window_config = WindowConfig::new(endpoint.window_size as usize, endpoint.timeout);
        let window = Window::new(window_config);

        Arc::new(Self {
            id: NEXT_CLIENT_ID.fetch_add(1, Ordering::Relaxed),
            endpoint: endpoint.clone(),
            read: Mutex::new(read_half),
            write: Mutex::new(write_half),
            ready: AtomicBool::new(false),
            ctx: Mutex::new(ConnectionContext::new(endpoint)),
            window: Some(window),
            config,
            last_active: StdMutex::new(std::time::Instant::now()),
            last_sent: StdMutex::new(std::time::Instant::now()),
            event_tx: Mutex::new(None),
            pending_responses: Mutex::new(HashMap::new()),
            decoder,
            pending_queue: Mutex::new(VecDeque::new()),
            account_connections: Mutex::new(None),
            remote_addr,
            tm,
            sending: AtomicBool::new(true),
            tasks: Mutex::new(Vec::new()),
            id_generator: Arc::new(crate::SimpleIdGenerator::new()),
        })
    }

    /// 交付链路管理器（设置了 `MessageCallback` 时存在）。
    pub fn delivery_manager(&self) -> Option<&Arc<TransactionManager>> {
        self.tm.as_ref()
    }

    /// 出站 fetcher 是否仍允许取新消息（优雅停机第一步置 false）。
    pub fn is_sending(&self) -> bool {
        self.sending.load(Ordering::Acquire)
    }

    /// 停止出站发送（不影响读循环，让在途 Resp 继续回来）。
    pub fn stop_sending(&self) {
        self.sending.store(false, Ordering::Release);
    }

    /// 登记一个后台 task 句柄，供 `shutdown` 收尾等待。
    pub async fn register_task(&self, handle: JoinHandle<()>) {
        self.tasks.lock().await.push(handle);
    }

    /// 优雅停机（零丢）：① 停出站发送 → ② 等在途 Submit 的 Resp 追平（有交付链路时，
    /// 上限 `timeout`）→ ③ 断连（停读循环/keepalive、令在途 send_request 失败）→
    /// ④ 等待后台 task 退出（每个上限 3s，keepalive 等慢则脱离，不阻塞收尾）。
    pub async fn shutdown(&self, timeout: Duration) {
        self.stop_sending();

        if let Some(tm) = &self.tm
            && !drain_pending(tm, timeout).await
        {
            tracing::warn!(conn_id = self.id, "shutdown drain 超时：仍有在途 Submit 未收到 Resp");
        }

        self.mark_disconnected().await;

        let handles: Vec<JoinHandle<()>> = std::mem::take(&mut *self.tasks.lock().await);
        for h in handles {
            let _ = tokio::time::timeout(Duration::from_secs(3), h).await;
        }
        info!(conn_id = self.id, "client connection shutdown complete");
    }

    pub async fn set_event_tx(&self, tx: mpsc::Sender<ConnectionEvent>) {
        *self.event_tx.lock().await = Some(tx);
    }

    pub fn touch(&self) {
        *self.last_active.lock().unwrap() = std::time::Instant::now();
    }

    pub fn touch_sent(&self) {
        *self.last_sent.lock().unwrap() = std::time::Instant::now();
    }

    pub fn last_sent_elapsed(&self) -> Duration {
        self.last_sent.lock().unwrap().elapsed()
    }

    pub fn last_active(&self) -> std::time::Instant {
        *self.last_active.lock().unwrap()
    }

    pub fn is_idle(&self, timeout: Duration) -> bool {
        self.last_active().elapsed() > timeout
    }

    pub fn ready_for_fetch(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    pub async fn push_pending(&self, pdu: RawPdu) {
        self.pending_queue.lock().await.push_back(pdu);
    }

    pub async fn pop_pending(&self) -> Option<RawPdu> {
        self.pending_queue.lock().await.pop_front()
    }

    pub async fn pending_count(&self) -> usize {
        self.pending_queue.lock().await.len()
    }
    
    pub async fn session_state(&self) -> SessionState {
        self.ctx.lock().await.session_state()
    }

    pub async fn account_connections(&self) -> Option<Arc<AccountConnections>> {
        self.account_connections.lock().await.clone()
    }

    pub async fn set_account_connections(&self, acc_conn: Option<Arc<AccountConnections>>) {
        *self.account_connections.lock().await = acc_conn;
    }

    pub async fn transaction_manager(&self) -> Option<Arc<crate::transaction::TransactionManager>> {
        self.account_connections.lock().await
            .as_ref()
            .map(|acc| acc.transaction_manager())
    }

    #[instrument(skip(self, pdu), fields(len = pdu.len()))]
    pub async fn write_frame(&self, pdu: &[u8]) -> Result<()> {
        {
            let mut w = self.write.lock().await;
            w.write_all(pdu).await?;
            w.flush().await?;
        }
        self.touch();
        self.touch_sent();
        Ok(())
    }

    /// 批量写入多帧，仅在末尾 flush 一次，减少每帧 flush 的 syscall 开销。
    /// 帧按入参顺序写出（长短信分组在调用方保持连续即同序）。
    pub async fn write_frames(&self, frames: &[&[u8]]) -> Result<()> {
        if frames.is_empty() {
            return Ok(());
        }
        {
            let mut w = self.write.lock().await;
            for f in frames {
                w.write_all(f).await?;
            }
            w.flush().await?;
        }
        self.touch();
        self.touch_sent();
        Ok(())
    }

    pub async fn mark_connected(&self) {
        let g = self.ctx.lock().await;
        let _ = g.mark_connected();
        self.ready.store(true, Ordering::Release);
    }

    pub async fn mark_disconnected(&self) {
        self.ready.store(false, Ordering::Release);
        let g = self.ctx.lock().await;
        g.mark_disconnected();
        if let Some(tx) = self.event_tx.lock().await.take() {
            let _ = tx.send(ConnectionEvent::disconnected(self.id)).await;
        }
        // 连接断开：立即让所有在途 send_request 失败，而非各自挂到 endpoint.timeout。
        // 否则断开-重连场景下大量在途请求全部卡满超时窗口，造成延迟尖峰。
        let mut pending = self.pending_responses.lock().await;
        for (_, tx) in pending.drain() {
            let _ = tx.send(Err(RsmsError::ConnectionClosed(
                "connection closed while awaiting response".to_string(),
            )));
        }
    }
    
    pub fn config(&self) -> &ClientConfig {
        &self.config
    }
    
    pub fn window(&self) -> Option<&Window<u32, Vec<u8>, Vec<u8>>> {
        self.window.as_ref()
    }
    
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn remote_ip(&self) -> String {
        self.remote_addr
            .map(|a| a.ip().to_string())
            .unwrap_or_default()
    }

    pub fn remote_port(&self) -> u16 {
        self.remote_addr.map(|a| a.port()).unwrap_or(0)
    }
    
    /// Send request and track with window for response matching
    /// Returns error if window is full or rate limited
    /// Timeout is configured globally via `EndpointConfig.timeout`
    /// Returns a ResponseFuture that resolves to the response PDU
    ///
    /// **sequence_id 契约**：sequence_id 直接从你构造的 PDU 头读取（框架不代生成）。
    /// 滑动窗口按连接隔离，故同步响应匹配天然不串；但**交付回调链路的 `TransactionManager`
    /// 是按账号共享、以 sequence_id 为键**。因此**同账号多连接场景下，sequence_id 必须在账号内唯一**，
    /// 否则两条连接用相同 sequence 会在 TM 里互相覆盖、导致回执/回调错配。
    /// 推荐用账号共享的生成器取值：`account_connections.id_generator().next_sequence_id()`
    /// （同账号多连接共用同一实例，原子单调，天然账号内唯一）。
    pub async fn send_request<P: Into<RawPdu>>(&self, pdu: P) -> Result<ResponseFuture> {
        let pdu = pdu.into();
        let pdu_slice = pdu.as_bytes_ref();
        
        if !self.config.can_submit {
            return Err(RsmsError::Other("submit not allowed".to_string()));
        }
        
        if !self.ready_for_fetch() {
            return Err(RsmsError::ConnectionClosed("connection not ready".to_string()));
        }
        
        let window = self.window.as_ref().ok_or_else(|| RsmsError::Codec("Window not initialized".to_string()))?;
        
        let seq_offset = self.endpoint.protocol.seq_offset();
        let sequence_id = if pdu_slice.len() >= seq_offset + 4 {
            u32::from_be_bytes([pdu_slice[seq_offset], pdu_slice[seq_offset + 1], pdu_slice[seq_offset + 2], pdu_slice[seq_offset + 3]])
        } else {
            return Err(RsmsError::Codec("PDU too short".to_string()));
        };
        
        let (tx, rx) = oneshot::channel();
        
        // 窗口仅用 sequence_id 做请求/响应匹配，不读取 payload，传空 Vec 避免每条消息整包拷贝
        let _future = match window.offer(sequence_id, Vec::new(), self.endpoint.timeout).await {
            Ok(f) => {
                tracing::trace!(conn_id = self.id, remote_ip = %self.remote_ip(), remote_port = self.remote_port(), "window.offer success, seq_id={}", sequence_id);
                f
            }
            Err(e) => {
                return Err(RsmsError::Codec(format!("Window offer failed: {:?}", e)));
            }
        };
        
        {
            let mut pending = self.pending_responses.lock().await;
            pending.insert(sequence_id, tx);
        }

        // 发侧自动登记：必须在写出**之前**登记，否则极快返回的 SubmitResp 可能在读循环里
        // 早于本处执行，导致 on_submit_resp 找不到事务而丢回调（仅 TM 启用、且帧为 Submit 时生效）。
        if let Some(tm) = &self.tm {
            register_outbound(tm, self.endpoint.protocol, pdu_slice).await;
        }

        if let Err(e) = self.write_frame(pdu_slice).await {
            error!(conn_id = self.id, remote_ip = %self.remote_ip(), remote_port = self.remote_port(), "write_frame failed, sequence_id={}: {}", sequence_id, e);
            let _ = window.cancel(&sequence_id).await;
            let _ = self.pending_responses.lock().await.remove(&sequence_id);
            // 写失败：撤销登记并告警（未登记则为 no-op）
            if let Some(tm) = &self.tm {
                tm.on_send_failed(sequence_id, &e.to_string()).await;
            }
            return Err(e);
        }

        Ok(ResponseFuture::new(rx))
    }
}

#[async_trait]
impl ProtocolConnection for ClientConnection {
    fn id(&self) -> u64 {
        self.id
    }

    async fn write_frame(&self, data: &[u8]) -> Result<()> {
        ClientConnection::write_frame(self, data).await
    }

    async fn set_authenticated_account(&self, account: String) {
        self.ctx.lock().await.set_authenticated_account(account);
    }

    async fn authenticated_account(&self) -> Option<String> {
        Some(self.endpoint.id.clone())
    }

    async fn submit_limiter(&self) -> Option<Arc<dyn SubmitLimiter>> {
        None
    }

    async fn protocol_version(&self) -> Option<u8> {
        self.ctx.lock().await.protocol_version()
    }

    async fn set_protocol_version(&self, version: u8) {
        self.ctx.lock().await.set_protocol_version(version);
    }

    async fn replace_decoder(&self, decoder: Box<dyn FrameDecoder>) {
        *self.decoder.lock().await = decoder;
    }

    async fn peer_addr(&self) -> Option<std::net::SocketAddr> {
        self.ctx.lock().await.connection_info.peer_addr
    }

    async fn local_addr(&self) -> Option<std::net::SocketAddr> {
        self.ctx.lock().await.connection_info.local_addr
    }

    async fn connection_info(&self) -> ConnectionInfo {
        self.ctx.lock().await.connection_info.clone()
    }

    fn remote_ip(&self) -> String {
        self.remote_addr
            .map(|a| a.ip().to_string())
            .unwrap_or_default()
    }

    fn remote_port(&self) -> u16 {
        self.remote_addr.map(|a| a.port()).unwrap_or(0)
    }

    fn should_log(&self, level: tracing::Level) -> bool {
        self.endpoint.log_level.is_none_or(|max| level >= max)
    }
}

/// 窄腰 `MessageContext` 要求 `rsms_business::ProtocolConnection`；客户端连接据此参与统一处理链。
/// 与上方 `crate::protocol::ProtocolConnection` 委托同一份连接状态（客户端无限流，故 `rate_limiter` 恒 `None`）。
#[async_trait]
impl BusinessProtocolConnection for ClientConnection {
    fn id(&self) -> u64 {
        self.id
    }

    async fn write_frame(&self, data: &[u8]) -> Result<()> {
        ClientConnection::write_frame(self, data).await
    }

    async fn authenticated_account(&self) -> Option<String> {
        Some(self.endpoint.id.clone())
    }

    async fn rate_limiter(&self) -> Option<Arc<dyn BusinessRateLimiter>> {
        None
    }

    async fn protocol_version(&self) -> Option<u8> {
        self.ctx.lock().await.protocol_version()
    }
}

/// 客户端连接上下文（用于业务处理器）。
pub struct ClientContext<'a> {
    pub endpoint: &'a EndpointConfig,
    pub conn: &'a Arc<ClientConnection>,
}

#[async_trait]
pub trait ClientHandler: Send + Sync {
    fn name(&self) -> &'static str;
    async fn on_inbound(&self, ctx: &ClientContext<'_>, frame: &Frame) -> Result<()>;
}

/// 空客户端处理器：迁到 [`ClientBuilder::with_message_handler`] 后，`ClientBuilder::new` 仍强制
/// 传入一个 `ClientHandler`（其他协议 example 共用该签名），用它占位。WP4-3 删 `client_handler` 时移除。
pub struct NoopClientHandler;

#[async_trait]
impl ClientHandler for NoopClientHandler {
    fn name(&self) -> &'static str {
        "noop-client"
    }
    async fn on_inbound(&self, _ctx: &ClientContext<'_>, _frame: &Frame) -> Result<()> {
        Ok(())
    }
}

/// 客户端构建器：链式配置后调用 [`ClientBuilder::connect`] 建连并启动读循环。
///
/// ```ignore
/// let conn = ClientBuilder::new(endpoint, Arc::new(MyClient), CmppDecoder)
///     .message_source(source)
///     .connect()
///     .await?;
/// ```
pub struct ClientBuilder<D: FrameDecoder + Send + Sync + 'static> {
    endpoint: Arc<EndpointConfig>,
    client_handler: Arc<dyn ClientHandler>,
    decoder: D,
    client_config: Option<ClientConfig>,
    message_source: Option<Arc<dyn MessageSource>>,
    event_handler: Option<Arc<dyn ClientEventHandler>>,
    metrics: Option<Arc<dyn Metrics>>,
    message_callback: Option<Arc<dyn MessageCallback>>,
    message_handler: Option<Arc<dyn MessageHandler>>,
}

impl<D: FrameDecoder + Send + Sync + 'static> ClientBuilder<D> {
    pub fn new(
        endpoint: Arc<EndpointConfig>,
        client_handler: Arc<dyn ClientHandler>,
        decoder: D,
    ) -> Self {
        Self {
            endpoint,
            client_handler,
            decoder,
            client_config: None,
            message_source: None,
            event_handler: None,
            metrics: None,
            message_callback: None,
            message_handler: None,
        }
    }

    /// 注入指标记录器（可观测性）。不设置时使用 `NoopMetrics`。
    pub fn metrics(mut self, metrics: Arc<dyn Metrics>) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// 注入交付链路回调（at-least-once 使能原语）。设置后框架自动驱动：
    /// 出站 Submit 自动登记，收到 SubmitResp/Report/MO 自动回调，并启动 Resp 超时检查器。
    /// 不设置时交付链路完全不启用，相关路径零成本。
    pub fn message_callback(mut self, callback: Arc<dyn MessageCallback>) -> Self {
        self.message_callback = Some(callback);
        self
    }

    pub fn client_config(mut self, config: ClientConfig) -> Self {
        self.client_config = Some(config);
        self
    }

    pub fn message_source(mut self, message_source: Arc<dyn MessageSource>) -> Self {
        self.message_source = Some(message_source);
        self
    }

    pub fn event_handler(mut self, event_handler: Arc<dyn ClientEventHandler>) -> Self {
        self.event_handler = Some(event_handler);
        self
    }

    /// 注入窄腰统一消息处理器（重塑后主路径）。设置后读循环把入站帧解码为 `UnifiedMessage`
    /// 并调 `on_message`；与构造时的 `client_handler`（裸帧旧路径）二选一，设置它即覆盖旧路径。
    pub fn with_message_handler(mut self, handler: Arc<dyn MessageHandler>) -> Self {
        self.message_handler = Some(handler);
        self
    }

    /// 连接到服务器并启动读循环。
    pub async fn connect(self) -> Result<Arc<ClientConnection>> {
        let ClientBuilder {
            endpoint,
            client_handler,
            decoder,
            client_config,
            message_source,
            event_handler,
            metrics,
            message_callback,
            message_handler,
        } = self;
        let metrics: Arc<dyn Metrics> = metrics.unwrap_or_else(|| Arc::new(NoopMetrics));
        let config = client_config.unwrap_or_default();
    let addr = format!("{}:{}", endpoint.host, endpoint.port);
    info!(%addr, "connecting to server");
    let stream = TcpStream::connect(&addr).await.map_err(RsmsError::Io)?;
    let decoder: Arc<tokio::sync::Mutex<Box<dyn FrameDecoder>>> = Arc::new(tokio::sync::Mutex::new(Box::new(decoder)));
    let decoder_for_conn = Arc::clone(&decoder);

    // 配置了 MessageCallback 即启用交付链路：建 TM、挂回调、启动 Resp 超时检查器。
    let tm = if let Some(cb) = message_callback {
        let tm = Arc::new(TransactionManager::new(endpoint.timeout.as_secs().max(1)));
        tm.set_callback(Some(cb)).await;
        Arc::clone(&tm).start_timeout_checker().await;
        Some(tm)
    } else {
        None
    };

    let conn = ClientConnection::new(stream, endpoint.clone(), config, decoder_for_conn, tm).await;

    if let Some(window) = &conn.window {
        window.start_monitor();
    }

    let conn_clone = Arc::clone(&conn);
    let client_handler_clone = Arc::clone(&client_handler);

    if let Some(handler) = &event_handler {
        let conn_for_handler = conn_clone.clone() as Arc<dyn ProtocolConnection>;
        handler.on_connected(&conn_for_handler).await;
    }

    metrics.connection_opened();
    let metrics_for_read = Arc::clone(&metrics);
    let message_handler_clone = message_handler.clone();
    let read_handle = tokio::spawn(async move {
        run_client_read_loop(conn_clone, client_handler_clone, message_handler_clone, Arc::clone(&decoder), event_handler, metrics_for_read).await;
    });
    conn.register_task(read_handle).await;

    // 启动 keepalive 任务
    let protocol = endpoint.protocol;
    let idle_timeout = endpoint.idle_time_sec as u32;
    let keepalive_handle = start_keepalive_task(Arc::clone(&conn), protocol, idle_timeout);
    conn.register_task(keepalive_handle).await;

    conn.mark_connected().await;
    info!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "connection established");

    if let Some(source) = message_source {
        let conn_clone = Arc::clone(&conn);
        let fetch_handle = tokio::spawn(async move {
            run_outbound_fetcher(conn_clone, source, metrics).await;
        });
        conn.register_task(fetch_handle).await;
    }

    Ok(conn)
    }
}

fn create_decoder(protocol: Protocol) -> Box<dyn FrameDecoder + Send + Sync> {
    match protocol {
        Protocol::Cmpp => Box::new(CmppDecoder),
        Protocol::Sgip => Box::new(SgipDecoder),
        Protocol::Smgp => Box::new(SmgpDecoder),
        Protocol::Smpp => Box::new(SmppDecoder),
    }
}

/// 连接函数，由 ClientPool 调用，连接的生命周期由 Pool 管理（内部 API）
pub(crate) async fn connect_with_pool(
    stream: TcpStream,
    endpoint: Arc<EndpointConfig>,
    client_handler: Arc<dyn ClientHandler>,
    client_config: Option<ClientConfig>,
    message_source: Option<Arc<dyn MessageSource>>,
    event_handler: Option<Arc<dyn ClientEventHandler>>,
    event_tx: mpsc::Sender<ConnectionEvent>,
) -> Result<Arc<ClientConnection>> {
    let decoder: Arc<tokio::sync::Mutex<Box<dyn FrameDecoder>>> = Arc::new(tokio::sync::Mutex::new(create_decoder(endpoint.protocol)));
    let decoder_for_conn = Arc::clone(&decoder);
    let config = client_config.unwrap_or_default();
    // ClientPool 路径：交付链路沿用 AccountConnections 上的 TM（手动驱动），此处直连字段置 None。
    let conn = ClientConnection::new(stream, endpoint.clone(), config, decoder_for_conn, None).await;
    conn.set_event_tx(event_tx).await;

    if let Some(window) = &conn.window {
        window.start_monitor();
    }

    let conn_clone = Arc::clone(&conn);
    let client_handler_clone = Arc::clone(&client_handler);

    if let Some(handler) = &event_handler {
        let conn_for_handler = conn_clone.clone() as Arc<dyn ProtocolConnection>;
        handler.on_connected(&conn_for_handler).await;
    }

    // ClientPool 管理的连接本期不接指标(用 Noop);池侧指标留作后续小增量。
    let metrics: Arc<dyn Metrics> = Arc::new(NoopMetrics);
    let metrics_for_read = Arc::clone(&metrics);
    let read_handle = tokio::spawn(async move {
        run_client_read_loop(conn_clone, client_handler_clone, None, decoder, event_handler, metrics_for_read).await;
    });
    conn.register_task(read_handle).await;

    // 启动 keepalive 任务
    let protocol = endpoint.protocol;
    let idle_timeout = endpoint.idle_time_sec as u32;
    let keepalive_handle = start_keepalive_task(Arc::clone(&conn), protocol, idle_timeout);
    conn.register_task(keepalive_handle).await;

    conn.mark_connected().await;
    info!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "connection established");

    if let Some(source) = message_source {
        let conn_clone = Arc::clone(&conn);
        let fetch_handle = tokio::spawn(async move {
            run_outbound_fetcher(conn_clone, source, metrics).await;
        });
        conn.register_task(fetch_handle).await;
    }

    Ok(conn)
}

async fn run_client_read_loop(
    conn: Arc<ClientConnection>,
    client_handler: Arc<dyn ClientHandler>,
    message_handler: Option<Arc<dyn MessageHandler>>,
    decoder: Arc<tokio::sync::Mutex<Box<dyn FrameDecoder>>>,
    event_handler: Option<Arc<dyn ClientEventHandler>>,
    metrics: Arc<dyn Metrics>,
) {
    let mut read_buf = Vec::new();
    let mut tmp_buf = [0u8; 8192];
    let read_timeout = Duration::from_secs(1);
    let mut should_close = false;
    
    loop {
        let n = {
            let mut r = conn.read.lock().await;
            match timeout(read_timeout, r.read(&mut tmp_buf)).await {
                Ok(Ok(0)) => break,
                Ok(Ok(n)) => n,
                Ok(Err(e)) => {
                    error!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "read error: {}", e);
                    break;
                }
                Err(_) => {
                    if !conn.ready_for_fetch() {
                        break;
                    }
                    continue;
                }
            }
        };

        read_buf.extend_from_slice(&tmp_buf[..n]);

        let frames = match decoder.lock().await.decode_frames(&mut read_buf) {
            Ok(f) => f,
                Err(e) => {
                    metrics.decode_error(conn.endpoint.protocol);
                    error!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "frame decode: {}", e);
                break;
            }
        };

        for frame in frames {
            conn.touch();
            metrics.inbound_frame(conn.endpoint.protocol, frame.command_id);

            // Check for connection close commands (e.g., CMPP Terminate, SMGP Exit)
            if frame.command_id == CmppCommandId::Terminate as u32
                || frame.command_id == SmgpCommandId::Exit as u32
                || frame.command_id == SmppCommandId::UNBIND as u32
                || frame.command_id == SgipCommandId::Unbind as u32
            {
                tracing::info!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), command_id = frame.command_id, "received terminate command, closing connection");
                should_close = true;
                break;
            }

            // Try to match with pending request in window and pending_responses
            if let Some(window) = conn.window.as_ref()
                && window.contains(&frame.sequence_id).await
            {
                let response = frame.data.to_vec();
                // window.complete 不读取响应体，传空 Vec 避免一次整包 clone
                let _ = window.complete(&frame.sequence_id, Vec::new()).await;

                let mut pending = conn.pending_responses.lock().await;
                if let Some(tx) = pending.remove(&frame.sequence_id) {
                    let _ = tx.send(Ok(response));
                    tracing::debug!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), seq_id = frame.sequence_id, "response sent to caller via ResponseFuture");
                }
                
                let conn_clone = conn.clone();
                tokio::spawn(async move {
                    while let Some(pending_pdu) = conn_clone.pop_pending().await {
                        let pdu_slice = pending_pdu.as_bytes_ref();
                        let seq_off = conn_clone.endpoint.protocol.seq_offset();
                        if pdu_slice.len() >= seq_off + 4 {
                            let seq_id = u32::from_be_bytes([pdu_slice[seq_off], pdu_slice[seq_off + 1], pdu_slice[seq_off + 2], pdu_slice[seq_off + 3]]);
                            if let Some(window) = conn_clone.window.as_ref()
                                && window.contains(&seq_id).await {
                                    continue;
                                }
                        }
                        match conn_clone.send_request(pending_pdu).await {
                            Ok(_) => {}
                            Err(e) => {
                                tracing::debug!(conn_id = conn_clone.id, remote_ip = %conn_clone.remote_ip(), remote_port = conn_clone.remote_port(), "failed to send pending: {}", e);
                                break;
                            }
                        }
                    }
                });
            }

            // 收侧自动驱动交付链路：SubmitResp/Report/MO 自动回调到用户的 MessageCallback
            // （仅 TM 启用时）。与窗口/ResponseFuture 的同步匹配相互独立、互不干扰。
            if let Some(tm) = &conn.tm {
                drive_inbound(tm, conn.endpoint.protocol, &frame).await;
            }

            if conn.endpoint.log_level.is_none_or(|max| tracing::Level::INFO <= max) {
                tracing::info!(
                    conn_id = conn.id,
                    remote_ip = %conn.remote_ip(),
                    remote_port = conn.remote_port(),
                    len = frame.len(),
                    cmd_id = frame.command_id,
                    "received frame"
                );
            }

            // 影子比对（客户端收包方向）：unified-shadow 开启时对每帧统一解码并打日志，只观测不接管。
            #[cfg(feature = "unified-shadow")]
            {
                use rsms_model::ProtocolAdapter as _;
                let protocol = conn.endpoint.protocol;
                match crate::adapter_registry::adapter_for(protocol).decode(&frame) {
                    Ok(unified) => tracing::debug!(conn_id = conn.id, proto = protocol.as_str(), cmd_id = frame.command_id, ?unified, "shadow decode ok"),
                    Err(e) => tracing::warn!(conn_id = conn.id, proto = protocol.as_str(), cmd_id = frame.command_id, "shadow decode err: {e}"),
                }
            }

            if let Some(mh) = &message_handler {
                // 窄腰主路径：解码为统一消息，构造 MessageContext，调 on_message。
                use rsms_model::ProtocolAdapter as _;
                let adapter = crate::adapter_registry::adapter_for(conn.endpoint.protocol);
                match adapter.decode(&frame) {
                    Ok(unified) => {
                        let ctx = MessageContext::new(
                            conn.endpoint.clone(),
                            conn.clone() as Arc<dyn BusinessProtocolConnection>,
                            conn.id_generator.clone(),
                            adapter,
                            adapter.sequence_of(&frame),
                        );
                        if let Err(e) = mh.on_message(&ctx, &unified).await {
                            error!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "message handler error: {}", e);
                        }
                    }
                    Err(e) => tracing::warn!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "统一模型解码失败（跳过该帧）: {e}"),
                }
            } else {
                let ctx = ClientContext {
                    endpoint: &conn.endpoint,
                    conn: &conn,
                };
                if let Err(e) = client_handler.on_inbound(&ctx, &frame).await {
                    error!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "client handler error: {}", e);
                }
            }
        }
        
        if should_close {
            break;
        }
    }
    
    conn.mark_disconnected().await;
    metrics.connection_closed();
    info!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "connection closed");

    // Trigger on_disconnected callback
    if let Some(handler) = event_handler {
        handler.on_disconnected(conn.id).await;
    }
}

async fn run_outbound_fetcher(
    conn: Arc<ClientConnection>,
    source: Arc<dyn MessageSource>,
    metrics: Arc<dyn Metrics>,
) {
    loop {
        if !conn.ready_for_fetch() || !conn.is_sending() {
            break;
        }

        if !conn.config.can_submit {
            tokio::time::sleep(Duration::from_millis(10)).await;
            continue;
        }

        let account = ProtocolConnection::authenticated_account(&*conn).await.unwrap_or_default();

        let items = match source.fetch(&account, 16).await {
            Ok(items) if !items.is_empty() => items,
            Ok(_) => {
                tokio::time::sleep(Duration::from_millis(1)).await;
                continue;
            }
            Err(e) => {
                error!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "fetch failed: {}", e);
                tokio::time::sleep(Duration::from_millis(10)).await;
                continue;
            }
        };

        if !conn.ready_for_fetch() {
            break;
        }
        // 将一次 fetch 的所有 PDU（含长短信分组，按序）合并为一批，单次 flush 写出。
        let mut pdus = Vec::new();
        for item in items {
            match item {
                MessageItem::Single(pdu) => pdus.push(pdu),
                MessageItem::Group { items } => pdus.extend(items),
            }
        }
        let slices: Vec<&[u8]> = pdus.iter().map(|p| p.as_bytes()).collect();
        match conn.write_frames(&slices).await {
            Ok(_) => {
                for _ in 0..slices.len() {
                    metrics.outbound_frame();
                }
                // 主出站路径发侧登记：经 MessageSource 发出的 Submit 也纳入交付链路追踪
                // （此前主路径不走 window，零交付追踪；仅 TM 启用时生效）。
                if let Some(tm) = &conn.tm {
                    for s in slices.iter().copied() {
                        register_outbound(tm, conn.endpoint.protocol, s).await;
                    }
                }
                if conn.endpoint.log_level.is_none_or(|max| tracing::Level::TRACE <= max) {
                    tracing::trace!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), batch = slices.len(), "send batch success");
                }
            }
            Err(e) => {
                // 批量写失败可能在线上留下半个 PDU（流错位），且本批剩余 PDU 已无法重发。
                // 标记连接断开并退出 fetcher——绝不复用一条可能错位的流（由上层重连）。
                error!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "send batch failed, closing connection: {}", e);
                conn.mark_disconnected().await;
                break;
            }
        }
    }

    info!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "outbound fetcher stopped");
}

/// 启动客户端 keepalive 任务，返回 JoinHandle 供优雅停机收尾。
///
/// 检查节奏取 `min(keepalive_interval, 1s)`（下限 100ms 防零时长 panic），
/// 据 `last_sent_elapsed >= keepalive_interval` 决定是否真正发心跳——既保留发送时序，
/// 又能在 `mark_disconnected`（ready=false）后 ≤1s 内退出，不拖慢 `shutdown` 的 task await。
fn start_keepalive_task(conn: Arc<ClientConnection>, protocol: Protocol, idle_timeout: u32) -> JoinHandle<()> {
    let keepalive_interval = Duration::from_secs(idle_timeout as u64 / 2);
    let check_interval = keepalive_interval
        .min(Duration::from_secs(1))
        .max(Duration::from_millis(100));
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(check_interval);

        loop {
            interval.tick().await;

            if !conn.ready_for_fetch() {
                break;
            }

            let elapsed = conn.last_sent_elapsed();
            if elapsed >= keepalive_interval {
                if let Err(e) = send_keepalive_packet(&conn, protocol).await {
                    error!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), protocol = %protocol, "keepalive failed: {}", e);
                } else {
                    tracing::debug!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), protocol = %protocol, "keepalive sent");
                }
            }
        }

        info!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "keepalive task stopped");
    })
}

/// 发送保活报文
async fn send_keepalive_packet(conn: &Arc<ClientConnection>, protocol: Protocol) -> Result<()> {
    use rsms_model::ProtocolAdapter as _;
    // CMPP/SMPP/SMGP 心跳经 adapter 统一编码（sequence_id=1，与旧实现一致）；adapter 意外失败兜底回旧构造器。
    // SMGP 此前旧心跳 13B（多 1 字节保留位）是 latent bug，现经 adapter 产出合规 12B
    // （SMGP 3.0.3 ActiveTest body 空，codec BODY_SIZE=0、解码器拒收≠12B）——此为修复。
    // SGIP 无专用心跳（用 Trace）、adapter 无 Ping，保留旧实现。
    let pdu = match protocol {
        Protocol::Cmpp => crate::adapter_registry::adapter_for(protocol)
            .encode(&rsms_model::UnifiedMessage::Ping, rsms_model::Sequence::Plain(1))
            .unwrap_or_else(|_| build_cmpp_active_test_pdu()),
        Protocol::Smpp => crate::adapter_registry::adapter_for(protocol)
            .encode(&rsms_model::UnifiedMessage::Ping, rsms_model::Sequence::Plain(1))
            .unwrap_or_else(|_| build_smpp_enquire_link_pdu()),
        Protocol::Smgp => crate::adapter_registry::adapter_for(protocol)
            .encode(&rsms_model::UnifiedMessage::Ping, rsms_model::Sequence::Plain(1))
            .unwrap_or_else(|_| build_smgp_active_test_pdu()),
        Protocol::Sgip => build_sgip_keepalive_pdu(),
    };

    conn.write_frame(&pdu).await?;
    Ok(())
}

/// CMPP ActiveTest 保活报文
fn build_cmpp_active_test_pdu() -> Vec<u8> {
    let total_len = 12u32;
    let command_id = CmppCommandId::ActiveTest as u32;
    let sequence_id = 1u32;
    
    let mut pdu = Vec::with_capacity(12);
    pdu.extend_from_slice(&total_len.to_be_bytes());
    pdu.extend_from_slice(&command_id.to_be_bytes());
    pdu.extend_from_slice(&sequence_id.to_be_bytes());
    pdu
}

/// SMGP ActiveTest 保活报文
fn build_smgp_active_test_pdu() -> Vec<u8> {
    let total_len = 13u32;
    let command_id = SmgpCommandId::ActiveTest as u32;
    let sequence_id = 1u32;
    
    let mut pdu = Vec::with_capacity(13);
    pdu.extend_from_slice(&total_len.to_be_bytes());
    pdu.extend_from_slice(&command_id.to_be_bytes());
    pdu.extend_from_slice(&sequence_id.to_be_bytes());
    pdu.extend_from_slice(&0u8.to_be_bytes()); // reserved
    pdu
}

/// SMPP EnquireLink 保活报文
fn build_smpp_enquire_link_pdu() -> Vec<u8> {
    let total_len = 16u32;
    let command_id = SmppCommandId::ENQUIRE_LINK as u32;
    let sequence_id = 1u32;
    let command_status = 0u32;
    
    let mut pdu = Vec::with_capacity(16);
    pdu.extend_from_slice(&total_len.to_be_bytes());
    pdu.extend_from_slice(&command_id.to_be_bytes());
    pdu.extend_from_slice(&command_status.to_be_bytes());
    pdu.extend_from_slice(&sequence_id.to_be_bytes());
    pdu
}

/// SGIP 无专用心跳命令，使用 Trace 命令（0x00001000）作为轻量保活
fn build_sgip_keepalive_pdu() -> Vec<u8> {
    let total_len = (20u32).to_be_bytes();
    let command_id = (0x00001000u32).to_be_bytes();
    
    let mut pdu = Vec::with_capacity(20);
    pdu.extend_from_slice(&total_len);
    pdu.extend_from_slice(&command_id);
    pdu.extend_from_slice(&[0u8; 12]);
    pdu
}

#[cfg(test)]
mod wp4_client_tests {
    use super::*;
    use async_trait::async_trait;
    use rsms_business::{MessageContext, MessageHandler};
    use rsms_model::UnifiedMessage;

    struct DummyMh;
    #[async_trait]
    impl MessageHandler for DummyMh {
        fn name(&self) -> &'static str { "dummy" }
        async fn on_message(&self, _ctx: &MessageContext, _msg: &UnifiedMessage) -> rsms_core::Result<()> { Ok(()) }
    }

    #[test]
    fn with_message_handler_stores_handler() {
        let ep = Arc::new(rsms_core::EndpointConfig::new("ep", "127.0.0.1", 7890, 16, 60));
        let b = ClientBuilder::new(ep, Arc::new(NoopClientHandler), crate::CmppDecoder)
            .with_message_handler(Arc::new(DummyMh));
        assert!(b.message_handler.is_some(), "with_message_handler 应存入处理器");
    }
}

#[cfg(test)]
mod converge_keepalive_gating {
    use super::*;
    use crate::adapter_registry::adapter_for;
    use rsms_core::Protocol;
    use rsms_model::{ProtocolAdapter as _, Sequence, UnifiedMessage};

    /// 锁定收敛：CMPP/SMPP 的 adapter Ping(seq=1) 仍与旧构造器字节一致。
    #[test]
    fn converged_keepalive_arms_byte_identical() {
        assert_eq!(
            adapter_for(Protocol::Cmpp).encode(&UnifiedMessage::Ping, Sequence::Plain(1)).ok(),
            Some(build_cmpp_active_test_pdu()),
            "CMPP keepalive 字节一致"
        );
        assert_eq!(
            adapter_for(Protocol::Smpp).encode(&UnifiedMessage::Ping, Sequence::Plain(1)).ok(),
            Some(build_smpp_enquire_link_pdu()),
            "SMPP keepalive 字节一致"
        );
    }

    /// 记录修复：SMGP 心跳现为合规 12B（adapter，ActiveTest body 空），不再是旧构造器的 13B；SGIP 无 Ping。
    #[test]
    fn smgp_keepalive_fixed_sgip_no_ping() {
        let smgp = adapter_for(Protocol::Smgp).encode(&UnifiedMessage::Ping, Sequence::Plain(1)).expect("smgp ping");
        assert_eq!(smgp.len(), 12, "SMGP 心跳应为合规 12B");
        assert_ne!(smgp, build_smgp_active_test_pdu(), "应脱离旧 13B 构造器");
        assert!(
            adapter_for(Protocol::Sgip).encode(&UnifiedMessage::Ping, Sequence::Plain(1)).is_err(),
            "SGIP 无心跳，adapter Ping 应报错"
        );
    }
}
