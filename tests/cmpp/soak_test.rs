//! 长稳测试：周期性断连重连下验证资源稳定（无连接/内存/fd 泄漏）。
//!
//! 时长由环境变量 `RSMS_SOAK_SECS` 驱动（默认 20s 快速验证；发布前可设 3600+ 做小时级长稳）。
//! 复用裸 TcpStream 模拟客户端反复建连/认证/发包/断开，直接行使 4A.1 的连接注销路径
//! （固定长连接压测触发不到此路径）。以 WARN 日志运行（CLAUDE.md 压测铁律）。

use async_trait::async_trait;
// 窄腰统一模型：Connect/Submit 构造经 CmppAdapter。compute_connect_auth 为鉴权工具，保留。
use rsms_codec_cmpp::adapter::CmppAdapter;
use rsms_codec_cmpp::auth::compute_connect_auth;
use rsms_model::{
    Address, Encoding, ProtocolAdapter, ProtocolExtra, Sequence, UnifiedBind, UnifiedMessage,
    UnifiedSubmit,
};
use rsms_connector::{
    AccountConfig, AccountConfigProvider, AuthCredentials, AuthHandler, AuthResult, Protocol,
    ServerBuilder,
};
use rsms_core::{ConnectionInfo, EndpointConfig, Result};
use rsms_test_common::{assert_resource_stable, spawn_resource_sampler};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::time::{Duration, Instant};

const TEST_ACCOUNT: &str = "900001";
const TEST_PASSWORD: &str = "pass001";
const CMPP_VERSION: u8 = 0x30;
const CONNS_PER_ROUND: usize = 4;

struct PasswordAuth;

#[async_trait]
impl AuthHandler for PasswordAuth {
    fn name(&self) -> &'static str {
        "soak-auth"
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

fn build_submit_pdu(seq: u32) -> Vec<u8> {
    // 统一 Submit（其余 CMPP 方言取默认，与原 Submit::new() 默认一致）。
    let submit = UnifiedMessage::Submit(UnifiedSubmit {
        src: Address::plain(TEST_ACCOUNT),
        dests: vec![Address::plain("13800138000")],
        content: b"soak".to_vec(),
        encoding: Encoding::Ascii,
        want_report: false,
        concat: None,
        extra: ProtocolExtra::Cmpp(Default::default()),
        tlvs: vec![],
    });
    CmppAdapter.encode(&submit, Sequence::Plain(seq)).expect("encode submit")
}

#[tokio::test]
async fn soak_reconnect_no_resource_leak() {
    let soak_secs: u64 = std::env::var("RSMS_SOAK_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);

    let cfg = Arc::new(
        EndpointConfig::new("soak-server", "127.0.0.1", 0, 0, 60)
            .with_protocol(Protocol::Cmpp)
            .with_log_level(tracing::Level::WARN),
    );
    // max_connections=0：不限连接数、不触发缩容，单纯验证「断开即注销」。
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
    let server_handle = tokio::spawn(async move {
        let _ = server.run().await;
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    // 每 2 秒采样一次进程 RSS/fd，直到 soak 结束。
    let (samples, sampler) = spawn_resource_sampler(2, soak_secs);

    let start = Instant::now();
    let mut round = 0u64;
    let mut seq = 1u32;
    while start.elapsed().as_secs() < soak_secs {
        round += 1;

        // 建立一批裸连接，认证 + 发几条 submit。
        let mut streams = Vec::with_capacity(CONNS_PER_ROUND);
        for _ in 0..CONNS_PER_ROUND {
            let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            s.write_all(&build_connect_pdu(seq)).await.unwrap();
            seq += 1;
            for _ in 0..3 {
                s.write_all(&build_submit_pdu(seq)).await.unwrap();
                seq += 1;
            }
            s.flush().await.unwrap();
            streams.push(s);
        }
        tokio::time::sleep(Duration::from_millis(300)).await; // 等服务端认证 + 注册

        // 全部断开，服务端 read 返回 0 → run_connection 收尾注销（4A.1）。
        drop(streams);
        tokio::time::sleep(Duration::from_millis(400)).await; // 等注销完成

        // 关键断言：连接数不随轮次累积（泄漏时会 ≈ CONNS_PER_ROUND × round）。
        let cnt = account_pool.connection_count(TEST_ACCOUNT).await;
        assert!(
            cnt <= CONNS_PER_ROUND,
            "第 {round} 轮断开后连接数 {cnt} 累积，疑似连接泄漏（应回落，不随轮次增长）"
        );
    }

    let _ = sampler.await;
    server_handle.abort();

    // 充分静置后，连接应已全部注销。
    tokio::time::sleep(Duration::from_millis(500)).await;
    let final_cnt = account_pool.connection_count(TEST_ACCOUNT).await;
    assert!(
        final_cnt <= 1,
        "长稳结束后连接数应回落到 0（实际 {final_cnt}）"
    );

    let samples = samples.lock().await;
    println!(
        "[soak] {round} 轮重连完成，{} 个资源采样点，结束连接数 {final_cnt}",
        samples.len()
    );
    // 长时间断连重连下，RSS/fd 不应单调增长（容差 15%）。
    assert_resource_stable(&samples, 0.15);
}
