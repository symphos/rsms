//! Frame Decoder Handler - 粘包处理

use bytes::{Bytes, BytesMut};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Frame decoder configuration
#[derive(Debug, Clone)]
pub struct FrameConfig {
    pub length_field_offset: usize,
    pub length_field_length: usize,
    pub length_adjustment: isize,
    pub max_frame_length: usize,
}

impl FrameConfig {
    pub fn cmpp() -> Self {
        Self {
            length_field_offset: 0,
            length_field_length: 4,
            length_adjustment: 0,
            max_frame_length: 1024 * 1024,
        }
    }
    
    pub fn sgip() -> Self {
        Self {
            length_field_offset: 0,
            length_field_length: 4,
            length_adjustment: 0,
            max_frame_length: 1024 * 1024,
        }
    }
    
    pub fn smpp() -> Self {
        Self {
            length_field_offset: 0,
            length_field_length: 4,
            length_adjustment: 0,
            max_frame_length: 1024 * 1024,
        }
    }
}

/// Frame decoder state
#[derive(Clone)]
pub struct FrameDecoder {
    config: FrameConfig,
    buffer: Arc<Mutex<BytesMut>>,
}

impl FrameDecoder {
    pub fn new(config: FrameConfig) -> Self {
        Self {
            config,
            buffer: Arc::new(Mutex::new(BytesMut::new())),
        }
    }
    
    pub fn cmpp() -> Self {
        Self::new(FrameConfig::cmpp())
    }
    
    pub fn sgip() -> Self {
        Self::new(FrameConfig::sgip())
    }
    
    pub fn smpp() -> Self {
        Self::new(FrameConfig::smpp())
    }
    
    /// 解码一帧或多帧
    pub async fn decode(&self, data: Bytes) -> Result<Vec<Bytes>, String> {
        let mut buf = self.buffer.lock().await;
        buf.extend_from_slice(&data);
        
        let mut frames = Vec::new();
        
        while buf.len() >= self.config.length_field_offset + self.config.length_field_length {
            let length = match self.config.length_field_length {
                2 => u16::from_be_bytes([buf[0], buf[1]]) as usize,
                4 => u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize,
                _ => return Err("Invalid length field".to_string()),
            };
            
            let frame_length = (length as isize + self.config.length_adjustment) as usize;
            
            if frame_length > self.config.max_frame_length {
                return Err(format!("Frame too large: {}", frame_length));
            }
            
            if buf.len() < frame_length {
                break;
            }
            
            frames.push(buf.split_to(frame_length).freeze());
        }
        
        Ok(frames)
    }
    
    /// 重置 buffer
    pub async fn reset(&self) {
        let mut buf = self.buffer.lock().await;
        buf.clear();
    }
}