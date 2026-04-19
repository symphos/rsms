use async_trait::async_trait;
use bytes::BytesMut;
use rsms_codec_smpp::{SmppMessage, decode_message_with_version, CommandId, CommandStatus, Encodable, BindTransmitterResp, BindReceiverResp, BindTransceiverResp, SubmitSmResp, EnquireLinkResp, UnbindResp, SmppVersion};
use rsms_core::{Frame, Result};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::protocol::{
    AuthCredentials, AuthHandler, AuthResult, HandleResult, Protocol, ProtocolConnection,
};

static NEXT_MSG_ID: AtomicU64 = AtomicU64::new(1);

pub struct SmppProtocol;

impl SmppProtocol {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SmppProtocol {
    fn default() -> Self {
        Self::new()
    }
}

impl Protocol for SmppProtocol {
    type Submit = SmppMessage;
    type SubmitResp = SubmitSmResp;
    type MsgId = String;
    type Deliver = SmppMessage;

    fn name(&self) -> &'static str {
        "smpp"
    }

    fn next_msg_id(&self) -> u64 {
        NEXT_MSG_ID.fetch_add(1, Ordering::Relaxed)
    }

    fn encode_submit_resp(
        &self,
        sequence_id: u32,
        msg_id: &Self::MsgId,
        result: u32,
    ) -> Vec<u8> {
        let resp = SubmitSmResp {
            message_id: msg_id.clone(),
        };
        let mut body = BytesMut::new();
        resp.encode(&mut body).unwrap();
        let mut pdu = encode_smpp_pdu_header(CommandId::SUBMIT_SM_RESP, sequence_id, result, body.len());
        pdu.extend_from_slice(&body);
        pdu
    }
}

pub struct SmppHandler {
    auth_handler: Option<Arc<dyn AuthHandler>>,
}

impl SmppHandler {
    pub fn new(auth_handler: Option<Arc<dyn AuthHandler>>) -> Self {
        Self { auth_handler }
    }
}

fn encode_smpp_pdu_header(command_id: CommandId, sequence_number: u32, status: u32, body_len: usize) -> Vec<u8> {
    let total_len = 16 + body_len;
    let mut pdu = Vec::with_capacity(total_len);
    pdu.extend_from_slice(&(total_len as u32).to_be_bytes());
    pdu.extend_from_slice(&(command_id as u32).to_be_bytes());
    pdu.extend_from_slice(&status.to_be_bytes());
    pdu.extend_from_slice(&sequence_number.to_be_bytes());
    pdu
}

#[allow(dead_code)]
fn encode_smpp_error_resp(command_id: CommandId, sequence_number: u32, status: CommandStatus) -> Vec<u8> {
    encode_smpp_pdu_header(command_id, sequence_number, status as u32, 0)
}

