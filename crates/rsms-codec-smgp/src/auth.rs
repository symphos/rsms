use md5::{Digest, Md5};

/// 计算 SMGP Login 请求的 ClientAuth
///
/// SMGP 3.0 规范：ClientAuth = MD5(ClientID + 7个二进制0 + Shared secret + Timestamp)
///
/// - ClientID: 登录名**原始字符串**（不补齐到 8 字节）
/// - Shared secret: 明文密码
/// - Timestamp: **10 位十进制字符串** `MMDDHHMMSS`（左补 0），非 4 字节整数
///
/// 与 cmos/SMSGate 等参考实现一致（2026-06-13 经 lihuanghe 模拟器联调验证）。
pub fn compute_login_auth(client_id: &str, password: &str, timestamp: u32) -> [u8; 16] {
    let mut hasher = Md5::new();

    hasher.update(client_id.as_bytes());

    hasher.update([0u8; 7]);

    hasher.update(password.as_bytes());

    hasher.update(format!("{timestamp:010}").as_bytes());

    hasher.finalize().into()
}

/// 计算 SMGP Login 响应的 ServerAuth
///
/// ServerAuth = MD5(Status + ClientAuth + Password)
pub fn compute_server_auth(status: u32, client_auth: &[u8; 16], password: &str) -> [u8; 16] {
    let mut hasher = Md5::new();
    hasher.update(status.to_be_bytes());
    hasher.update(client_auth);
    hasher.update(password.as_bytes());
    hasher.finalize().into()
}

/// 验证 Server 认证是否匹配
pub fn verify_server_auth(
    status: u32,
    client_auth: &[u8; 16],
    server_auth: &[u8; 16],
    password: &str,
) -> bool {
    let expected = compute_server_auth(status, client_auth, password);
    expected == *server_auth
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_login_auth_deterministic() {
        let auth1 = compute_login_auth("SP001", "test", 0x04051200);
        let auth2 = compute_login_auth("SP001", "test", 0x04051200);
        assert_eq!(auth1, auth2);
        assert_eq!(auth1.len(), 16);
    }

    #[test]
    fn compute_login_auth_different_inputs() {
        let auth1 = compute_login_auth("SP001", "test", 0x04051200);
        let auth2 = compute_login_auth("SP002", "test", 0x04051200);
        assert_ne!(auth1, auth2);
    }

    #[test]
    fn compute_server_auth_roundtrip() {
        let client_auth = compute_login_auth("SP001", "test", 0x04051200);
        let server_auth = compute_server_auth(0, &client_auth, "test");
        assert!(verify_server_auth(0, &client_auth, &server_auth, "test"));
    }

    #[test]
    fn verify_server_auth_wrong_password() {
        let client_auth = compute_login_auth("SP001", "test", 0x04051200);
        let server_auth = compute_server_auth(0, &client_auth, "test");
        assert!(!verify_server_auth(0, &client_auth, &server_auth, "wrong"));
    }
}
