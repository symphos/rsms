use async_trait::async_trait;
use bytes::BytesMut;
use rsms_codec_sgip::{SgipMessage, CommandId, CommandStatus, Encodable, BindResp, SubmitResp as SgipSubmitResp, ReportResp, DeliverResp, UnbindResp, decode_message};
use rsms_core::{Frame, Result};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::protocol::{
    AuthCredentials, AuthHandler, AuthResult, HandleResult, Protocol, ProtocolConnection,
    RESPONSE_COMMAND_MASK,
};

static NEXT_MSG_ID: AtomicU64 = AtomicU64::new(1);

pub struct SgipProtocol;

impl SgipProtocol {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SgipProtocol {
    fn default() -> Self {
        Self::new()
    }
}

impl Protocol for SgipProtocol {
    type Submit = SgipMessage;
    type SubmitResp = SgipSubmitResp;
    type MsgId = ();
    type Deliver = SgipMessage;

    fn name(&self) -> &'static str {
        "sgip"
    }

    fn next_msg_id(&self) -> u64 {
        NEXT_MSG_ID.fetch_add(1, Ordering::Relaxed)
    }

    fn encode_submit_resp(
        &self,
        _sequence_id: u32,
        _msg_id: &Self::MsgId,
        result: u32,
    ) -> Vec<u8> {
        let resp = SgipSubmitResp { result };
        let mut body = BytesMut::new();
        resp.encode(&mut body).unwrap();
        let mut pdu = encode_sgip_pdu_header(CommandId::SubmitResp, body.len());
        pdu.extend_from_slice(&body);
        pdu
    }
}

pub struct SgipHandler {
    auth_handler: Option<Arc<dyn AuthHandler>>,
}

impl SgipHandler {
    pub fn new(auth_handler: Option<Arc<dyn AuthHandler>>) -> Self {
        Self { auth_handler }
    }
}

fn encode_sgip_pdu_header(command_id: CommandId, body_len: usize) -> Vec<u8> {
    let total_len = 20 + body_len;
    let mut pdu = Vec::with_capacity(total_len);
    pdu.extend_from_slice(&(total_len as u32).to_be_bytes());
    pdu.extend_from_slice(&(command_id as u32).to_be_bytes());
    pdu.extend_from_slice(&[0u8; 12]);
    pdu
}

fn encode_sgip_error_response(command_id: u32, status: CommandStatus) -> Vec<u8> {
    let resp_command_id = command_id | RESPONSE_COMMAND_MASK;
    let body_len = 4;
    let total_len = 20 + body_len;
    let mut pdu = Vec::with_capacity(total_len);
    pdu.extend_from_slice(&total_len.to_be_bytes());
    pdu.extend_from_slice(&resp_command_id.to_be_bytes());
    pdu.extend_from_slice(&[0u8; 12]);
    pdu.extend_from_slice(&status.as_u32().to_be_bytes());
    pdu
}

