use async_trait::async_trait;
use rsms_codec_smgp::{
    compute_login_auth, datatypes::{tlv_tags, Tlv}, decode_message, datatypes::SmgpReport,
    CommandId, DeliverResp, Login, Pdu, SmgpMessage, Submit,
};
use rsms_connector::client::{ClientContext, ClientHandler};
use rsms_connector::{connect, MessageItem, MessageSource, SmgpDecoder};
use rsms_core::{EncodedPdu, EndpointConfig, Frame, Result};
use rsms_longmsg::split::SmsAlphabet;
use rsms_longmsg::{LongMessageFrame, LongMessageMerger, LongMessageSplitter, UdhParser};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

const ACCOUNT: &str = "900001";
const PASSWORD: &str = "password123";
const SERVER_ADDR: &str = "127.0.0.1:8890";

fn detect_alphabet(content: &[u8]) -> SmsAlphabet {
    if content.iter().all(|b| *b <= 0x7f) {
        SmsAlphabet::ASCII
    } else {
        SmsAlphabet::UCS2
    }
}

fn load_messages(path: &str) -> Vec<(String, String)> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("无法读取 {}: {}, 使用默认消息", path, e);
            return vec![(
                "13800138000".to_string(),
                "Hello from SMGP Client".to_string(),
            )];
        }
    };
    let messages: Vec<(String, String)> = content
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.trim().starts_with('#'))
        .filter_map(|line| {
            let trimmed = line.trim();
            trimmed
                .split_once(' ')
                .map(|(phone, content)| (phone.to_string(), content.to_string()))
        })
        .collect();
    if messages.is_empty() {
        tracing::warn!("{} 中没有有效消息，使用默认消息", path);
        return vec![(
            "13800138000".to_string(),
            "Hello from SMGP Client".to_string(),
        )];
    }
    messages
}

struct ClientMessageSource {
    queue: Arc<Mutex<VecDeque<MessageItem>>>,
    authenticated: Arc<AtomicBool>,
}

impl ClientMessageSource {
    fn from_messages(messages: &[(String, String)], authenticated: Arc<AtomicBool>) -> Self {
        let mut queue: VecDeque<MessageItem> = VecDeque::new();
        let mut seq = 1000u32;
        let mut splitter = LongMessageSplitter::new();

        for (phone, content) in messages {
            let alphabet = detect_alphabet(content.as_bytes());
            let single_max = match alphabet {
                SmsAlphabet::ASCII | SmsAlphabet::GSM7 => 160,
                SmsAlphabet::UCS2 => 70,
                SmsAlphabet::Binary => 140,
            };

            if content.len() > single_max {
                let frames = splitter.split(content.as_bytes(), alphabet);
                let mut items = Vec::new();
                for frame in frames {
                    let mut submit =
                        Submit::new().with_message(ACCOUNT, phone, &frame.content);
                    submit.need_report = 1;
                    if frame.has_udhi {
                        submit.optional_params.add(Tlv::Byte { tag: tlv_tags::TP_UDHI, value: 1 });
                        submit.optional_params.add(Tlv::Byte { tag: tlv_tags::PK_TOTAL, value: frame.total_segments });
                        submit.optional_params.add(Tlv::Byte { tag: tlv_tags::PK_NUMBER, value: frame.segment_number });
                    }
                    let pdu: Pdu = submit.into();
                    items.push(Arc::new(pdu.to_pdu_bytes(seq)) as Arc<dyn EncodedPdu>);
                    seq += 1;
                }
                queue.push_back(MessageItem::Group { items });
            } else {
                let mut submit =
                    Submit::new().with_message(ACCOUNT, phone, content.as_bytes());
                submit.need_report = 1;
                let pdu: Pdu = submit.into();
                queue.push_back(
                    MessageItem::Single(Arc::new(pdu.to_pdu_bytes(seq)) as Arc<dyn EncodedPdu>),
                );
                seq += 1;
            }
        }

        tracing::info!("MessageSource: 已加载 {} 条待发送消息", queue.len());

        Self {
            queue: Arc::new(Mutex::new(queue)),
            authenticated,
        }
    }
}

#[async_trait]
impl MessageSource for ClientMessageSource {
    async fn fetch(&self, account: &str, batch_size: usize) -> Result<Vec<MessageItem>> {
        if !self.authenticated.load(Ordering::Relaxed) {
            return Ok(Vec::new());
        }

        let mut result = Vec::new();
        let mut queue = self.queue.lock().unwrap();
        while result.len() < batch_size {
            match queue.pop_front() {
                Some(item) => result.push(item),
                None => break,
            }
        }

        if !result.is_empty() {
            tracing::info!(
                "MessageSource: 发送 {} 条消息 (account={})",
                result.len(),
                account
            );
        }

        Ok(result)
    }
}

struct SmgpClientHandler {
    authenticated: Arc<AtomicBool>,
    submit_count: AtomicU32,
    report_count: AtomicU32,
    mo_merger: std::sync::Mutex<LongMessageMerger>,
}

impl SmgpClientHandler {
    fn new(authenticated: Arc<AtomicBool>) -> Self {
        Self {
            authenticated,
            submit_count: AtomicU32::new(0),
            report_count: AtomicU32::new(0),
            mo_merger: std::sync::Mutex::new(LongMessageMerger::new()),
        }
    }
}

