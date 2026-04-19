//! Session configuration

use super::protocol_decoder::ProtocolType;

/// Session configuration
#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub protocol: ProtocolType,
    pub require_login: bool,
    pub heartbeat_timeout: u32,
}

impl SessionConfig {
    pub fn new(protocol: ProtocolType) -> Self {
        Self {
            protocol,
            require_login: true,
            heartbeat_timeout: 60,
        }
    }
}
