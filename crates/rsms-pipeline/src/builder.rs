//! Pipeline Builder - 方便构建 Pipeline

use std::net::SocketAddr;
use bytes::Bytes;
use std::pin::Pin;
use std::future::Future;

use crate::service::{PduRequest, PduResponse};
use crate::error::{PipelineResult, PipelineError};
use crate::handlers::FrameConfig;

/// Pipeline configuration
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    pub protocol: crate::handlers::ProtocolType,
    pub haproxy_enabled: bool,
    pub frame_config: FrameConfig,
    pub session_config: crate::handlers::SessionConfig,
    pub rate_limit_config: Option<crate::handlers::RateLimitConfig>,
}

impl PipelineConfig {
    pub fn new(protocol: crate::handlers::ProtocolType) -> Self {
        Self {
            protocol,
            haproxy_enabled: false,
            frame_config: FrameConfig::cmpp(),
            session_config: crate::handlers::SessionConfig::new(protocol),
            rate_limit_config: None,
        }
    }
    
    pub fn cmpp() -> Self {
        Self::new(crate::handlers::ProtocolType::Cmpp)
    }
    
    pub fn sgip() -> Self {
        let mut config = Self::new(crate::handlers::ProtocolType::Sgip);
        config.frame_config = FrameConfig::sgip();
        config
    }
    
    pub fn smgp() -> Self {
        let mut config = Self::new(crate::handlers::ProtocolType::Smgp);
        config.frame_config = FrameConfig::cmpp();
        config
    }
    
    pub fn smpp() -> Self {
        let mut config = Self::new(crate::handlers::ProtocolType::Smpp);
        config.frame_config = FrameConfig::smpp();
        config
    }
    
    #[allow(dead_code)]
    pub fn with_haproxy(mut self, enabled: bool) -> Self {
        self.haproxy_enabled = enabled;
        self
    }
    
    #[allow(dead_code)]
    pub fn with_rate_limit(mut self, qps: u64) -> Self {
        self.rate_limit_config = Some(crate::handlers::RateLimitConfig::new(qps, qps));
        self
    }
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self::cmpp()
    }
}

/// Pipeline Builder
pub struct PipelineBuilder {
    config: PipelineConfig,
    handlers: Vec<Box<dyn Handler>>,
}

/// Handler trait for pipeline stages
pub trait Handler: Send + Sync {
    fn name(&self) -> &str;
    fn handle(&self, req: PduRequest) -> BoxFuture<PipelineResult<PduResponse>>;
}

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

impl PipelineBuilder {
    pub fn new(config: PipelineConfig) -> Self {
        Self {
            config,
            handlers: Vec::new(),
        }
    }
    
    pub fn cmpp() -> Self {
        Self::new(PipelineConfig::cmpp())
    }
    
    pub fn sgip() -> Self {
        Self::new(PipelineConfig::sgip())
    }
    
    pub fn smgp() -> Self {
        Self::new(PipelineConfig::smgp())
    }
    
    pub fn smpp() -> Self {
        Self::new(PipelineConfig::smpp())
    }
    
    pub fn with_haproxy(mut self, enabled: bool) -> Self {
        self.config.haproxy_enabled = enabled;
        self
    }
    
    pub fn with_rate_limit(mut self, qps: u64) -> Self {
        self.config.rate_limit_config = Some(crate::handlers::RateLimitConfig::new(qps, qps));
        self
    }
    
    /// 添加自定义处理器
    pub fn with_handler<H: Handler + 'static>(mut self, handler: H) -> Self {
        self.handlers.push(Box::new(handler));
        self
    }
    
    /// 添加带闭包的处理器
    pub fn with_closure(self, name: &'static str, handler: impl Fn(PduRequest) -> BoxFuture<PipelineResult<PduResponse>> + Send + Sync + 'static) -> Self {
        self.with_handler(ClosureHandler { name, handler })
    }
    
    /// 构建 Pipeline
    pub fn build(self) -> SimplePipeline {
        SimplePipeline {
            config: self.config,
            handlers: self.handlers,
        }
    }
}

/// 简单 Pipeline 实现
pub struct SimplePipeline {
    config: PipelineConfig,
    handlers: Vec<Box<dyn Handler>>,
}