#[async_trait]
impl crate::protocol::ProtocolHandler for SgipHandler {
    fn name(&self) -> &'static str {
        "sgip-handler"
    }

    async fn handle_frame(&self, frame: &Frame, conn: Arc<dyn ProtocolConnection>) -> Result<HandleResult> {
        let frame_bytes = frame.data_as_slice();
        let msg = match decode_message(frame_bytes) {
            Ok(m) => m,
            Err(e) => {
                tracing::error!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "SGIP 消息解码失败: {e}");
                tracing::debug!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "原始数据 (hex): {:02x?}", frame_bytes);
                
                if frame.len() >= 20 {
                    let error_pdu = encode_sgip_error_response(
                        frame.command_id,
                        CommandStatus::ESME_RINVSYNTAX,
                    );
                    let _ = conn.write_frame(&error_pdu).await;
                }
                return Ok(HandleResult::Stop);
            }
        };

        match msg {
             SgipMessage::Bind(b) => {
                 tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "收到SGIP登录请求: login_name={}, login_type={}", b.login_name, b.login_type);
                 
                 let credentials = AuthCredentials::Sgip {
                     login_name: b.login_name.clone(),
                     login_password: b.login_password.clone(),
                 };
                 
                 let auth_result = if let Some(ref handler) = self.auth_handler {
                     let conn_info = conn.connection_info().await;
                     handler.authenticate(&b.login_name, credentials, &conn_info).await
                 } else {
                     Ok(AuthResult::success(b.login_name.clone()))
                 };
                 
                 match auth_result {
                     Ok(result) if result.status == 0 => {
                         tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "SGIP认证成功: account={}", result.account);
                         conn.set_authenticated_account(result.account.clone()).await;
                         
                         let resp = BindResp {
                             result: 0,
                             reserve: [0u8; 8],
                         };
                         let mut body = BytesMut::new();
                         resp.encode(&mut body).unwrap();
                         
                         let mut pdu = encode_sgip_pdu_header(CommandId::BindResp, body.len());
                         pdu.extend_from_slice(&body);
                         conn.write_frame(&pdu).await?;
                         tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "发送SGIP登录响应: result=0");
                         return Ok(HandleResult::Continue);
                     }
                     Ok(result) => {
                         tracing::warn!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "SGIP认证失败: status={}", result.status);
                         let resp = BindResp {
                             result: result.status as u8,
                             reserve: [0u8; 8],
                         };
                         let mut body = BytesMut::new();
                         resp.encode(&mut body).unwrap();
                         
                         let mut pdu = encode_sgip_pdu_header(CommandId::BindResp, body.len());
                         pdu.extend_from_slice(&body);
                         conn.write_frame(&pdu).await?;
                         return Ok(HandleResult::Stop);
                     }
                     Err(e) => {
                         tracing::error!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "SGIP认证错误: {}", e);
                         let resp = BindResp {
                             result: 1,
                             reserve: [0u8; 8],
                         };
                         let mut body = BytesMut::new();
                         resp.encode(&mut body).unwrap();
                         
                         let mut pdu = encode_sgip_pdu_header(CommandId::BindResp, body.len());
                         pdu.extend_from_slice(&body);
                         conn.write_frame(&pdu).await?;
                         return Ok(HandleResult::Stop);
                     }
                 }
             }
             SgipMessage::Submit(ref _s) => {
                 if conn.should_log(tracing::Level::DEBUG) {
                     tracing::debug!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "收到SGIP短信提交");
                 }
                 return Ok(HandleResult::Continue);
            }
             SgipMessage::Report(r) => {
                 tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "收到SGIP状态报告: submit_sequence={:?}, state={}", r.submit_sequence, r.state);
                 
                 let resp = ReportResp { result: 0 };
                 let mut body = BytesMut::new();
                 resp.encode(&mut body).unwrap();
                 
                 let mut pdu = encode_sgip_pdu_header(CommandId::ReportResp, body.len());
                 pdu.extend_from_slice(&body);
                 conn.write_frame(&pdu).await?;
                 tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "发送SGIP报告响应: result=0");
                 return Ok(HandleResult::Stop);
            }
             SgipMessage::Deliver(d) => {
                 tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "收到SGIP短信下发: user_number={}, sp_number={}", d.user_number, d.sp_number);
                 
                 let resp = DeliverResp { result: 0 };
                 let mut body = BytesMut::new();
                 resp.encode(&mut body).unwrap();
                 
                 let mut pdu = encode_sgip_pdu_header(CommandId::DeliverResp, body.len());
                 pdu.extend_from_slice(&body);
                 conn.write_frame(&pdu).await?;
                 tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "发送SGIP下发响应: result=0");
                 return Ok(HandleResult::Continue);
            }
            SgipMessage::Unbind(_) => {
                tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "收到SGIP Unbind请求");
                
                let resp = UnbindResp;
                let mut body = BytesMut::new();
                resp.encode(&mut body).unwrap();
                
                let mut pdu = encode_sgip_pdu_header(CommandId::UnbindResp, body.len());
                pdu.extend_from_slice(&body);
                conn.write_frame(&pdu).await?;
                tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "发送SGIP Unbind响应");
                return Ok(HandleResult::Stop);
            }
            _ => return Ok(HandleResult::Stop),
        }
    }
}