use md5::{Digest, Md5};

/// 计算 CMPP Connect 请求的 AuthenticatorSource
///
/// AuthenticatorSource = MD5(SourceAddr + 9个零字节 + Password + Timestamp)
///
/// - SourceAddr: 6字节 SP 企业代码
/// - Password: 明文密码
/// - Timestamp: 4字节大端 MMDDHHMMSS 格式
pub fn compute_connect_auth(source_addr: &str, password: &str, timestamp: u32) -> [u8; 16] {
    let mut hasher = Md5::new();

    // SourceAddr (6 bytes, space-padded)
    let mut addr_buf = [b' '; 6];
    let addr_bytes = source_addr.as_bytes();
    let copy_len = addr_bytes.len().min(6);
    addr_buf[..copy_len].copy_from_slice(&addr_bytes[..copy_len]);
    hasher.update(addr_buf);

    // 9 null bytes
    hasher.update([0u8; 9]);

    // Password
    hasher.update(password.as_bytes());

    // Timestamp (4 bytes big-endian)
    hasher.update(timestamp.to_be_bytes());

    hasher.finalize().into()
}

/// 验证 CMPP Connect 响应的 AuthenticatorISMG
///
/// AuthenticatorISMG = MD5(Status + AuthenticatorSource + Password)
///
/// - Status: 4字节大端
/// - AuthenticatorSource: 16字节（来自请求）
/// - Password: 明文密码
pub fn compute_ismg_auth(status: u32, authenticator_source: &[u8; 16], password: &str) -> [u8; 16] {
    let mut hasher = Md5::new();
    hasher.update(status.to_be_bytes());
    hasher.update(authenticator_source);
    hasher.update(password.as_bytes());
    hasher.finalize().into()
}

/// 验证 ISMG 认证是否匹配
pub fn verify_ismg_auth(
    status: u32,
    authenticator_source: &[u8; 16],
    authenticator_ismg: &[u8; 16],
    password: &str,
) -> bool {
    let expected = compute_ismg_auth(status, authenticator_source, password);
    expected == *authenticator_ismg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_connect_auth_deterministic() {
        let auth1 = compute_connect_auth("900001", "test", 0x04051200);
        let auth2 = compute_connect_auth("900001", "test", 0x04051200);
        assert_eq!(auth1, auth2);
        assert_eq!(auth1.len(), 16);
    }

    #[test]
    fn compute_connect_auth_different_inputs() {
        let auth1 = compute_connect_auth("900001", "test", 0x04051200);
        let auth2 = compute_connect_auth("900002", "test", 0x04051200);
        assert_ne!(auth1, auth2);
    }

    #[test]
    fn compute_ismg_auth_roundtrip() {
        let auth_source = compute_connect_auth("900001", "test", 0x04051200);
        let ismg_auth = compute_ismg_auth(0, &auth_source, "test");
        assert!(verify_ismg_auth(0, &auth_source, &ismg_auth, "test"));
    }

    #[test]
    fn verify_ismg_auth_wrong_password() {
        let auth_source = compute_connect_auth("900001", "test", 0x04051200);
        let ismg_auth = compute_ismg_auth(0, &auth_source, "test");
        assert!(!verify_ismg_auth(0, &auth_source, &ismg_auth, "wrong"));
    }
}
