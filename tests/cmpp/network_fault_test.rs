//! 网络层故障注入测试（P2，纯 Rust，零外部依赖）。
//!
//! 用测试内的 tokio TCP 代理（client → FaultProxy → server）注入网络层故障，
//! 验证应用层的容错与清理。覆盖 toxiproxy 等工具的常见场景，但无需安装任何外部进程，
//! 自包含、CI 友好、生产零依赖（代理只是测试代码，不进生产链路）。
//!
//! 覆盖：高延迟链路下仍能认证；链路中途突断后服务端正确注销（4A.1）。

use async_trait::async_trait;
// 窄腰统一模型：Connect 构造经 CmppAdapter。compute_connect_auth 为鉴权工具，保留。
use rsms_codec_cmpp::adapter::CmppAdapter;
use rsms_codec_cmpp::auth::compute_connect_auth;
use rsms_model::{ProtocolAdapter, Sequence, UnifiedBind, UnifiedMessage};
use rsms_connector::{
    AccountConfig, AccountConfigProvider, AccountPool, AuthCredentials, AuthHandler, AuthResult,
    Protocol, ServerBuilder,
};
use rsms_core::{ConnectionInfo, EndpointConfig, Result};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::Duration;

const TEST_ACCOUNT: &str = "900001";
const TEST_PASSWORD: &str = "pass001";
const CMPP_VERSION: u8 = 0x30;

// ==================== 故障注入代理 ====================

/// 注入到代理的网络故障类型。
#[derive(Clone, Copy)]
enum Fault {
    /// 每个转发数据块前延迟 N 毫秒（高延迟/慢链路）。
    LatencyMs(u64),
    /// 代理连接存活 N 毫秒后强制断开（中途突断 / 对端 RST）。
    CutAfterMs(u64),
}

/// 启动一个故障注入 TCP 代理，监听随机端口，把流量转发到 `upstream_port`，按 `fault` 注入故障。
/// 返回代理监听端口。
async fn spawn_fault_proxy(upstream_port: u16, fault: Fault) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((client, _)) = listener.accept().await {
            if let Ok(upstream) = TcpStream::connect(("127.0.0.1", upstream_port)).await {
                tokio::spawn(proxy_conn(client, upstream, fault));
            }
        }
    });
    port
}

async fn proxy_conn(client: TcpStream, upstream: TcpStream, fault: Fault) {
    let (mut cr, mut cw) = client.into_split();
    let (mut ur, mut uw) = upstream.into_split();

    let c2u = async {
        let mut buf = [0u8; 4096];
        loop {
            let n = match cr.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            if let Fault::LatencyMs(ms) = fault {
                tokio::time::sleep(Duration::from_millis(ms)).await;
            }
            if uw.write_all(&buf[..n]).await.is_err() {
                break;
            }
        }
    };
    let u2c = async {
        let mut buf = [0u8; 4096];
        loop {
            let n = match ur.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            if let Fault::LatencyMs(ms) = fault {
                tokio::time::sleep(Duration::from_millis(ms)).await;
            }
            if cw.write_all(&buf[..n]).await.is_err() {
                break;
            }
        }
    };

    match fault {
        // 到点强制断开：select 命中 sleep 分支后函数返回，两个 split 句柄被 drop → 连接关闭。
        Fault::CutAfterMs(ms) => {
            tokio::select! {
                _ = c2u => {}
                _ = u2c => {}
                _ = tokio::time::sleep(Duration::from_millis(ms)) => {}
            }
        }
        _ => {
            tokio::select! {
                _ = c2u => {}
                _ = u2c => {}
            }
        }
    }
}

// ==================== 服务端脚手架 ====================

struct PasswordAuth;

#[async_trait]
impl AuthHandler for PasswordAuth {
    fn name(&self) -> &'static str {
        "netfault-auth"
    }

    async fn authenticate(
        &self,
        _client_id: &str,
        credentials: AuthCredentials,
        _conn_info: &ConnectionInfo,
    ) -> Result<AuthResult> {
        if let AuthCredentials::Cmpp {
            source_addr,
            authenticator_source,
            timestamp,
            ..
        } = credentials
        {
            if source_addr == TEST_ACCOUNT
                && authenticator_source
                    == compute_connect_auth(&source_addr, TEST_PASSWORD, timestamp)
            {
                return Ok(AuthResult::success(&source_addr));
            }
        }
        Ok(AuthResult::failure(1, "invalid"))
    }
}

