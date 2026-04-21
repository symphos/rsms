use async_trait::async_trait;
use rsms_business::{run_chain, BusinessHandler, ProtocolConnection as BusinessProtocolConnection, RateLimiter};
use rsms_codec_cmpp::CommandId as CmppCommandId;
use rsms_codec_sgip::CommandId as SgipCommandId;
use rsms_codec_smgp::CommandId as SmgpCommandId;
use rsms_codec_smpp::CommandId as SmppCommandId;
use rsms_core::{ConnectionInfo, RawPdu, Frame, Result, SessionState};
use rsms_pipeline::{PipelineBuilder, SimplePipeline, PipelineConfig};
use rsms_session::ConnectionContext;
use rsms_window::{Window, WindowConfig};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::time::timeout;
use tracing::error;

use crate::handlers::cmpp::CmppHandler;
use crate::protocol::{AccountConfigProvider, FrameDecoder, HandleResult, ProtocolConnection, ProtocolHandler, ServerEventHandler, SubmitLimiter};
use crate::handlers::sgip::SgipHandler;
use crate::handlers::smgp::SmgpHandler;
use crate::handlers::smpp::SmppHandler;
use crate::pool::{AccountPool, AccountConnections};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub struct Connection {
    pub id: u64,
    pub config: Arc<rsms_core::EndpointConfig>,
    write: Mutex<OwnedWriteHalf>,
    ready: AtomicBool,
    ctx: Mutex<ConnectionContext>,
    #[allow(dead_code)]
    pipeline: Mutex<Option<SimplePipeline>>,
    authenticated_account: Mutex<Option<String>>,
    window: Option<Window<u32, Vec<u8>, Vec<u8>>>,
    last_active: Mutex<Instant>,
    account_connections: Mutex<Option<Arc<AccountConnections>>>,
    remote_addr: Option<std::net::SocketAddr>,
}

