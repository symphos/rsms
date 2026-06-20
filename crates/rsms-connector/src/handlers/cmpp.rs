use async_trait::async_trait;
use bytes::BytesMut;
use rsms_codec_cmpp::{
    CmppMessage, CommandId, CommandStatus, ConnectResp, Encodable,
    decode_message_with_version, encode_message,
};
use rsms_core::{Frame, Result};
use std::sync::Arc;

use crate::protocol::{
    AuthCredentials, AuthHandler, AuthResult, HandleResult, ProtocolConnection,
    RESPONSE_COMMAND_MASK,
};

const CMPP_VERSION_2_0: u8 = 0x20;
const CMPP_VERSION_3_0: u8 = 0x30;

fn encode_pdu_header(command_id: CommandId, sequence_id: u32, body_len: usize) -> Vec<u8> {
    let total_len = 12 + body_len;
    let mut pdu = Vec::with_capacity(total_len);
    pdu.extend_from_slice(&(total_len as u32).to_be_bytes());
    pdu.extend_from_slice(&(command_id as u32).to_be_bytes());
    pdu.extend_from_slice(&sequence_id.to_be_bytes());
    pdu
}

fn encode_error_response(command_id: u32, sequence_id: u32, status: CommandStatus) -> Vec<u8> {
    let resp_command_id = command_id | RESPONSE_COMMAND_MASK;
    let body_len = 4;
    let total_len = 12 + body_len;
    let mut pdu = Vec::with_capacity(total_len);
    pdu.extend_from_slice(&(total_len as u32).to_be_bytes());
    pdu.extend_from_slice(&resp_command_id.to_be_bytes());
    pdu.extend_from_slice(&sequence_id.to_be_bytes());
    pdu.extend_from_slice(&status.as_u32().to_be_bytes());
    pdu
}

fn is_version_supported(version: u8) -> bool {
    matches!(version, CMPP_VERSION_2_0 | CMPP_VERSION_3_0 | 0x00 | 0x01 | 0x7F)
}

/// CMPP 2.0 形态判定：2.0 及历史上以 0x00/0x01 标注的 2.0 实现，应答/回执按 V2.0 窄字段编码。
fn is_cmpp_v2(version: u8) -> bool {
    matches!(version, CMPP_VERSION_2_0 | 0x00 | 0x01)
}

/// 构造并发送 CMPP ConnectResp（version-unsupported 与三种认证结果共用）。
async fn send_connect_resp(
    conn: &Arc<dyn ProtocolConnection>,
    sequence_id: u32,
    version: u8,
    status: u32,
) -> Result<()> {
    // CMPP 2.0 的 ConnectResp Status 仅 1 字节（body=1+16+1=18B），与 V3.0 的 Status u32
    // （body=4+16+1=21B）不同。V2.0（0x20/0x00/0x01）手写 18B body；否则走 V3.0 codec。
    let is_v2 = is_cmpp_v2(version);
    if is_v2 {
        let body_len = 1 + 16 + 1;
        let mut pdu = encode_pdu_header(CommandId::ConnectResp, sequence_id, body_len);
        pdu.push(status as u8);
        pdu.extend_from_slice(&[0u8; 16]);
        pdu.push(version);
        return conn.write_frame(&pdu).await;
    }
    let resp = ConnectResp {
        status,
        authenticator_ismg: [0u8; 16],
        version,
    };
    let mut body = BytesMut::new();
    resp.encode(&mut body)
        .map_err(|e| rsms_core::RsmsError::Codec(e.to_string()))?;
    let mut pdu = encode_pdu_header(CommandId::ConnectResp, sequence_id, body.len());
    pdu.extend_from_slice(&body);
    conn.write_frame(&pdu).await
}

pub struct CmppHandler {
    auth_handler: Option<Arc<dyn AuthHandler>>,
}

impl CmppHandler {
    pub fn new(auth_handler: Option<Arc<dyn AuthHandler>>) -> Self {
        Self { auth_handler }
    }
}

