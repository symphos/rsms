use std::net::SocketAddr;
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    pub peer_addr: Option<SocketAddr>,
    pub local_addr: Option<SocketAddr>,
    pub connected_at: Instant,
}

impl ConnectionInfo {
    pub fn new(peer_addr: Option<SocketAddr>, local_addr: Option<SocketAddr>) -> Self {
        Self {
            peer_addr,
            local_addr,
            connected_at: Instant::now(),
        }
    }

    pub fn unknown() -> Self {
        Self {
            peer_addr: None,
            local_addr: None,
            connected_at: Instant::now(),
        }
    }
}
