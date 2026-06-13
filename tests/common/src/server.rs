use rsms_connector::{ServerBuilder, AuthHandler, ServerEventHandler, AccountConfigProvider};
use rsms_business::BusinessHandler;
use rsms_core::{EndpointConfig, Protocol};
use std::sync::Arc;

pub async fn start_test_server(
    name: &str,
    protocol: &str,
    auth_handler: Arc<dyn AuthHandler>,
    biz_handler: Arc<dyn BusinessHandler>,
    event_handler: Arc<dyn ServerEventHandler>,
    account_config: Arc<dyn AccountConfigProvider>,
    idle_timeout_secs: u16,
) -> rsms_core::Result<(u16, tokio::task::JoinHandle<()>)> {
    let mut cfg = EndpointConfig::new(name, "127.0.0.1", 0, 8, idle_timeout_secs);
    if !protocol.is_empty() {
        // 协议字符串解析为枚举（大小写不敏感），未知协议直接报错而非静默退化。
        let proto: Protocol = protocol.parse()?;
        cfg = cfg.with_protocol(proto);
    }
    let cfg = Arc::new(cfg);

    let server = ServerBuilder::new(cfg)
        .handlers(vec![biz_handler])
        .auth_handler(auth_handler)
        .account_config_provider(account_config)
        .event_handler(event_handler)
        .serve()
        .await?;

    let port = server.local_addr.port();
    let handle = tokio::spawn(async move {
        let _ = server.run().await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    Ok((port, handle))
}
