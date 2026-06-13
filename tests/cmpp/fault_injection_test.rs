//! 异常注入测试（第 1 层：协议/对端层，纯 Rust 裸 socket）。
//!
//! 补连接层端到端缺口（codec 模糊测试只覆盖到解码函数，未覆盖连接层行为）：
//! - 畸形/坏长度/超长/截断 PDU 字节流 → server 进程不崩（健壮性核心承诺）
//! - 逐字节分片到达 → `decode_frames_drain` 半包累积正确重组

use async_trait::async_trait;
use rsms_codec_cmpp::auth::compute_connect_auth;
use rsms_codec_cmpp::{Connect, Pdu};
use rsms_connector::{
    AccountConfig, AccountConfigProvider, AccountPool, AuthCredentials, AuthHandler, AuthResult,
    Protocol, ServerBuilder,
};
use rsms_core::{ConnectionInfo, EndpointConfig, Result};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::time::Duration;

const TEST_ACCOUNT: &str = "900001";
const TEST_PASSWORD: &str = "pass001";
const CMPP_VERSION: u8 = 0x30;

struct PasswordAuth;

#[async_trait]
impl AuthHandler for PasswordAuth {
    fn name(&self) -> &'static str {
        "fault-auth"
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
    let connect = Connect {
        source_addr: TEST_ACCOUNT.to_string(),
        authenticator_source: compute_connect_auth(TEST_ACCOUNT, TEST_PASSWORD, ts),
        version: CMPP_VERSION,
        timestamp: ts,
    };
    let pdu: Pdu = connect.into();
    pdu.to_pdu_bytes(seq).to_vec()
}

async fn start_server() -> (u16, Arc<AccountPool>, tokio::task::JoinHandle<()>) {
    let cfg = Arc::new(
        EndpointConfig::new("fault-server", "127.0.0.1", 0, 0, 60)
            .with_protocol(Protocol::Cmpp)
            .with_log_level(tracing::Level::WARN),
    );
    let provider = Arc::new(FixedConfig(
        AccountConfig::new()
            .with_max_connections(0)
            .with_max_qps(100_000),
    ));
    let server = ServerBuilder::new(cfg)
        .handlers(vec![])
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

/// 注入各种畸形字节流后，server 进程不得崩溃——仍能接受并认证新连接。
#[tokio::test]
async fn server_survives_malformed_pdu_stream() {
    let (port, account_pool, handle) = start_server().await;

    // 坏长度（<头）、超大长度声明、声明远大于实给（截断）、随机字节、半个头。
    let garbage: Vec<Vec<u8>> = vec![
        vec![0, 0, 0, 3],
        vec![255, 255, 255, 255, 0, 0, 0, 1, 0, 0, 0, 1],
        vec![0, 0, 0, 200, 0, 0, 0, 1, 0, 0, 0, 1],
        (0..50u8).collect(),
        vec![0, 0, 0, 12],
    ];
    for g in &garbage {
        if let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)).await {
            let _ = s.write_all(g).await;
            let _ = s.flush().await;
            // s 出作用域即断开
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;

    // 关键断言：server 仍存活，新的合法连接能认证注册。
    let mut good = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    good.write_all(&build_connect_pdu(1)).await.unwrap();
    good.flush().await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let cnt = account_pool.connection_count(TEST_ACCOUNT).await;
    assert!(
        cnt >= 1,
        "畸形流注入后 server 应仍存活并能认证新连接（实际连接数 {cnt}）"
    );

    drop(good);
    handle.abort();
}

/// 合法 Connect PDU 逐字节分片到达，应被 decode_frames_drain 正确重组并认证。
#[tokio::test]
async fn fragmented_connect_is_reassembled_and_authenticates() {
    let (port, account_pool, handle) = start_server().await;

    let pdu = build_connect_pdu(1);
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    // 逐字节发送 + flush + 微小间隔，模拟极端粘包/半包到达。
    for b in &pdu {
        s.write_all(std::slice::from_ref(b)).await.unwrap();
        s.flush().await.unwrap();
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;

    let cnt = account_pool.connection_count(TEST_ACCOUNT).await;
    assert_eq!(
        cnt, 1,
        "逐字节分片的 Connect 应被正确重组并完成认证（实际连接数 {cnt}）"
    );

    drop(s);
    handle.abort();
}