#[async_trait]
impl ClientHandler for SmgpClientHandler {
    fn name(&self) -> &'static str {
        "smgp-client"
    }

    async fn on_inbound(&self, ctx: &ClientContext<'_>, frame: &Frame) -> Result<()> {
        let data = frame.data_as_slice();
        let seq_id = frame.sequence_id;

        match frame.command_id {
            cmd if cmd == CommandId::LoginResp as u32 => match decode_message(data) {
                Ok(SmgpMessage::LoginResp { resp, .. }) => {
                    if resp.status == 0 {
                        tracing::info!("SMGP 认证成功 (version=0x{:02x})", resp.version);
                        self.authenticated.store(true, Ordering::Relaxed);
                    } else {
                        tracing::error!("SMGP 认证失败: status={}", resp.status);
                    }
                }
                _ => tracing::warn!("无法解析 LoginResp"),
            },

            cmd if cmd == CommandId::SubmitResp as u32 => match decode_message(data) {
                Ok(SmgpMessage::SubmitResp { resp, .. }) => {
                    let count = self.submit_count.fetch_add(1, Ordering::Relaxed) + 1;
                    tracing::info!(
                        "[{}] SubmitResp: msg_id={}, status={}",
                        count,
                        resp.msg_id,
                        resp.status
                    );
                }
                _ => tracing::warn!("无法解析 SubmitResp"),
            },

            cmd if cmd == CommandId::Deliver as u32 => match decode_message(data) {
                Ok(SmgpMessage::Deliver { deliver, .. }) => {
                    if deliver.is_report == 1 {
                        if let Some(report) = SmgpReport::parse(&deliver.msg_content) {
                            let count = self.report_count.fetch_add(1, Ordering::Relaxed) + 1;
                            tracing::info!(
                                "[{}] 状态报告: msg_id={}, stat={}, src={}",
                                count,
                                report.msg_id,
                                report.stat,
                                deliver.src_term_id
                            );
                        }
                    } else if let Some((udh, _)) = UdhParser::extract_udh(&deliver.msg_content) {
                        let ref_id = udh.reference_id;
                        let total = udh.total_segments;
                        let seg = udh.segment_number;
                        let frame = LongMessageFrame::new(
                            udh.reference_id,
                            udh.total_segments,
                            udh.segment_number,
                            deliver.msg_content.clone(),
                            true,
                            Some(udh),
                        );
                        let mut merger = self.mo_merger.lock().unwrap();
                        match merger.add_frame(frame) {
                            Ok(Some(complete)) => {
                                let content = String::from_utf8_lossy(&complete);
                                tracing::info!(
                                    "长短信合包完成: src={}, content={}",
                                    deliver.src_term_id,
                                    content
                                );
                            }
                            Ok(None) => {
                                tracing::info!(
                                    "长短信等待更多分段: ref={}, seg={}/{}",
                                    ref_id,
                                    seg,
                                    total
                                );
                            }
                            Err(e) => {
                                tracing::warn!("长短信合包错误: {}", e);
                            }
                        }
                    } else {
                        let content = String::from_utf8_lossy(&deliver.msg_content);
                        tracing::info!(
                            "上行短信: src={}, content={}",
                            deliver.src_term_id,
                            content
                        );
                    }

                    let resp = DeliverResp { status: 0 };
                    let resp_pdu: Pdu = resp.into();
                    ctx.conn
                        .write_frame(resp_pdu.to_pdu_bytes(seq_id).as_bytes_ref())
                        .await?;
                }
                _ => tracing::warn!("无法解析 Deliver"),
            },

            cmd if cmd == CommandId::ActiveTestResp as u32 => {
                tracing::debug!("收到心跳响应");
            }

            _ => {
                tracing::debug!("收到未知命令: command_id=0x{:08x}", frame.command_id);
            }
        }

        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let messages_path = std::path::Path::new(manifest_dir).join("messages.conf");
    let messages = load_messages(&messages_path.to_string_lossy());
    tracing::info!("加载了 {} 条待发送消息", messages.len());

    let authenticated = Arc::new(AtomicBool::new(false));
    let msg_source = Arc::new(ClientMessageSource::from_messages(
        &messages,
        authenticated.clone(),
    ));
    let handler = Arc::new(SmgpClientHandler::new(authenticated.clone()));

    let (host, port) = if let Some((h, p)) = SERVER_ADDR.rsplit_once(':') {
        (h.to_string(), p.parse().unwrap_or(8890))
    } else {
        ("127.0.0.1".to_string(), 8890)
    };

    let endpoint = Arc::new(
        EndpointConfig::new(ACCOUNT, host, port, 100, 60)
            .with_window_size(2048)
            .with_log_level(tracing::Level::INFO),
    );

    tracing::info!("正在连接 SMGP 服务端 {}...", SERVER_ADDR);

    let conn = connect(
        endpoint,
        handler,
        SmgpDecoder,
        None,
        Some(msg_source as Arc<dyn MessageSource>),
        None,
    )
    .await?;

    tracing::info!("TCP 连接已建立 (conn_id={})", conn.id);

    let timestamp = smgp_timestamp();
    let authenticator = compute_login_auth(ACCOUNT, PASSWORD, timestamp);
    let login_pdu = Login {
        client_id: ACCOUNT.to_string(),
        authenticator,
        login_mode: 0,
        timestamp,
        version: 0x30,
    };
    let pdu: Pdu = login_pdu.into();
    conn.send_request(pdu.to_pdu_bytes(1)).await?;
    tracing::info!("已发送 Login 认证请求");

    tracing::info!("认证成功后 MessageSource 将自动发送短信...");
    tracing::info!("按 Ctrl+C 退出");

    tokio::signal::ctrl_c().await?;
    tracing::info!("收到退出信号，正在关闭...");

    Ok(())
}

fn smgp_timestamp() -> u32 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let month = ((secs / 86400 / 30) % 12 + 1) as u32;
    let day = ((secs / 86400) % 30 + 1) as u32;
    let h = ((secs / 3600) % 24 + 8) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    month * 100000000 + day * 1000000 + h as u32 * 10000 + m as u32 * 100 + s as u32
}