impl Connection {
    pub fn from_stream(stream: TcpStream, config: Arc<rsms_core::EndpointConfig>) -> (Arc<Self>, OwnedReadHalf) {
        let peer_addr = stream.peer_addr().ok();
        let local_addr = stream.local_addr().ok();
        let remote_addr = peer_addr;
        let (read, write) = stream.into_split();
        let pipeline = PipelineBuilder::new(PipelineConfig::default()).build();
        let conn_info = ConnectionInfo::new(peer_addr, local_addr);
        let conn = Arc::new(Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            config: config.clone(),
            write: Mutex::new(write),
            ready: AtomicBool::new(false),
            ctx: Mutex::new(ConnectionContext::with_connection_info(config, conn_info)),
            pipeline: Mutex::new(Some(pipeline)),
            authenticated_account: Mutex::new(None),
            window: None,
            last_active: Mutex::new(Instant::now()),
            account_connections: Mutex::new(None),
            remote_addr,
        });
        (conn, read)
    }

    pub fn new_with_window(stream: TcpStream, config: Arc<rsms_core::EndpointConfig>, window_size: u16) -> (Arc<Self>, OwnedReadHalf) {
        let peer_addr = stream.peer_addr().ok();
        let local_addr = stream.local_addr().ok();
        let remote_addr = peer_addr;
        let (read, write) = stream.into_split();
        let pipeline = PipelineBuilder::new(PipelineConfig::default()).build();
        let window_config = WindowConfig::new(window_size as usize, config.timeout);
        let window = Window::new(window_config);
        let conn_info = ConnectionInfo::new(peer_addr, local_addr);

        let conn = Arc::new(Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            config: config.clone(),
            write: Mutex::new(write),
            ready: AtomicBool::new(false),
            ctx: Mutex::new(ConnectionContext::with_connection_info(config, conn_info)),
            pipeline: Mutex::new(Some(pipeline)),
            authenticated_account: Mutex::new(None),
            window: Some(window),
            last_active: Mutex::new(Instant::now()),
            account_connections: Mutex::new(None),
            remote_addr,
        });
        (conn, read)
    }

    pub fn remote_ip(&self) -> String {
        self.remote_addr
            .map(|a| a.ip().to_string())
            .unwrap_or_default()
    }

    pub fn remote_port(&self) -> u16 {
        self.remote_addr.map(|a| a.port()).unwrap_or(0)
    }

    pub fn ready_for_fetch(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    pub async fn session_state(&self) -> SessionState {
        self.ctx.lock().await.session_state()
    }

    pub async fn mark_pipeline_ready(&self) {
        self.ready.store(true, Ordering::Release);
    }

    pub async fn mark_disconnected(&self) {
        let ctx = self.ctx.lock().await;
        ctx.mark_disconnected();
    }

    pub async fn close(&self) {
        self.ready.store(false, Ordering::Release);
        {
            let ctx = self.ctx.lock().await;
            ctx.mark_disconnected();
        }
        if let Some(close_pkt) = encode_close_packet(&self.config.protocol) {
            let _ = self.write_frame(&close_pkt).await;
        }
        {
            let mut write = self.write.lock().await;
            let _ = write.shutdown().await;
        }
        tracing::warn!(
            conn_id = self.id,
            remote_ip = %self.remote_ip(),
            remote_port = self.remote_port(),
            protocol = %self.config.protocol,
            "connection closed by framework (evict)"
        );
    }

    pub async fn write_frame(&self, data: &[u8]) -> Result<()> {
        {
            let mut write = self.write.lock().await;
            write.write_all(data).await?;
            write.flush().await?;
        }
        self.touch().await;
        Ok(())
    }
    
    pub async fn writable(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    pub async fn authenticated_account(&self) -> Option<String> {
        self.authenticated_account.lock().await.clone()
    }

    pub async fn mark_authenticated(&self, account: String) {
        let ctx = self.ctx.lock().await;
        ctx.set_authenticated_account(account.clone());
        ctx.mark_authenticated(account).unwrap_or_default();
    }

    pub async fn touch(&self) {
        let mut last = self.last_active.lock().await;
        *last = Instant::now();
    }

    pub async fn last_active(&self) -> Instant {
        *self.last_active.lock().await
    }

    pub async fn is_idle(&self, timeout: Duration) -> bool {
        self.last_active().await.elapsed() > timeout
    }

    pub fn window(&self) -> Option<&Window<u32, Vec<u8>, Vec<u8>>> {
        self.window.as_ref()
    }

    pub async fn is_healthy(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    pub async fn account_connections(&self) -> Option<Arc<AccountConnections>> {
        self.account_connections.lock().await.clone()
    }

    pub async fn set_account_connections(&self, acc_conn: Option<Arc<AccountConnections>>) {
        *self.account_connections.lock().await = acc_conn;
    }

    pub async fn submit_limiter(&self) -> Option<Arc<dyn SubmitLimiter>> {
        self.account_connections.lock().await.clone().map(|ac| ac as Arc<dyn SubmitLimiter>)
    }
}

#[async_trait::async_trait]
impl ProtocolConnection for Connection {
    fn id(&self) -> u64 {
        self.id
    }

    async fn write_frame(&self, data: &[u8]) -> Result<()> {
        Connection::write_frame(self, data).await
    }

    async fn set_authenticated_account(&self, account: String) {
        let ctx = self.ctx.lock().await;
        ctx.set_authenticated_account(account.clone());
        let _ = ctx.mark_authenticated(account.clone());
        let mut auth = self.authenticated_account.lock().await;
        *auth = Some(account);
    }

    async fn authenticated_account(&self) -> Option<String> {
        self.authenticated_account.lock().await.clone()
    }

    async fn submit_limiter(&self) -> Option<Arc<dyn SubmitLimiter>> {
        Connection::submit_limiter(self).await
    }

    async fn protocol_version(&self) -> Option<u8> {
        self.ctx.lock().await.protocol_version()
    }

    async fn set_protocol_version(&self, version: u8) {
        self.ctx.lock().await.set_protocol_version(version);
    }

    async fn replace_decoder(&self, _decoder: Box<dyn FrameDecoder>) {
        tracing::warn!(conn_id = self.id, remote_ip = %self.remote_ip(), remote_port = self.remote_port(), "Server-side Connection does not support decoder replacement");
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
        self.config.log_level.map_or(true, |max| level <= max)
    }
}

#[async_trait]
impl BusinessProtocolConnection for Connection {
    fn id(&self) -> u64 {
        self.id
    }

    async fn write_frame(&self, data: &[u8]) -> Result<()> {
        Connection::write_frame(self, data).await
    }

    async fn authenticated_account(&self) -> Option<String> {
        Connection::authenticated_account(self).await
    }

    async fn rate_limiter(&self) -> Option<Arc<dyn RateLimiter>> {
        Connection::submit_limiter(self).await.map(|limiter| {
            Arc::new(SubmitLimiterAdapter { inner: limiter }) as Arc<dyn RateLimiter>
        })
    }
}

struct SubmitLimiterAdapter {
    inner: Arc<dyn crate::protocol::SubmitLimiter>,
}

#[async_trait]
impl RateLimiter for SubmitLimiterAdapter {
    async fn try_acquire(&self) -> bool {
        self.inner.try_acquire_submit().await
    }

    async fn acquire(&self, timeout: std::time::Duration) -> bool {
        self.inner.acquire_submit(timeout).await
    }
}

pub async fn run_connection(
    read: OwnedReadHalf,
    conn: Arc<Connection>,
    handlers: Vec<Arc<dyn BusinessHandler>>,
    account_pool: Option<Arc<AccountPool>>,
    account_config_provider: Option<Arc<dyn AccountConfigProvider>>,
    auth_handler: Option<Arc<dyn crate::protocol::AuthHandler>>,
    protocol: &str,
    event_handler: Option<Arc<dyn ServerEventHandler>>,
) {
    let mut read = read;
    let mut read_buf = Vec::new();
    let mut tmp_buf = [0u8; 8192];
    let cmpp_handler = CmppHandler::new(auth_handler.clone());
    let smgp_handler = SmgpHandler::new(auth_handler.clone());
    let sgip_handler = SgipHandler::new(auth_handler.clone());
    let smpp_handler = SmppHandler::new(auth_handler.clone());
    
    let idle_timeout = Duration::from_secs(conn.config.idle_time_sec as u64);
    let idle_check_interval = Duration::from_secs((conn.config.idle_time_sec / 2) as u64);
    
    loop {
        let n = match timeout(idle_check_interval, read.read(&mut tmp_buf)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => n,
            Ok(Err(e)) => {
                error!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "read error: {e}");
                break;
            }
            Err(_) => {
                if conn.is_idle(idle_timeout).await {
                    tracing::warn!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), idle_timeout_secs = idle_timeout.as_secs(), "connection idle timeout, closing");
                    if let Some(close_pkt) = encode_close_packet(protocol) {
                        let _ = conn.write_frame(&close_pkt).await;
                    }
                    break;
                }
                continue;
            }
        };

        read_buf.extend_from_slice(&tmp_buf[..n]);
        conn.touch().await;

        let frames = match decode_frames_drain(&mut read_buf, protocol) {
            Ok(f) => f,
            Err(e) => {
                error!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "frame decode: {e}");
                break;
            }
        };

        for frame in frames {
            let conn_arc = conn.clone();
            
            let handle_result = match protocol {
                "cmpp" => cmpp_handler.handle_frame(&frame, conn_arc.clone()).await,
                "smgp" => smgp_handler.handle_frame(&frame, conn_arc.clone()).await,
                "sgip" => sgip_handler.handle_frame(&frame, conn_arc.clone()).await,
                "smpp" => smpp_handler.handle_frame(&frame, conn_arc.clone()).await,
                _ => {
                    tracing::warn!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), protocol, "unknown protocol");
                    Ok(HandleResult::Stop)
                }
            };
            
            if let Err(e) = handle_result {
                error!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), protocol, "handler error: {}", e);
                continue;
            }
            
            if handle_result.unwrap() == HandleResult::Continue {
                let id_gen = conn_arc.account_connections().await.map(|ac| ac.id_generator().clone());
                if let Err(e) = run_chain(conn.config.clone(), conn_arc.clone() as Arc<dyn rsms_business::ProtocolConnection>, &handlers, &frame, id_gen).await {
                    error!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "business: {}", e);
                }
            }
            
            // handler 执行后再注册到 account pool（确保 authenticated_account 已设置）
            // 只有尚未注册时才注册，避免每帧都重复注册
            if conn_arc.account_connections().await.is_none() {
                let account_after_handler = conn.authenticated_account().await;
                
                if let Some(ref pool) = account_pool {
                    if let Some(acc) = account_after_handler {
                        tracing::info!(conn_id = conn_arc.id, remote_ip = %conn_arc.remote_ip(), remote_port = conn_arc.remote_port(), account = %acc, "registering to account pool");
                        let acc_pool = pool.get_or_create(&acc).await;
                        let _ = acc_pool.add_connection(conn_arc.clone()).await;
                        conn_arc.set_account_connections(Some(acc_pool.clone())).await;
                        tracing::info!(conn_id = conn_arc.id, remote_ip = %conn_arc.remote_ip(), remote_port = conn_arc.remote_port(), account = %acc, "registered connection to account pool");
                        
                        if let Some(ref provider) = account_config_provider {
                            match provider.get_config(&acc).await {
                                Ok(config) => {
                                    tracing::info!(conn_id = conn_arc.id, remote_ip = %conn_arc.remote_ip(), remote_port = conn_arc.remote_port(), account = %acc, "updated account config");
                                    acc_pool.update_config(config).await;
                                }
                                Err(e) => {
                                    tracing::warn!(conn_id = conn_arc.id, remote_ip = %conn_arc.remote_ip(), remote_port = conn_arc.remote_port(), account = %acc, "failed to get config: {}", e);
                                }
                            }
                        }
                    } else {
                        tracing::warn!(conn_id = conn_arc.id, remote_ip = %conn_arc.remote_ip(), remote_port = conn_arc.remote_port(), "no authenticated account, skipping pool registration");
                    }
                }
            }
        }
    }
    
    let conn_id = conn.id;
    let account = conn.authenticated_account().await;
    conn.mark_disconnected().await;
    
    if let Some(ref handler) = event_handler {
        handler.on_disconnected(conn_id, account.as_deref()).await;
    }
}

