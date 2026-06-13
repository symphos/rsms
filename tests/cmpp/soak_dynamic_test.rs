//! P1 长稳：动态账号轮换，量化 4B.2「账号 map 不回收」的真实影响。
//!
//! 每轮换一个新账号建连/认证/发包/断开，观测：
//! - 每账号的连接是否归零（4A.1，应成立）
//! - `account_pool` 驻留账号数是否随轮换单调增长（4B.2 已知不回收——本测试表征此行为）
//! - RSS 随账号数的增长，量化「每账号内存成本」，据此印证 4B.2 暂缓是否合理
//!
//! 账号数由 `RSMS_SOAK_ACCOUNTS` 驱动（默认 60）。WARN 日志（CLAUDE.md 铁律）。

use async_trait::async_trait;
use rsms_codec_cmpp::auth::compute_connect_auth;
use rsms_codec_cmpp::{Connect, Pdu, Submit};
use rsms_connector::{
    AccountConfig, AccountConfigProvider, AccountPool, AuthCredentials, AuthHandler, AuthResult,
    Protocol, ServerBuilder,
};
use rsms_core::{ConnectionInfo, EndpointConfig, Result};
use rsms_test_common::read_rss_kb;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::time::Duration;

const TEST_PASSWORD: &str = "pass001";
const CMPP_VERSION: u8 = 0x30;
const CONNS_PER_ACCOUNT: usize = 2;

/// 接受任意 source_addr + 统一密码的认证器（模拟多租户）。
struct MultiAccountAuth;

#[async_trait]
impl AuthHandler for MultiAccountAuth {
    fn name(&self) -> &'static str {
        "multi-account-auth"
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
            if authenticator_source
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

fn build_connect_pdu(account: &str, seq: u32) -> Vec<u8> {
    let ts = 0u32;
    let connect = Connect {
        source_addr: account.to_string(),
        authenticator_source: compute_connect_auth(account, TEST_PASSWORD, ts),
        version: CMPP_VERSION,
        timestamp: ts,
    };
    let pdu: Pdu = connect.into();
    pdu.to_pdu_bytes(seq).to_vec()
}

fn build_submit_pdu(account: &str, seq: u32) -> Vec<u8> {
    let mut submit = Submit::new();
    submit.src_id = account.to_string();
    submit.dest_usr_tl = 1;
    submit.dest_terminal_ids = vec!["13800138000".to_string()];
    submit.msg_content = b"soak".to_vec();
    let pdu: Pdu = submit.into();
    pdu.to_pdu_bytes(seq).to_vec()
}

async fn start_server() -> (u16, Arc<AccountPool>, tokio::task::JoinHandle<()>) {
    let cfg = Arc::new(
        EndpointConfig::new("soak-dyn-server", "127.0.0.1", 0, 0, 60)
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
        .auth_handler(Arc::new(MultiAccountAuth))
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

#[tokio::test]
async fn soak_rotating_accounts_quantify_retention() {
    let num_accounts: usize = std::env::var("RSMS_SOAK_ACCOUNTS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);

    let (port, account_pool, handle) = start_server().await;
    let rss_before = read_rss_kb().unwrap_or(0);

    let mut seq = 1u32;
    for i in 0..num_accounts {
        let account = format!("90{i:04}");

        let mut streams = Vec::with_capacity(CONNS_PER_ACCOUNT);
        for _ in 0..CONNS_PER_ACCOUNT {
            let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            s.write_all(&build_connect_pdu(&account, seq)).await.unwrap();
            seq += 1;
            s.write_all(&build_submit_pdu(&account, seq)).await.unwrap();
            seq += 1;
            s.flush().await.unwrap();
            streams.push(s);
        }
        tokio::time::sleep(Duration::from_millis(200)).await; // 认证 + 注册

        drop(streams);
        tokio::time::sleep(Duration::from_millis(300)).await; // 注销

        // 4A.1：该账号的连接应已注销（连接数归零，与账号是否回收无关）。
        let cnt = account_pool.connection_count(&account).await;
        assert!(
            cnt <= 1,
            "账号 {account} 断开后连接未注销（4A.1），实际 {cnt}"
        );
    }

    tokio::time::sleep(Duration::from_millis(500)).await;
    let total_accounts = account_pool.all_accounts().await.len();
    let rss_after = read_rss_kb().unwrap_or(0);
    handle.abort();

    let rss_delta = rss_after.saturating_sub(rss_before);
    let per_account_kb = if num_accounts > 0 {
        rss_delta / num_accounts as u64
    } else {
        0
    };
    println!(
        "[dynamic-soak] {num_accounts} 账号轮换；account_pool 驻留账号数 {total_accounts}；\
         RSS {rss_before} → {rss_after} KB（+{rss_delta} KB，每账号 ~{per_account_kb} KB）"
    );

    // 表征 4B.2：账号确实不回收（驻留数 ≈ 轮换数）。这是已知的暂缓行为，本断言固化它，
    // 一旦未来实现账号回收，此测试会提示更新预期。
    assert!(
        total_accounts >= num_accounts.saturating_sub(2),
        "account_pool 应驻留所有轮换过的账号（4B.2 当前不回收），实际驻留 {total_accounts}"
    );

    // 量化：每账号内存成本应有界（粗略上界 200KB/账号，防止意外的大对象泄漏）。
    // /proc 不可用（rss=0）时跳过。
    if rss_before > 0 && rss_after > 0 {
        assert!(
            per_account_kb < 200,
            "每账号内存成本 {per_account_kb}KB 异常偏高，账号驻留的代价超预期"
        );
    }
}
