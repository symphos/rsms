pub mod cmpp20_test;
pub mod cmpp_test;
pub mod stress_test;
pub mod transaction_manager_test;
pub mod transaction_integration_test;

use cmpp_test::*;
use rsms_connector::{serve, connect, CmppDecoder};
use rsms_connector::client::ClientConfig;
use rsms_core::EndpointConfig;
use std::sync::Arc;
use tokio::time::Duration;

#[allow(dead_code)]
fn get_test_port() -> u16 {
    use std::sync::atomic::{AtomicU16, Ordering};
    static COUNTER: AtomicU16 = AtomicU16::new(0);
    20000 + COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// 启动测试服务端
async fn start_test_server(
    port: u16,
    auth_handler: Option<Arc<dyn rsms_connector::AuthHandler>>,
    biz_handler: Option<Arc<dyn rsms_business::BusinessHandler>>,
    event_handler: Option<Arc<dyn rsms_connector::ServerEventHandler>>,
) -> tokio::task::JoinHandle<()> {
    let config = Arc::new(
        EndpointConfig::new("cmpp-test", "127.0.0.1", port, 100, 60).with_protocol("cmpp"),
    );

    let handlers: Vec<Arc<dyn rsms_business::BusinessHandler>> = if let Some(h) = biz_handler {
        vec![h]
    } else {
        vec![]
    };

    let server = serve(config, handlers, auth_handler, None, None, event_handler, None)
        .await
        .unwrap();

    tokio::spawn(async move {
        let _ = server.run().await;
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_auth_correct_password() {
        let port = get_test_port();
        let account = "900001";
        let password = "testpass";

        let auth_handler = Arc::new(PasswordAuthHandler::new().add_account(account, password));
        let _guard = start_test_server(port, Some(auth_handler.clone()), None, None).await;
        tokio::time::sleep(Duration::from_millis(100)).await;

        let endpoint = Arc::new(EndpointConfig::new("test", "127.0.0.1", port, 100, 60));
        let handler = Arc::new(TestClientHandler::new());
        let ts = Arc::new(TestClientEventHandler::new());
        let ms = Arc::new(TestMessageSource);

        let conn = connect(
            endpoint,
            handler.clone(),
            CmppDecoder,
            Some(ClientConfig::new()),
            Some(ms),
            Some(ts),
        )
        .await
        .unwrap();

        let connect_pdu = handler.build_connect_pdu(account, password);
        conn.send_request(connect_pdu).await.unwrap();

        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if handler.is_connected() {
                break;
            }
        }

        assert_eq!(handler.get_connect_status(), Some(0), "正确密码应该返回status=0");
        assert_eq!(auth_handler.auth_success_count(), 1);
    }

    #[tokio::test]
    async fn test_auth_wrong_password() {
        let port = get_test_port();
        let account = "900001";

        let auth_handler = Arc::new(PasswordAuthHandler::new().add_account(account, "correct"));
        let _guard = start_test_server(port, Some(auth_handler.clone()), None, None).await;
        tokio::time::sleep(Duration::from_millis(100)).await;

        let endpoint = Arc::new(EndpointConfig::new("test", "127.0.0.1", port, 100, 60));
        let handler = Arc::new(TestClientHandler::new());
        let ts = Arc::new(TestClientEventHandler::new());
        let ms = Arc::new(TestMessageSource);

        let conn = connect(
            endpoint,
            handler.clone(),
            CmppDecoder,
            Some(ClientConfig::new()),
            Some(ms),
            Some(ts),
        )
        .await
        .unwrap();

        let connect_pdu = handler.build_connect_pdu(account, "wrong");
        conn.send_request(connect_pdu).await.unwrap();

        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if let Some(status) = handler.get_connect_status() {
                if status != 0 {
                    break;
                }
            }
        }

        assert_ne!(handler.get_connect_status(), Some(0), "错误密码应该返回非0状态");
        assert_eq!(auth_handler.auth_fail_count(), 1);
    }

    #[tokio::test]
    async fn test_auth_unknown_account() {
        let port = get_test_port();
        let auth_handler = Arc::new(PasswordAuthHandler::new());
        let _guard = start_test_server(port, Some(auth_handler.clone()), None, None).await;
        tokio::time::sleep(Duration::from_millis(100)).await;

        let endpoint = Arc::new(EndpointConfig::new("test", "127.0.0.1", port, 100, 60));
        let handler = Arc::new(TestClientHandler::new());
        let ts = Arc::new(TestClientEventHandler::new());
        let ms = Arc::new(TestMessageSource);

        let conn = connect(
            endpoint,
            handler.clone(),
            CmppDecoder,
            Some(ClientConfig::new()),
            Some(ms),
            Some(ts),
        )
        .await
        .unwrap();

        let connect_pdu = handler.build_connect_pdu("123456", "pwd");
        conn.send_request(connect_pdu).await.unwrap();

        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if let Some(status) = handler.get_connect_status() {
                if status != 0 {
                    break;
                }
            }
        }

        assert_ne!(handler.get_connect_status(), Some(0), "未知账号应该失败");
    }

    #[tokio::test]
    async fn test_submit_message() {
        let port = get_test_port();
        let account = "900001";
        let password = "pwd";

        let auth_handler = Arc::new(PasswordAuthHandler::new().add_account(account, password));
        let biz_handler = Arc::new(TestBusinessHandler::new());
        let _guard = start_test_server(port, Some(auth_handler), Some(biz_handler.clone()), None).await;
        tokio::time::sleep(Duration::from_millis(100)).await;

        let endpoint = Arc::new(EndpointConfig::new("test", "127.0.0.1", port, 100, 60));
        let handler = Arc::new(TestClientHandler::new());
        let ts = Arc::new(TestClientEventHandler::new());
        let ms = Arc::new(TestMessageSource);

        let conn = connect(
            endpoint,
            handler.clone(),
            CmppDecoder,
            Some(ClientConfig::new()),
            Some(ms),
            Some(ts),
        )
        .await
        .unwrap();

        let connect_pdu = handler.build_connect_pdu(account, password);
        conn.send_request(connect_pdu).await.unwrap();

        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if handler.is_connected() {
                break;
            }
        }

        assert!(handler.is_connected(), "应该连接成功");

        tokio::time::sleep(Duration::from_millis(50)).await;

        let submit_pdu = handler.build_submit_pdu(account, "13800138000", "Hello Test");
        conn.send_request(submit_pdu).await.unwrap();

        tokio::time::sleep(Duration::from_millis(200)).await;

        assert_eq!(handler.get_submit_status(), Some(0), "短信应该发送成功");
    }

    #[test]
    fn test_compute_auth() {
        use rsms_codec_cmpp::auth::compute_connect_auth;
        let auth = compute_connect_auth("900001", "pwd", 0);
        assert_eq!(auth.len(), 16);
    }
}