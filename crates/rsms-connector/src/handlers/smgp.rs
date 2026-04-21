use async_trait::async_trait;
use bytes::BytesMut;
use rsms_codec_smgp::{
    SmgpMessage, decode_message, CommandId, Encodable, LoginResp, 
    ActiveTestResp, DeliverResp,
};
use rsms_core::{Frame, Result};
use std::sync::Arc;

use crate::protocol::{
    ProtocolConnection, AuthCredentials, AuthHandler, AuthResult, HandleResult,
};

pub struct SmgpHandler {
    auth_handler: Option<Arc<dyn AuthHandler>>,
}

impl SmgpHandler {
    pub fn new(auth_handler: Option<Arc<dyn AuthHandler>>) -> Self {
        Self { auth_handler }
    }
}

fn encode_pdu_header(command_id: CommandId, sequence_id: u32, body_len: usize) -> Vec<u8> {
    let total_len = 12 + body_len;
    let mut pdu = Vec::with_capacity(total_len);
    pdu.extend_from_slice(&(total_len as u32).to_be_bytes());
    pdu.extend_from_slice(&(command_id as u32).to_be_bytes());
    pdu.extend_from_slice(&sequence_id.to_be_bytes());
    pdu
}

#[async_trait]
impl crate::protocol::ProtocolHandler for SmgpHandler {
    fn name(&self) -> &'static str {
        "smgp-handler"
    }

    async fn handle_frame(&self, frame: &Frame, conn: Arc<dyn ProtocolConnection>) -> Result<HandleResult> {
        let frame_bytes = frame.data_as_slice();
        let msg = match decode_message(frame_bytes) {
            Ok(m) => m,
            Err(_) => {
                return Ok(HandleResult::Stop);
            }
        };

        match msg {
            SmgpMessage::Login { sequence_id, login: l } => {
                let client_id = &l.client_id;
                tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "收到SMGP登录请求: client_id={}, version={:02x}", client_id, l.version);
                
                let credentials = AuthCredentials::Smgp {
                    client_id: client_id.clone(),
                    authenticator: l.authenticator,
                    version: l.version,
                };
                
                let auth_result = if let Some(ref handler) = self.auth_handler {
                    let conn_info = conn.connection_info().await;
                    handler.authenticate(client_id, credentials, &conn_info).await
                } else {
                    Ok(AuthResult::success(client_id.clone()))
                };
                
                match auth_result {
                    Ok(result) if result.status == 0 => {
                        tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "SMGP认证成功: account={}", result.account);
                        conn.set_authenticated_account(result.account.clone()).await;
                        
                        let resp = LoginResp {
                            status: 0,
                            authenticator: [0u8; 16],
                            version: l.version,
                        };
                        let mut body = BytesMut::new();
                        resp.encode(&mut body).unwrap();
                        
                        let mut pdu = encode_pdu_header(CommandId::LoginResp, sequence_id, body.len());
                        pdu.extend_from_slice(&body);
                        conn.write_frame(&pdu).await?;
                        tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "发送SMGP登录响应: status=0");
                        return Ok(HandleResult::Continue);
                    }
                    Ok(result) => {
                        tracing::warn!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "SMGP认证失败: status={}", result.status);
                        let resp = LoginResp {
                            status: result.status,
                            authenticator: [0u8; 16],
                            version: l.version,
                        };
                        let mut body = BytesMut::new();
                        resp.encode(&mut body).unwrap();
                        
                        let mut pdu = encode_pdu_header(CommandId::LoginResp, sequence_id, body.len());
                        pdu.extend_from_slice(&body);
                        conn.write_frame(&pdu).await?;
                        return Ok(HandleResult::Stop);
                    }
                    Err(e) => {
                        tracing::error!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "SMGP认证错误: {}", e);
                        let resp = LoginResp {
                            status: 1,
                            authenticator: [0u8; 16],
                            version: l.version,
                        };
                        let mut body = BytesMut::new();
                        resp.encode(&mut body).unwrap();
                        
                        let mut pdu = encode_pdu_header(CommandId::LoginResp, sequence_id, body.len());
                        pdu.extend_from_slice(&body);
                        conn.write_frame(&pdu).await?;
                        return Ok(HandleResult::Stop);
                    }
                }
            }
            SmgpMessage::Submit { sequence_id, submit: _ } => {
                if conn.should_log(tracing::Level::DEBUG) {
                    tracing::debug!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "收到SMGP短信提交: seq_id={}", sequence_id);
                }
                return Ok(HandleResult::Continue);
            }
            SmgpMessage::ActiveTest { sequence_id } => {
                tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "收到SMGP活性检测: seq_id={}", sequence_id);
                
                let resp = ActiveTestResp { reserved: 0 };
                let mut body = BytesMut::new();
                resp.encode(&mut body).unwrap();
                
                let mut pdu = encode_pdu_header(CommandId::ActiveTestResp, sequence_id, body.len());
                pdu.extend_from_slice(&body);
                conn.write_frame(&pdu).await?;
                tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "发送SMGP活性检测响应");
                return Ok(HandleResult::Continue);
            }
            SmgpMessage::Deliver { sequence_id, deliver: _ } => {
                if conn.should_log(tracing::Level::INFO) {
                    tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "收到SMGP Deliver消息: seq_id={}", sequence_id);
                }
                
                let resp = DeliverResp { status: 0 };
                let mut body = BytesMut::new();
                resp.encode(&mut body).unwrap();
                
                let mut pdu = encode_pdu_header(CommandId::DeliverResp, sequence_id, body.len());
                pdu.extend_from_slice(&body);
                conn.write_frame(&pdu).await?;
                tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "发送SMGP Deliver响应: status=0");
                return Ok(HandleResult::Continue);
            }
            SmgpMessage::Exit { sequence_id } => {
                tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "收到SMGP退出请求: seq_id={}", sequence_id);
                
                let resp = rsms_codec_smgp::ExitResp { reserved: 0 };
                let mut body = BytesMut::new();
                resp.encode(&mut body).unwrap();
                
                let mut pdu = encode_pdu_header(CommandId::ExitResp, sequence_id, body.len());
                pdu.extend_from_slice(&body);
                conn.write_frame(&pdu).await?;
                tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "发送SMGP退出响应");
                return Ok(HandleResult::Stop);
            }
            _ => return Ok(HandleResult::Stop),
        }
    }
}