impl SimplePipeline {
    pub async fn call(&self, req: PduRequest) -> PipelineResult<PduResponse> {
        // 1. HAProxy 解码 (如果启用)
        let req = if self.config.haproxy_enabled {
            decode_haproxy(req).await?
        } else {
            req
        };
        
        // 2. Frame 解码 - 循环解帧
        let frames = decode_frames(&req, &self.config.frame_config)?;
        
        if frames.is_empty() {
            return Ok(PduResponse::None);
        }
        
        // 3. 顺序执行自定义 handlers
        let mut final_resp = PduResponse::None;
        
        for handler in &self.handlers {
            let frame_req = PduRequest::new(
                frames.first().cloned().unwrap_or_default(),
                req.remote_addr,
            );
            let resp = handler.handle(frame_req).await?;
            if !resp.is_none() {
                final_resp = resp;
            }
        }
        
        Ok(final_resp)
    }
}

/// 闭包处理器包装
struct ClosureHandler<F> {
    name: &'static str,
    handler: F,
}

impl<F> Handler for ClosureHandler<F>
where
    F: Fn(PduRequest) -> BoxFuture<PipelineResult<PduResponse>> + Send + Sync + 'static,
{
    fn name(&self) -> &str {
        self.name
    }
    
    fn handle(&self, req: PduRequest) -> BoxFuture<PipelineResult<PduResponse>> {
        (self.handler)(req)
    }
}

/// HAProxy 解码
async fn decode_haproxy(mut req: PduRequest) -> PipelineResult<PduRequest> {
    let data = req.data.clone();
    if data.len() < 8 {
        return Ok(req);
    }
    
    // 检测 V2 (二进制)
    if data[0] == 0x21 && data.len() >= 16 {
        return Ok(req);
    }
    
    // 检测 V1 (文本)
    if data.starts_with(b"PROXY ") {
        let line_end = data[6..].iter()
            .position(|&b| b == b'\r')
            .and_then(|pos| {
                if data.get(6 + pos + 1) == Some(&b'\n') {
                    Some(6 + pos)
                } else {
                    None
                }
            })
            .ok_or_else(|| PipelineError::Decoding("Invalid HAProxy V1 format".to_string()))?;
        
        let line = std::str::from_utf8(&data[6..line_end])
            .map_err(|e| PipelineError::Decoding(format!("HAProxy V1 UTF8: {}", e)))?;
        
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 6 && parts[1] == "TCP4" {
            let source: SocketAddr = format!("{}:{}", parts[2], parts[3])
                .parse()
                .map_err(|e| PipelineError::Decoding(format!("HAProxy source: {}", e)))?;
            req.remote_addr = source;
        }
    }
    
    Ok(req)
}

/// Frame 解码 - 循环解帧
fn decode_frames(req: &PduRequest, config: &FrameConfig) -> PipelineResult<Vec<Bytes>> {
    let mut frames = Vec::new();
    let mut data = req.data.as_ref();
    
    while data.len() >= config.length_field_offset + config.length_field_length {
        let length = match config.length_field_length {
            2 => u16::from_be_bytes([data[0], data[1]]) as usize,
            4 => u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize,
            _ => return Err(PipelineError::Decoding("Invalid length field".to_string())),
        };
        
        let frame_length = (length as isize + config.length_adjustment) as usize;
        
        if frame_length > config.max_frame_length {
            return Err(PipelineError::Decoding(format!("Frame too large: {}", frame_length)));
        }
        
        if data.len() < frame_length {
            break;
        }
        
        frames.push(Bytes::copy_from_slice(&data[..frame_length]));
        data = &data[frame_length..];
    }
    
    Ok(frames)
}

/// Helper: 创建一个空 handler
pub fn empty_handler() -> impl Fn(PduRequest) -> BoxFuture<PipelineResult<PduResponse>> + Send + Sync + 'static {
    |_req| Box::pin(async { Ok(PduResponse::None) })
}

/// Helper: 创建一个日志 handler
pub fn logging_handler() -> impl Fn(PduRequest) -> BoxFuture<PipelineResult<PduResponse>> + Send + Sync + 'static {
    |req| {
        Box::pin(async move {
            println!("Received {} bytes from {}", req.data.len(), req.remote_addr);
            Ok(PduResponse::None)
        })
    }
}