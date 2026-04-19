use md5::{Digest, Md5};

/// 计算 SMGP Login 请求的 ClientAuth
///
/// ClientAuth = MD5(ClientId + 7个零字节 + Password + Timestamp)
///
/// - ClientId: 8字节 SP 企业代码
/// - Password: 明文密码
/// - Timestamp: 4字节大端 MMDDHHMMSS 格式
pub fn compute_login_auth(client_id: &str, password: &str, timestamp: u32) -> [u8; 16] {
    let mut hasher = Md5::new();

    let mut id_buf = [b' '; 8];
    let id_bytes = client_id.as_bytes();
    let copy_len = id_bytes.len().min(8);
    id_buf[..copy_len].copy_from_slice(&id_bytes[..copy_len]);
    hasher.update(id_buf);

    hasher.update([0u8; 7]);

    hasher.update(password.as_bytes());

    hasher.update(timestamp.to_be_bytes());

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
