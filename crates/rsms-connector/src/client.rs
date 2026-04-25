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
use rsms_core::{ConnectionInfo, RawPdu, EndpointConfig, Frame, Result, RsmsError, SessionState};
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
    while buf.len() >= 4 {
        let total = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        if total < min_len || total > 100_000 {
            buf.drain(..1);
            continue;
        }
        if total > buf.len() {
            break;
        }
        let frame_data: Vec<u8> = buf.drain(..total).collect();
        let (command_id, sequence_id) = extract_header(&frame_data);
        out.push(Frame::new(command_id, sequence_id, RawPdu::from_vec(frame_data)));
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
}

impl ClientConnection {
    async fn new(stream: TcpStream, endpoint: Arc<EndpointConfig>, config: ClientConfig, decoder: Arc<tokio::sync::Mutex<Box<dyn FrameDecoder>>>) -> Arc<Self> {
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
        })
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
        
        let seq_offset = match self.endpoint.protocol.as_str() {
            "smpp" | "sgip" => 12,
            _ => 8,
        };
        let sequence_id = if pdu_slice.len() >= seq_offset + 4 {
            u32::from_be_bytes([pdu_slice[seq_offset], pdu_slice[seq_offset + 1], pdu_slice[seq_offset + 2], pdu_slice[seq_offset + 3]])
        } else {
            return Err(RsmsError::Codec("PDU too short".to_string()));
        };
        
        let (tx, rx) = oneshot::channel();
        
        let pdu_vec = pdu_slice.to_vec();
        let _future = match window.offer(sequence_id, pdu_vec, self.endpoint.timeout).await {
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
        
        if let Err(e) = self.write_frame(pdu_slice).await {
            error!(conn_id = self.id, remote_ip = %self.remote_ip(), remote_port = self.remote_port(), "write_frame failed, sequence_id={}: {}", sequence_id, e);
            let _ = window.cancel(&sequence_id).await;
            let _ = self.pending_responses.lock().await.remove(&sequence_id);
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
        self.endpoint.log_level.map_or(true, |max| level >= max)
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

/// 连接到服务器并启动读循环。
pub async fn connect<D: FrameDecoder + Send + Sync + 'static>(
    endpoint: Arc<EndpointConfig>,
    client_handler: Arc<dyn ClientHandler>,
    decoder: D,
    client_config: Option<ClientConfig>,
    message_source: Option<Arc<dyn MessageSource>>,
    event_handler: Option<Arc<dyn ClientEventHandler>>,
) -> Result<Arc<ClientConnection>> {
    let config = client_config.unwrap_or_default();
    let addr = format!("{}:{}", endpoint.host, endpoint.port);
    info!(%addr, "connecting to server");
    let stream = TcpStream::connect(&addr).await.map_err(RsmsError::Io)?;
    let decoder: Arc<tokio::sync::Mutex<Box<dyn FrameDecoder>>> = Arc::new(tokio::sync::Mutex::new(Box::new(decoder)));
    let decoder_for_conn = Arc::clone(&decoder);
    let conn = ClientConnection::new(stream, endpoint.clone(), config, decoder_for_conn).await;

    if let Some(window) = &conn.window {
        window.start_monitor();
    }

    let conn_clone = Arc::clone(&conn);
    let client_handler_clone = Arc::clone(&client_handler);

    if let Some(handler) = &event_handler {
        let conn_for_handler = conn_clone.clone() as Arc<dyn ProtocolConnection>;
        handler.on_connected(&conn_for_handler).await;
    }

    tokio::spawn(async move {
        run_client_read_loop(conn_clone, client_handler_clone, Arc::clone(&decoder), event_handler).await;
    });

    // 启动 keepalive 任务
    let protocol = endpoint.protocol.clone();
    let idle_timeout = endpoint.idle_time_sec as u32;
    start_keepalive_task(Arc::clone(&conn), protocol, idle_timeout);

    conn.mark_connected().await;
    info!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "connection established");

    if let Some(source) = message_source {
        let conn_clone = Arc::clone(&conn);
        tokio::spawn(async move {
            run_outbound_fetcher(conn_clone, source).await;
        });
    }

    Ok(conn)
}

fn create_decoder(protocol: &str) -> Box<dyn FrameDecoder + Send + Sync> {
    match protocol {
        "cmpp" => Box::new(CmppDecoder),
        "sgip" => Box::new(SgipDecoder),
        "smgp" => Box::new(SmgpDecoder),
        "smpp" => Box::new(SmppDecoder),
        _ => Box::new(CmppDecoder),
    }
}

/// 连接函数，由 ClientPool 调用，连接的生命周期由 Pool 管理
pub async fn connect_with_pool(
    stream: TcpStream,
    endpoint: Arc<EndpointConfig>,
    client_handler: Arc<dyn ClientHandler>,
    client_config: Option<ClientConfig>,
    message_source: Option<Arc<dyn MessageSource>>,
    event_handler: Option<Arc<dyn ClientEventHandler>>,
    event_tx: mpsc::Sender<ConnectionEvent>,
) -> Result<Arc<ClientConnection>> {
    let decoder: Arc<tokio::sync::Mutex<Box<dyn FrameDecoder>>> = Arc::new(tokio::sync::Mutex::new(create_decoder(&endpoint.protocol)));
    let decoder_for_conn = Arc::clone(&decoder);
    let config = client_config.unwrap_or_default();
    let conn = ClientConnection::new(stream, endpoint.clone(), config, decoder_for_conn).await;
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

    tokio::spawn(async move {
        run_client_read_loop(conn_clone, client_handler_clone, decoder, event_handler).await;
    });

    // 启动 keepalive 任务
    let protocol = endpoint.protocol.clone();
    let idle_timeout = endpoint.idle_time_sec as u32;
    start_keepalive_task(Arc::clone(&conn), protocol, idle_timeout);

    conn.mark_connected().await;
    info!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "connection established");

    if let Some(source) = message_source {
        let conn_clone = Arc::clone(&conn);
        tokio::spawn(async move {
            run_outbound_fetcher(conn_clone, source).await;
        });
    }

    Ok(conn)
}

