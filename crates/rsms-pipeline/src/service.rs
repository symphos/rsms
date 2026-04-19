//! Core service types for pipeline

use bytes::Bytes;
use std::any::Any;
use std::net::SocketAddr;
use async_trait::async_trait;

/// Protocol PDU trait - 统一不同协议的PDU
pub trait ProtocolPdu: Send + Sync {
    fn command_id(&self) -> u32;
    fn as_any(&self) -> &dyn Any;
}

/// Incoming PDU request
#[derive(Debug, Clone)]
pub struct PduRequest {
    pub data: Bytes,
    pub remote_addr: SocketAddr,
    pub proxy_info: Option<ProxyInfo>,
}

impl PduRequest {
    pub fn new(data: Bytes, remote_addr: SocketAddr) -> Self {
        Self { data, remote_addr, proxy_info: None }
    }
    
    pub fn with_proxy(data: Bytes, remote_addr: SocketAddr, proxy: ProxyInfo) -> Self {
        Self { data, remote_addr, proxy_info: Some(proxy) }
    }
}

/// HAProxy 协议信息
#[derive(Debug, Clone)]
pub struct ProxyInfo {
    pub protocol: ProxyProtocol,
    pub source_addr: SocketAddr,
    pub dest_addr: SocketAddr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyProtocol {
    V1,
    V2,
}

/// Outgoing PDU response
pub enum PduResponse {
    Pdu(Box<dyn ProtocolPdu>),
    None,
    Close,
    Raw(Bytes),
}

impl std::fmt::Debug for PduResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PduResponse::Pdu(_) => write!(f, "Pdu(...)"),
            PduResponse::None => write!(f, "None"),
            PduResponse::Close => write!(f, "Close"),
            PduResponse::Raw(b) => write!(f, "Raw({} bytes)", b.len()),
        }
    }
}

impl Clone for PduResponse {
    fn clone(&self) -> Self {
        match self {
            PduResponse::Pdu(_) => PduResponse::None,  // 无法 Clone 具体 PDU
            PduResponse::None => PduResponse::None,
            PduResponse::Close => PduResponse::Close,
            PduResponse::Raw(b) => PduResponse::Raw(b.clone()),
        }
    }
}

impl PduResponse {
    pub fn raw(data: impl Into<Bytes>) -> Self {
        PduResponse::Raw(data.into())
    }
    
    pub fn is_none(&self) -> bool {
        matches!(self, PduResponse::None)
    }
}

/// Pipeline Service trait
#[async_trait]
pub trait PduService: Send + Sync {
    async fn call(&self, req: PduRequest) -> PipelineResult<PduResponse>;
}

use crate::error::PipelineResult;

/// 空实现的 Service
#[derive(Clone)]
pub struct IdentityService;

#[async_trait]
impl PduService for IdentityService {
    async fn call(&self, _req: PduRequest) -> PipelineResult<PduResponse> {
        Ok(PduResponse::None)
    }
}