#[async_trait]
impl crate::protocol::ProtocolHandler for CmppHandler {
    fn name(&self) -> &'static str {
        "cmpp-handler"
    }

    async fn handle_frame(&self, frame: &Frame, conn: Arc<dyn ProtocolConnection>) -> Result<HandleResult> {
        let frame_bytes = frame.data_as_slice();
        let version = conn.protocol_version().await;
        let msg = match decode_message_with_version(frame_bytes, version) {
            Ok(m) => m,
            Err(e) => {
                tracing::error!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "CMPP 消息解码失败: {e}, frame_len={}, cmd_id={:#x}, seq_id={}", 
                    frame_bytes.len(), frame.command_id, frame.sequence_id);
                tracing::debug!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "原始数据 (hex): {:02x?}", frame_bytes);

                
                if frame.len() >= 12 {
                    let error_pdu = encode_error_response(
                        frame.command_id,
                        frame.sequence_id,
                        CommandStatus::ESME_RINVSYNTAX,
                    );
                    let _ = conn.write_frame(&error_pdu).await;
                }
                return Ok(HandleResult::Stop);
            }
        };

        match msg {
            CmppMessage::Connect { version: _, sequence_id, connect: c } => {
                let source = &c.source_addr;
                tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "收到CMPP连接请求: source_addr={}, version={:02x}", source, c.version);
                
                if !is_version_supported(c.version) {
                    tracing::warn!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "不支持的CMPP版本: {:02x}", c.version);
                    send_connect_resp(&conn, sequence_id, CMPP_VERSION_3_0, 1).await?;
                    return Ok(HandleResult::Stop);
                }
                
                conn.set_protocol_version(c.version).await;
                
                let is_v2 = is_cmpp_v2(c.version);
                tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "CMPP版本: {} (V2.0={})", c.version, is_v2);
                
                let credentials = AuthCredentials::Cmpp {
                    source_addr: source.clone(),
                    authenticator_source: c.authenticator_source,
                    version: c.version,
                    timestamp: c.timestamp,
                };
                
                let auth_result = if let Some(ref handler) = self.auth_handler {
                    let conn_info = conn.connection_info().await;
                    handler.authenticate(source, credentials, &conn_info).await
                } else {
                    Ok(AuthResult::success(source.clone()))
                };
                
                match auth_result {
                    Ok(result) if result.status == 0 => {
                        tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "CMPP认证成功: account={}", result.account);
                        conn.set_authenticated_account(result.account.clone()).await;
                        send_connect_resp(&conn, sequence_id, c.version, 0).await?;
                        tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "发送CMPP连接响应: status=0");
                        return Ok(HandleResult::Continue);
                    }
                    Ok(result) => {
                        tracing::warn!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "CMPP认证失败: status={}, message={:?}", result.status, result.message);
                        send_connect_resp(&conn, sequence_id, c.version, result.status).await?;
                        return Ok(HandleResult::Stop);
                    }
                    Err(e) => {
                        tracing::error!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "CMPP认证错误: {}", e);
                        send_connect_resp(&conn, sequence_id, c.version, 1).await?;
                        return Ok(HandleResult::Stop);
                    }
                }
            }
            CmppMessage::SubmitV20 { .. } | CmppMessage::SubmitV30 { .. } => {
                if conn.should_log(tracing::Level::DEBUG) {
                    tracing::debug!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "收到CMPP短信提交");
                }
                return Ok(HandleResult::Continue);
            }
            CmppMessage::Terminate { version, sequence_id } => {
                tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "收到CMPP终止请求: sequence_id={}", sequence_id);
                
                let resp = CmppMessage::TerminateResp { version, sequence_id };
                if let Ok(pdu) = encode_message(&resp) {
                    conn.write_frame(&pdu).await?;
                    tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "发送CMPP终止响应");
                }
                return Ok(HandleResult::Stop);
            }
            CmppMessage::DeliverV20 { .. } | CmppMessage::DeliverV30 { .. } => {
                if conn.should_log(tracing::Level::INFO) {
                    tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "收到CMPP Deliver (上行/状态报告)");
                }
                return Ok(HandleResult::Continue);
            }
            _ => return Ok(HandleResult::Stop),
        }
    }
}