fn decode_frames_drain(buf: &mut Vec<u8>, protocol: &str) -> Result<Vec<Frame>> {
    let mut frames = Vec::new();
    
    let seq_offset = match protocol {
        "smpp" | "sgip" => 12,
        _ => 8,
    };
    
    while buf.len() >= 4 {
        let total = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        if !(4..=100_000).contains(&total) {
            buf.drain(..1);
            continue;
        }
        
        if total > buf.len() {
            break;
        }
        
        let data: Vec<u8> = buf.drain(..total).collect();
        let command_id = if data.len() >= 8 {
            u32::from_be_bytes([data[4], data[5], data[6], data[7]])
        } else {
            0
        };
        let sequence_id = if data.len() >= seq_offset + 4 {
            u32::from_be_bytes([data[seq_offset], data[seq_offset + 1], data[seq_offset + 2], data[seq_offset + 3]])
        } else {
            0
        };
        
        frames.push(Frame::new(command_id, sequence_id, RawPdu::from_vec(data)));
    }
    
    Ok(frames)
}

fn encode_close_packet(protocol: &str) -> Option<Vec<u8>> {
    let command_id: u32;
    let body_len: usize;
    match protocol {
        "cmpp" => {
            command_id = CmppCommandId::Terminate as u32;
            body_len = 0;
        }
        "smgp" => {
            command_id = SmgpCommandId::Exit as u32;
            body_len = 1;
        }
        "sgip" => {
            command_id = SgipCommandId::Unbind as u32;
            body_len = 0;
        }
        "smpp" => {
            command_id = SmppCommandId::UNBIND as u32;
            body_len = 0;
        }
        _ => return None,
    };

    let total_len = 20 + body_len;
    let mut pdu = Vec::with_capacity(total_len);
    pdu.extend_from_slice(&(total_len as u32).to_be_bytes());
    pdu.extend_from_slice(&command_id.to_be_bytes());
    pdu.extend_from_slice(&[0u8; 12]); // sequence ID
    if protocol == "smgp" {
        pdu.extend_from_slice(&[0u8; 1]); // reserved byte
    }
    Some(pdu)
}