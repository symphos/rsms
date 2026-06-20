use async_trait::async_trait;
use rsms_business::{run_chain, BusinessHandler, ProtocolConnection as BusinessProtocolConnection, RateLimiter};
use rsms_codec_cmpp::CommandId as CmppCommandId;
use rsms_codec_sgip::CommandId as SgipCommandId;
use rsms_codec_smgp::CommandId as SmgpCommandId;
use rsms_codec_smpp::CommandId as SmppCommandId;
use rsms_core::{ConnectionInfo, Protocol, RawPdu, Frame, Result, SessionState};
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
        let conn_info = ConnectionInfo::new(peer_addr, local_addr);
        let conn = Arc::new(Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            config: config.clone(),
            write: Mutex::new(write),
            ready: AtomicBool::new(false),
            ctx: Mutex::new(ConnectionContext::with_connection_info(config, conn_info)),
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
        let window_config = WindowConfig::new(window_size as usize, config.timeout);
        let window = Window::new(window_config);
        let conn_info = ConnectionInfo::new(peer_addr, local_addr);

        let conn = Arc::new(Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            config: config.clone(),
            write: Mutex::new(write),
            ready: AtomicBool::new(false),
            ctx: Mutex::new(ConnectionContext::with_connection_info(config, conn_info)),
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

    pub async fn mark_ready(&self) {
        self.ready.store(true, Ordering::Release);
    }

    pub async fn mark_disconnected(&self) {
        self.ready.store(false, Ordering::Release);
        let ctx = self.ctx.lock().await;
        ctx.mark_disconnected();
    }

    pub async fn close(&self) {
        self.ready.store(false, Ordering::Release);
        {
            let ctx = self.ctx.lock().await;
            ctx.mark_disconnected();
        }
        if let Some(close_pkt) = close_packet(self.config.protocol) {
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

    /// 批量写入多帧，仅在末尾 flush 一次，减少每帧 flush 的 syscall 开销。
    /// 帧按入参顺序写出，长短信分组在调用方保持连续即可保证同序。
    pub async fn write_frames(&self, frames: &[&[u8]]) -> Result<()> {
        if frames.is_empty() {
            return Ok(());
        }
        {
            let mut write = self.write.lock().await;
            for f in frames {
                write.write_all(f).await?;
            }
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

    /// 把会话状态机推进到 Connecting（服务端连接 accept 后调用）。
    /// 状态机要求 Disconnected→Connecting→Authenticated；服务端此前从不调本方法，
    /// 导致认证时 transition_to_authenticated 因缺前驱态失败（错误被吞）、session_state 永不到 Logined，
    /// 进而 fetch_available_connection（要求 Logined）找不到连接、**服务端 MessageSource 的 MO/回执从不下发**。
    /// 联调（cmos 客户端连 rsms server）实测此 bug。
    pub async fn mark_connected(&self) {
        let ctx = self.ctx.lock().await;
        let _ = ctx.mark_connected();
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
        self.config.log_level.is_none_or(|max| level >= max)
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

    async fn protocol_version(&self) -> Option<u8> {
        self.ctx.lock().await.protocol_version()
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

#[allow(clippy::too_many_arguments)]
pub async fn run_connection(
    read: OwnedReadHalf,
    conn: Arc<Connection>,
    handlers: Vec<Arc<dyn BusinessHandler>>,
    account_pool: Option<Arc<AccountPool>>,
    account_config_provider: Option<Arc<dyn AccountConfigProvider>>,
    auth_handler: Option<Arc<dyn crate::protocol::AuthHandler>>,
    protocol: Protocol,
    event_handler: Option<Arc<dyn ServerEventHandler>>,
    metrics: Arc<dyn rsms_core::Metrics>,
    shutdown: Arc<AtomicBool>,
) {
    metrics.connection_opened();
    let mut read = read;
    let mut read_buf = Vec::new();
    let mut tmp_buf = [0u8; 8192];
    let cmpp_handler = CmppHandler::new(auth_handler.clone());
    let smgp_handler = SmgpHandler::new(auth_handler.clone());
    let sgip_handler = SgipHandler::new(auth_handler.clone());
    let smpp_handler = SmppHandler::new(auth_handler.clone());

    let idle_timeout = Duration::from_secs(conn.config.idle_time_sec as u64);
    let idle_check_interval = Duration::from_secs((conn.config.idle_time_sec / 2) as u64);
    // 读超时取 min(idle_check_interval, 1s)（下限 100ms 防零时长 busy-loop）：既维持原 idle
    // 检测语义，又让循环每 ≤1s 醒来检查停机标志，使优雅停机能及时收尾本连接。
    let poll = idle_check_interval
        .min(Duration::from_secs(1))
        .max(Duration::from_millis(100));

    loop {
        // 优雅停机：标志置位即跳出，走循环后的统一收尾（注销/断连/回调）。
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        let n = match timeout(poll, read.read(&mut tmp_buf)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => n,
            Ok(Err(e)) => {
                error!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "read error: {e}");
                break;
            }
            Err(_) => {
                if conn.is_idle(idle_timeout).await {
                    tracing::warn!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), idle_timeout_secs = idle_timeout.as_secs(), "connection idle timeout, closing");
                    if let Some(close_pkt) = close_packet(protocol) {
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
                metrics.decode_error(protocol);
                error!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "frame decode: {e}");
                break;
            }
        };

        for frame in frames {
            metrics.inbound_frame(protocol, frame.command_id);
            let conn_arc = conn.clone();
            
            let handle_result = match protocol {
                Protocol::Cmpp => cmpp_handler.handle_frame(&frame, conn_arc.clone()).await,
                Protocol::Smgp => smgp_handler.handle_frame(&frame, conn_arc.clone()).await,
                Protocol::Sgip => sgip_handler.handle_frame(&frame, conn_arc.clone()).await,
                Protocol::Smpp => smpp_handler.handle_frame(&frame, conn_arc.clone()).await,
            };

            // 影子比对：unified-shadow feature 开启时，对任意协议帧经 registry 做统一模型解码。
            // 只打日志，不接管实际处理，错误隔离不影响旧路径。
            #[cfg(feature = "unified-shadow")]
            {
                use rsms_model::ProtocolAdapter as _;
                match crate::adapter_registry::adapter_for(protocol).decode(&frame) {
                    Ok(unified) => tracing::debug!(conn_id = conn.id, proto = protocol.as_str(), ?unified, "shadow decode ok"),
                    Err(e) => tracing::warn!(conn_id = conn.id, proto = protocol.as_str(), "shadow decode err: {e}"),
                }
            }

            // handler 错误即跳过该帧；仅 Continue 才进入业务链，其余（如 Stop）不处理。
            let action = match handle_result {
                Ok(action) => action,
                Err(e) => {
                    error!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), protocol = %protocol, "handler error: {}", e);
                    continue;
                }
            };

            if action == HandleResult::Continue {
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
                        metrics.connection_authenticated(&acc);
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

    // 注销：从 account pool 移除本连接，避免断开后 Arc<Connection> 永久残留在
    // AccountConnections.connections 造成内存泄漏（此前断开仅清理 ConnectionPool，
    // remove_connection 的唯一调用点是缩容 evict_excess_connections）。
    if let Some(acc) = conn.account_connections().await {
        acc.remove_connection(conn_id).await;
    }

    conn.mark_disconnected().await;
    metrics.connection_closed();

    if let Some(ref handler) = event_handler {
        handler.on_disconnected(conn_id, account.as_deref()).await;
    }
}

fn decode_frames_drain(buf: &mut Vec<u8>, protocol: Protocol) -> Result<Vec<Frame>> {
    let mut frames = Vec::new();

    let seq_offset = protocol.seq_offset();
    
    // 用读游标 off 解析，循环结束后一次性 drain 已消费部分，
    // 避免每帧 drain(..total) 触发的 O(N·buflen) 缓冲区搬移。
    let mut off = 0usize;
    while buf.len() - off >= 4 {
        let total =
            u32::from_be_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]) as usize;
        if !(4..=100_000).contains(&total) {
            // 坏长度：前移 1 字节重同步（仅推进 offset，不做 O(n) drain）
            off += 1;
            continue;
        }

        if total > buf.len() - off {
            break;
        }

        let data = buf[off..off + total].to_vec();
        off += total;

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

    if off > 0 {
        buf.drain(..off);
    }

    Ok(frames)
}

fn encode_close_packet(protocol: Protocol) -> Option<Vec<u8>> {
    let command_id: u32;
    let header_len: usize;
    let body_len: usize;
    match protocol {
        Protocol::Cmpp => {
            command_id = CmppCommandId::Terminate as u32;
            header_len = 12;
            body_len = 0;
        }
        Protocol::Smgp => {
            command_id = SmgpCommandId::Exit as u32;
            header_len = 12;
            body_len = 1;
        }
        Protocol::Sgip => {
            command_id = SgipCommandId::Unbind as u32;
            header_len = 20;
            body_len = 0;
        }
        Protocol::Smpp => {
            command_id = SmppCommandId::UNBIND as u32;
            header_len = 16;
            body_len = 0;
        }
    };

    let total_len = header_len + body_len;
    let mut pdu = Vec::with_capacity(total_len);
    pdu.extend_from_slice(&(total_len as u32).to_be_bytes());
    pdu.extend_from_slice(&command_id.to_be_bytes());
    if protocol == Protocol::Smpp {
        pdu.extend_from_slice(&[0u8; 4]);
    }
    pdu.extend_from_slice(&[0u8; 4]);
    if protocol == Protocol::Sgip {
        pdu.extend_from_slice(&[0u8; 8]);
    }
    if protocol == Protocol::Smgp {
        pdu.extend_from_slice(&[0u8; 1]);
    }
    Some(pdu)
}

/// 关闭包编码（全协议收敛经 adapter）：四协议均经 adapter 统一编码；adapter 意外失败兜底回旧实现。
/// SMGP 此前旧实现发 13B（多 1 字节保留位）是 latent bug，现经 adapter 产出合规 12B
/// （SMGP 3.0.3 Exit body 为空，见 codec `Exit::BODY_SIZE = 0`、解码器拒收 total_length≠12）——此为修复。
fn close_packet(protocol: Protocol) -> Option<Vec<u8>> {
    use rsms_model::ProtocolAdapter as _;
    crate::adapter_registry::adapter_for(protocol)
        .encode(&rsms_model::UnifiedMessage::Unbind, rsms_model::Sequence::Plain(0))
        .ok()
        .or_else(|| encode_close_packet(protocol))
}

#[cfg(test)]
mod converge_close_gating {
    use super::*;
    use crate::adapter_registry::adapter_for;
    use rsms_model::{ProtocolAdapter as _, Sequence, UnifiedMessage};

    /// 锁定全协议收敛：close_packet 对四协议均产出 adapter 字节。
    #[test]
    fn close_packet_uses_adapter_for_all_protocols() {
        for p in [Protocol::Cmpp, Protocol::Smgp, Protocol::Sgip, Protocol::Smpp] {
            let via = adapter_for(p).encode(&UnifiedMessage::Unbind, Sequence::Plain(0)).ok();
            assert_eq!(close_packet(p), via, "{p:?} close 应走 adapter 字节");
        }
    }

    /// 记录修复：SMGP 关闭包现为合规 12B（adapter，Exit body 空），不再是旧 encode_close_packet 的 13B。
    #[test]
    fn smgp_close_fixed_to_12b() {
        let fixed = close_packet(Protocol::Smgp).expect("smgp close");
        assert_eq!(fixed.len(), 12, "SMGP 关闭包应为合规 12B");
        assert_ne!(
            fixed,
            encode_close_packet(Protocol::Smgp).expect("legacy"),
            "应已脱离旧 13B 实现（旧实现保留作兜底/回归对照）"
        );
    }
}