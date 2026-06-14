use md5::{Digest, Md5};

/// 计算 CMPP Connect 请求的 AuthenticatorSource
///
/// AuthenticatorSource = MD5(SourceAddr原始 + 9个零字节 + Password + Timestamp的10位十进制字符串)
///
/// 对齐 lihuanghe/SMSGate `SessionLoginManager.validClientMsg`：SourceAddr 用登录名**原始字节**
/// （不补齐到 6B），Timestamp 用 `String.format("%010d", ts)` 的 ASCII 串（**非 4 字节整数**）。
/// 2026-06-14 经 cmos 模拟器联调验证。
pub fn compute_connect_auth(source_addr: &str, password: &str, timestamp: u32) -> [u8; 16] {
    let mut hasher = Md5::new();
    hasher.update(source_addr.as_bytes());
    hasher.update([0u8; 9]);
    hasher.update(password.as_bytes());
    hasher.update(format!("{timestamp:010}").as_bytes());
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