async fn run_client_read_loop(
    conn: Arc<ClientConnection>,
    client_handler: Arc<dyn ClientHandler>,
    decoder: Arc<tokio::sync::Mutex<Box<dyn FrameDecoder>>>,
    event_handler: Option<Arc<dyn ClientEventHandler>>,
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
                    error!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "frame decode: {}", e);
                break;
            }
        };

        for frame in frames {
            conn.touch();

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
                let response_for_caller = response.clone();
                let _ = window.complete(&frame.sequence_id, response).await;
                
                let mut pending = conn.pending_responses.lock().await;
                if let Some(tx) = pending.remove(&frame.sequence_id) {
                    let _ = tx.send(Ok(response_for_caller));
                    tracing::debug!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), seq_id = frame.sequence_id, "response sent to caller via ResponseFuture");
                }
                
                let conn_clone = conn.clone();
                tokio::spawn(async move {
                    while let Some(pending_pdu) = conn_clone.pop_pending().await {
                        let pdu_slice = pending_pdu.as_bytes_ref();
                        let seq_off = match conn_clone.endpoint.protocol.as_str() {
                            "smpp" | "sgip" => 12,
                            _ => 8,
                        };
                        if pdu_slice.len() >= seq_off + 4 {
                            let seq_id = u32::from_be_bytes([pdu_slice[seq_off], pdu_slice[seq_off + 1], pdu_slice[seq_off + 2], pdu_slice[seq_off + 3]]);
                            if let Some(window) = conn_clone.window.as_ref() {
                                if window.contains(&seq_id).await {
                                    continue;
                                }
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

            let ctx = ClientContext {
                endpoint: &conn.endpoint,
                conn: &conn,
            };

            if conn.endpoint.log_level.map_or(true, |max| tracing::Level::INFO <= max) {
                tracing::info!(
                    conn_id = conn.id,
                    remote_ip = %conn.remote_ip(),
                    remote_port = conn.remote_port(),
                    len = frame.len(),
                    cmd_id = frame.command_id,
                    "received frame"
                );
            }
            
            if let Err(e) = client_handler.on_inbound(&ctx, &frame).await {
                error!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "client handler error: {}", e);
            }
        }
        
        if should_close {
            break;
        }
    }
    
    conn.mark_disconnected().await;
    info!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "connection closed");

    // Trigger on_disconnected callback
    if let Some(handler) = event_handler {
        handler.on_disconnected(conn.id).await;
    }
}

async fn run_outbound_fetcher(
    conn: Arc<ClientConnection>,
    source: Arc<dyn MessageSource>,
) {
    loop {
        if !conn.ready_for_fetch() {
            break;
        }

        if !conn.config.can_submit {
            tokio::time::sleep(Duration::from_millis(10)).await;
            continue;
        }

        let account = conn.authenticated_account().await.unwrap_or_default();

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

        for item in items {
            if !conn.ready_for_fetch() {
                break;
            }
            let pdus = match item {
                MessageItem::Single(pdu) => vec![pdu],
                MessageItem::Group { items } => items,
            };
            for pdu in pdus {
                match conn.write_frame(pdu.as_bytes()).await {
                    Ok(_) => {
                        if conn.endpoint.log_level.map_or(true, |max| tracing::Level::TRACE <= max) {
                            tracing::trace!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "send success");
                        }
                    }
                    Err(e) => {
                        if conn.endpoint.log_level.map_or(true, |max| tracing::Level::DEBUG <= max) {
                            tracing::debug!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "send failed: {}", e);
                        }
                        tokio::time::sleep(Duration::from_millis(1)).await;
                        break;
                    }
                }
            }
        }
    }

    info!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "outbound fetcher stopped");
}

/// 启动客户端 keepalive 任务
fn start_keepalive_task(conn: Arc<ClientConnection>, protocol: String, idle_timeout: u32) {
    let keepalive_interval = Duration::from_secs(idle_timeout as u64 / 2);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(keepalive_interval);
        
        loop {
            interval.tick().await;
            
            if !conn.ready_for_fetch() {
                break;
            }
            
            let elapsed = conn.last_sent_elapsed();
            if elapsed >= keepalive_interval {
                if let Err(e) = send_keepalive_packet(&conn, &protocol).await {
                    error!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), protocol = %protocol, "keepalive failed: {}", e);
                } else {
                    tracing::debug!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), protocol = %protocol, "keepalive sent");
                }
            }
        }
        
        info!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "keepalive task stopped");
    });
}

/// 发送保活报文
async fn send_keepalive_packet(conn: &Arc<ClientConnection>, protocol: &str) -> Result<()> {
    let pdu = match protocol {
        "sgip" => build_sgip_keepalive_pdu(),
        "smgp" => build_smgp_active_test_pdu(),
        "smpp" => build_smpp_enquire_link_pdu(),
        "cmpp" => build_cmpp_active_test_pdu(),
        _ => return Err(RsmsError::Other(format!("unsupported protocol: {}", protocol))),
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