struct FixedConfig(AccountConfig);

#[async_trait]
impl AccountConfigProvider for FixedConfig {
    async fn get_config(&self, _account: &str) -> Result<AccountConfig> {
        Ok(self.0.clone())
    }
}

fn build_connect_pdu(seq: u32) -> Vec<u8> {
    let ts = 0u32;
    let bind = UnifiedMessage::Bind(UnifiedBind {
        client_id: TEST_ACCOUNT.to_string(),
        authenticator: compute_connect_auth(TEST_ACCOUNT, TEST_PASSWORD, ts).to_vec(),
        timestamp: ts,
        version: CMPP_VERSION,
        system_type: None,
        mode: rsms_model::BindMode::default(),
        login_mode: None,
    });
    CmppAdapter.encode(&bind, Sequence::Plain(seq)).expect("encode bind")
}

async fn start_server() -> (u16, Arc<AccountPool>, tokio::task::JoinHandle<()>) {
    let cfg = Arc::new(
        EndpointConfig::new("netfault-server", "127.0.0.1", 0, 0, 60)
            .with_protocol(Protocol::Cmpp)
            .with_log_level(tracing::Level::WARN),
    );
    let provider = Arc::new(FixedConfig(
        AccountConfig::new()
            .with_max_connections(0)
            .with_max_qps(100_000),
    ));
    let server = ServerBuilder::new(cfg)
        .message_handlers(vec![])
        .auth_handler(Arc::new(PasswordAuth))
        .account_config_provider(provider as Arc<dyn AccountConfigProvider>)
        .serve()
        .await
        .unwrap();
    let port = server.local_addr.port();
    let account_pool = server.account_pool();
    let handle = tokio::spawn(async move {
        let _ = server.run().await;
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    (port, account_pool, handle)
}

// ==================== 用例 ====================

/// 高延迟链路（每块 +30ms）下仍能完成认证，不被误判为断连。
#[tokio::test]
async fn high_latency_link_still_authenticates() {
    let (server_port, account_pool, server_handle) = start_server().await;
    let proxy_port = spawn_fault_proxy(server_port, Fault::LatencyMs(30)).await;

    let mut s = TcpStream::connect(("127.0.0.1", proxy_port)).await.unwrap();
    s.write_all(&build_connect_pdu(1)).await.unwrap();
    s.flush().await.unwrap();
    // 给足时间穿过高延迟链路完成认证。
    tokio::time::sleep(Duration::from_secs(1)).await;

    let cnt = account_pool.connection_count(TEST_ACCOUNT).await;
    assert!(cnt >= 1, "高延迟链路下应仍能完成认证（实际连接数 {cnt}）");

    drop(s);
    server_handle.abort();
}

/// 链路在认证后中途突断（代理强制关闭），服务端应注销该连接（4A.1）、进程不崩。
#[tokio::test]
async fn midstream_cut_is_cleaned_up() {
    let (server_port, account_pool, server_handle) = start_server().await;
    let proxy_port = spawn_fault_proxy(server_port, Fault::CutAfterMs(500)).await;

    let mut s = TcpStream::connect(("127.0.0.1", proxy_port)).await.unwrap();
    s.write_all(&build_connect_pdu(1)).await.unwrap();
    s.flush().await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        account_pool.connection_count(TEST_ACCOUNT).await >= 1,
        "突断前应已完成认证"
    );

    // 代理在 500ms 后强制断开 → 服务端 read 返回 0 → run_connection 收尾注销。
    tokio::time::sleep(Duration::from_millis(900)).await;
    let cnt = account_pool.connection_count(TEST_ACCOUNT).await;
    assert_eq!(cnt, 0, "网络中途突断后服务端应注销连接（4A.1），实际 {cnt}");

    drop(s);
    server_handle.abort();
}