#[async_trait]
impl crate::protocol::ProtocolHandler for SmppHandler {
    fn name(&self) -> &'static str {
        "smpp-handler"
    }

    async fn handle_frame(&self, frame: &Frame, conn: Arc<dyn ProtocolConnection>) -> Result<HandleResult> {
        let frame_bytes = frame.data_as_slice();
        let sequence_number = frame.sequence_id;
        
        let version = conn.protocol_version().await;
        let smpp_version = version.and_then(|v| SmppVersion::from_interface_version(v));
        
        let msg = match decode_message_with_version(frame_bytes, smpp_version) {
            Ok(m) => m,
            Err(e) => {
                tracing::error!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "SMPP 消息解码失败: {e}");
                tracing::debug!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "原始数据 (hex): {:02x?}", frame_bytes);
                
                return Ok(HandleResult::Stop);
            }
        };

        match msg {
            SmppMessage::BindTransmitter(b) => {
                tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "收到SMPP绑定(发送端): system_id={}", b.system_id);
                
                let credentials = AuthCredentials::Smpp {
                    system_id: b.system_id.clone(),
                    password: b.password.clone(),
                    interface_version: b.interface_version,
                };
                
                let auth_result = if let Some(ref handler) = self.auth_handler {
                    let conn_info = conn.connection_info().await;
                    handler.authenticate(&b.system_id, credentials, &conn_info).await
                } else {
                    Ok(AuthResult::success(b.system_id.clone()))
                };
                
                match auth_result {
                    Ok(result) if result.status == 0 => {
                        conn.set_authenticated_account(result.account.clone()).await;
                        conn.set_protocol_version(b.interface_version).await;
                        tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "SMPP认证成功: account={}, version={:02x}", result.account, b.interface_version);
                        
                        let resp = BindTransmitterResp {
                            system_id: "SERVER".to_string(),
                            sc_interface_version: b.interface_version,
                        };
                        let mut body = BytesMut::new();
                        resp.encode(&mut body).unwrap();
                        
                        let mut pdu = encode_smpp_pdu_header(CommandId::BIND_TRANSMITTER_RESP, sequence_number, 0, body.len());
                        pdu.extend_from_slice(&body);
                        conn.write_frame(&pdu).await?;
                        tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "发送SMPP绑定响应: status=0");
                        return Ok(HandleResult::Continue);
                    }
                    Ok(result) => {
                        tracing::warn!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "SMPP认证失败: status={}", result.status);
                        let resp = BindTransmitterResp {
                            system_id: "SERVER".to_string(),
                            sc_interface_version: b.interface_version,
                        };
                        let mut body = BytesMut::new();
                        resp.encode(&mut body).unwrap();
                        
                        let mut pdu = encode_smpp_pdu_header(CommandId::BIND_TRANSMITTER_RESP, sequence_number, result.status, body.len());
                        pdu.extend_from_slice(&body);
                        conn.write_frame(&pdu).await?;
                        return Ok(HandleResult::Stop);
                    }
                    Err(e) => {
                        tracing::error!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "SMPP认证错误: {}", e);
                        let resp = BindTransmitterResp {
                            system_id: "SERVER".to_string(),
                            sc_interface_version: b.interface_version,
                        };
                        let mut body = BytesMut::new();
                        resp.encode(&mut body).unwrap();
                        
                        let mut pdu = encode_smpp_pdu_header(CommandId::BIND_TRANSMITTER_RESP, sequence_number, 1, body.len());
                        pdu.extend_from_slice(&body);
                        conn.write_frame(&pdu).await?;
                        return Ok(HandleResult::Stop);
                    }
                }
            }
            SmppMessage::BindReceiver(b) => {
                tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "收到SMPP绑定(接收端): system_id={}", b.system_id);
                
                let credentials = AuthCredentials::Smpp {
                    system_id: b.system_id.clone(),
                    password: b.password.clone(),
                    interface_version: b.interface_version,
                };
                
                let auth_result = if let Some(ref handler) = self.auth_handler {
                    let conn_info = conn.connection_info().await;
                    handler.authenticate(&b.system_id, credentials, &conn_info).await
                } else {
                    Ok(AuthResult::success(b.system_id.clone()))
                };
                
                match auth_result {
                    Ok(result) if result.status == 0 => {
                        conn.set_authenticated_account(result.account.clone()).await;
                        conn.set_protocol_version(b.interface_version).await;
                        tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "SMPP认证成功: account={}, version={:02x}", result.account, b.interface_version);
                        
                        let resp = BindReceiverResp {
                            system_id: "SERVER".to_string(),
                            sc_interface_version: b.interface_version,
                        };
                        let mut body = BytesMut::new();
                        resp.encode(&mut body).unwrap();
                        
                        let mut pdu = encode_smpp_pdu_header(CommandId::BIND_RECEIVER_RESP, sequence_number, 0, body.len());
                        pdu.extend_from_slice(&body);
                        conn.write_frame(&pdu).await?;
                        tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "发送SMPP绑定响应: status=0");
                        return Ok(HandleResult::Continue);
                    }
                    Ok(result) => {
                        tracing::warn!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "SMPP认证失败: status={}", result.status);
                        let resp = BindReceiverResp {
                            system_id: "SERVER".to_string(),
                            sc_interface_version: b.interface_version,
                        };
                        let mut body = BytesMut::new();
                        resp.encode(&mut body).unwrap();
                        
                        let mut pdu = encode_smpp_pdu_header(CommandId::BIND_RECEIVER_RESP, sequence_number, result.status, body.len());
                        pdu.extend_from_slice(&body);
                        conn.write_frame(&pdu).await?;
                        return Ok(HandleResult::Stop);
                    }
                    Err(e) => {
                        tracing::error!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "SMPP认证错误: {}", e);
                        let resp = BindReceiverResp {
                            system_id: "SERVER".to_string(),
                            sc_interface_version: b.interface_version,
                        };
                        let mut body = BytesMut::new();
                        resp.encode(&mut body).unwrap();
                        
                        let mut pdu = encode_smpp_pdu_header(CommandId::BIND_RECEIVER_RESP, sequence_number, 1, body.len());
                        pdu.extend_from_slice(&body);
                        conn.write_frame(&pdu).await?;
                        return Ok(HandleResult::Stop);
                    }
                }
            }
            SmppMessage::BindTransceiver(b) => {
                tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "收到SMPP绑定(收发一体): system_id={}", b.system_id);
                
                let credentials = AuthCredentials::Smpp {
                    system_id: b.system_id.clone(),
                    password: b.password.clone(),
                    interface_version: b.interface_version,
                };
                
                let auth_result = if let Some(ref handler) = self.auth_handler {
                    let conn_info = conn.connection_info().await;
                    handler.authenticate(&b.system_id, credentials, &conn_info).await
                } else {
                    Ok(AuthResult::success(b.system_id.clone()))
                };
                
                match auth_result {
                    Ok(result) if result.status == 0 => {
                        conn.set_authenticated_account(result.account.clone()).await;
                        conn.set_protocol_version(b.interface_version).await;
                        tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "SMPP认证成功: account={}, version={:02x}", result.account, b.interface_version);
                        
                        let resp = BindTransceiverResp {
                            system_id: "SERVER".to_string(),
                            sc_interface_version: b.interface_version,
                        };
                        let mut body = BytesMut::new();
                        resp.encode(&mut body).unwrap();
                        
                        let mut pdu = encode_smpp_pdu_header(CommandId::BIND_TRANSCEIVER_RESP, sequence_number, 0, body.len());
                        pdu.extend_from_slice(&body);
                        conn.write_frame(&pdu).await?;
                        tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "发送SMPP绑定响应: status=0");
                        return Ok(HandleResult::Continue);
                    }
                    Ok(result) => {
                        tracing::warn!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "SMPP认证失败: status={}", result.status);
                        let resp = BindTransceiverResp {
                            system_id: "SERVER".to_string(),
                            sc_interface_version: b.interface_version,
                        };
                        let mut body = BytesMut::new();
                        resp.encode(&mut body).unwrap();
                        
                        let mut pdu = encode_smpp_pdu_header(CommandId::BIND_TRANSCEIVER_RESP, sequence_number, result.status, body.len());
                        pdu.extend_from_slice(&body);
                        conn.write_frame(&pdu).await?;
                        return Ok(HandleResult::Stop);
                    }
                    Err(e) => {
                        tracing::error!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "SMPP认证错误: {}", e);
                        let resp = BindTransceiverResp {
                            system_id: "SERVER".to_string(),
                            sc_interface_version: b.interface_version,
                        };
                        let mut body = BytesMut::new();
                        resp.encode(&mut body).unwrap();
                        
                        let mut pdu = encode_smpp_pdu_header(CommandId::BIND_TRANSCEIVER_RESP, sequence_number, 1, body.len());
                        pdu.extend_from_slice(&body);
                        conn.write_frame(&pdu).await?;
                        return Ok(HandleResult::Stop);
                    }
                }
            }
            SmppMessage::SubmitSm(ref _s) => {
                if conn.should_log(tracing::Level::DEBUG) {
                    tracing::debug!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "收到SMPP短信提交");
                }
                return Ok(HandleResult::Continue);
            }
            SmppMessage::EnquireLink { .. } => {
                tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "收到SMPP链路检测");
                
                let resp = EnquireLinkResp;
                let mut body = BytesMut::new();
                resp.encode(&mut body).unwrap();
                
                let mut pdu = encode_smpp_pdu_header(CommandId::ENQUIRE_LINK_RESP, sequence_number, 0, body.len());
                pdu.extend_from_slice(&body);
                conn.write_frame(&pdu).await?;
                tracing::debug!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "发送SMPP链路检测响应");
                return Ok(HandleResult::Continue);
            }
            SmppMessage::DeliverSm(d) => {
                if conn.should_log(tracing::Level::DEBUG) {
                    tracing::debug!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "收到SMPP Deliver: from={}, to={}", d.source_addr, d.destination_addr);
                }
                return Ok(HandleResult::Continue);
            }
            SmppMessage::Unbind(_) => {
                tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "收到SMPP解绑请求");
                
                let resp = UnbindResp;
                let mut body = BytesMut::new();
                resp.encode(&mut body).unwrap();
                
                let mut pdu = encode_smpp_pdu_header(CommandId::UNBIND_RESP, sequence_number, 0, body.len());
                pdu.extend_from_slice(&body);
                conn.write_frame(&pdu).await?;
                tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "发送SMPP解绑响应");
                return Ok(HandleResult::Stop);
            }
            _ => return Ok(HandleResult::Stop),
        }
    }
}