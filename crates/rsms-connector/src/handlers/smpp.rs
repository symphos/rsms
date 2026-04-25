use async_trait::async_trait;
use bytes::BytesMut;
use rsms_codec_smpp::{SmppMessage, decode_message_with_version, CommandId, Encodable, BindTransmitterResp, EnquireLinkResp, UnbindResp, SmppVersion};
use rsms_core::{Frame, Result};
use std::sync::Arc;

use crate::protocol::{
    AuthCredentials, AuthHandler, AuthResult, HandleResult, ProtocolConnection,
};

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

struct BindInfo {
    system_id: String,
    password: String,
    interface_version: u8,
    resp_command_id: CommandId,
    bind_type_name: &'static str,
}

fn extract_bind_info(msg: &SmppMessage) -> Option<BindInfo> {
    match msg {
        SmppMessage::BindTransmitter(b) => Some(BindInfo {
            system_id: b.system_id.clone(),
            password: b.password.clone(),
            interface_version: b.interface_version,
            resp_command_id: CommandId::BIND_TRANSMITTER_RESP,
            bind_type_name: "发送端",
        }),
        SmppMessage::BindReceiver(b) => Some(BindInfo {
            system_id: b.system_id.clone(),
            password: b.password.clone(),
            interface_version: b.interface_version,
            resp_command_id: CommandId::BIND_RECEIVER_RESP,
            bind_type_name: "接收端",
        }),
        SmppMessage::BindTransceiver(b) => Some(BindInfo {
            system_id: b.system_id.clone(),
            password: b.password.clone(),
            interface_version: b.interface_version,
            resp_command_id: CommandId::BIND_TRANSCEIVER_RESP,
            bind_type_name: "收发一体",
        }),
        _ => None,
    }
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

        if let Some(info) = extract_bind_info(&msg) {
            tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "收到SMPP绑定({}): system_id={}", info.bind_type_name, info.system_id);
            
            let credentials = AuthCredentials::Smpp {
                system_id: info.system_id.clone(),
                password: info.password.clone(),
                interface_version: info.interface_version,
            };
            
            let auth_result = if let Some(ref handler) = self.auth_handler {
                let conn_info = conn.connection_info().await;
                handler.authenticate(&info.system_id, credentials, &conn_info).await
            } else {
                Ok(AuthResult::success(info.system_id.clone()))
            };
            
            let (status, should_continue) = match &auth_result {
                Ok(result) if result.status == 0 => {
                    conn.set_authenticated_account(result.account.clone()).await;
                    conn.set_protocol_version(info.interface_version).await;
                    tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "SMPP认证成功: account={}, version={:02x}", result.account, info.interface_version);
                    (0, true)
                }
                Ok(result) => {
                    tracing::warn!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "SMPP认证失败: status={}", result.status);
                    (result.status, false)
                }
                Err(e) => {
                    tracing::error!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "SMPP认证错误: {}", e);
                    (1, false)
                }
            };

            let resp = BindTransmitterResp {
                system_id: "SERVER".to_string(),
                sc_interface_version: info.interface_version,
            };
            let mut body = BytesMut::new();
            resp.encode(&mut body).unwrap();
            
            let mut pdu = encode_smpp_pdu_header(info.resp_command_id, sequence_number, status, body.len());
            pdu.extend_from_slice(&body);
            conn.write_frame(&pdu).await?;
            
            if should_continue {
                tracing::info!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "发送SMPP绑定响应: status=0");
            }
            
            return Ok(if should_continue { HandleResult::Continue } else { HandleResult::Stop });
        }

        match msg {
            SmppMessage::SubmitSm(_) => {
                if conn.should_log(tracing::Level::DEBUG) {
                    tracing::debug!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "收到SMPP短信提交");
                }
                Ok(HandleResult::Continue)
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
                Ok(HandleResult::Continue)
            }
            SmppMessage::DeliverSm(d) => {
                if conn.should_log(tracing::Level::DEBUG) {
                    tracing::debug!(conn_id = conn.id(), remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "收到SMPP Deliver: from={}, to={}", d.source_addr, d.destination_addr);
                }
                Ok(HandleResult::Continue)
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
                Ok(HandleResult::Stop)
            }
            _ => Ok(HandleResult::Stop),
        }
    }
}
