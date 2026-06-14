use rsms_core::Result;
// 窄腰统一模型：Connect/Submit 构造改走 CmppAdapter.encode(UnifiedMessage::{Bind,Submit})。
// ConnectResp/SubmitResp 仅作为本桩的返回结构保留（read_*_resp 是纯字节级 wire 读取，见下方注释）。
use rsms_codec_cmpp::adapter::CmppAdapter;
use rsms_codec_cmpp::{ConnectResp, SubmitResp};
use rsms_model::{
    Address, CmppExtra, Encoding, ProtocolAdapter, ProtocolExtra, Sequence, UnifiedBind,
    UnifiedMessage, UnifiedSubmit,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub struct TestClient {
    stream: TcpStream,
    seq_id: Arc<AtomicUsize>,
    connected: bool,
}

impl TestClient {
    pub async fn connect(addr: &str) -> Result<Self> {
        let stream = TcpStream::connect(addr).await?;
        stream.readable().await?;
        
        Ok(Self {
            stream,
            seq_id: Arc::new(AtomicUsize::new(0)),
            connected: true,
        })
    }

    pub async fn send_connect(&mut self, source_addr: &str, password: &str) -> Result<ConnectResp> {
        let seq_id = self.next_seq_id() as u32;
        
        // 统一 Bind：authenticator 由测试桩 compute_authenticator 算出（非真 MD5，保留桩逻辑），
        // 放入 UnifiedBind.authenticator；adapter 原样回填，不重算。CMPP 无 system_type/login_mode。
        let bind = UnifiedMessage::Bind(UnifiedBind {
            client_id: source_addr.to_string(),
            authenticator: compute_authenticator(source_addr, password, seq_id).to_vec(),
            timestamp: 0,
            version: 0x30,
            system_type: None,
            mode: rsms_model::BindMode::default(),
            login_mode: None,
        });
        let encoded = CmppAdapter.encode(&bind, Sequence::Plain(seq_id))?;

        self.send_pdu(&encoded).await?;
        self.read_connect_resp().await
    }

    pub async fn send_submit(&mut self, src_id: &str, dest_id: &str, content: &str) -> Result<SubmitResp> {
        let seq_id = self.next_seq_id() as u32;
        
        // 统一 Submit：want_report=true 对应 registered_delivery=1；encoding=Gbk 对应 msg_fmt=15；
        // CMPP 方言（pk_total/pk_number/msg_level/service_id/fee_*）落 ProtocolExtra::Cmpp。
        let submit = UnifiedMessage::Submit(UnifiedSubmit {
            src: Address::plain(src_id),
            dests: vec![Address::plain(dest_id)],
            content: content.as_bytes().to_vec(),
            encoding: Encoding::Gbk,
            want_report: true,
            concat: None,
            extra: ProtocolExtra::Cmpp(CmppExtra {
                pk_total: 1,
                pk_number: 1,
                msg_level: 1,
                service_id: "SMS".to_string(),
                fee_type: "01".to_string(),
                fee_code: "005".to_string(),
                ..Default::default()
            }),
            tlvs: vec![],
        });
        let encoded = CmppAdapter.encode(&submit, Sequence::Plain(seq_id))?;

        self.send_pdu(&encoded).await?;
        self.read_submit_resp().await
    }

    async fn send_pdu(&mut self, pdu: &[u8]) -> Result<()> {
        self.stream.write_all(pdu).await?;
        self.stream.flush().await?;
        Ok(())
    }

    async fn read_connect_resp(&mut self) -> Result<ConnectResp> {
        let mut header = [0u8; 12];
        self.stream.read_exact(&mut header).await?;
        
        let status = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
        
        Ok(ConnectResp {
            status,
            authenticator_ismg: [0u8; 16],
            version: 0x30,
        })
    }

    async fn read_submit_resp(&mut self) -> Result<SubmitResp> {
        let mut header = [0u8; 12];
        self.stream.read_exact(&mut header).await?;
        
        let result = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
        
        Ok(SubmitResp {
            result,
            msg_id: [0u8; 8],
        })
    }

    fn next_seq_id(&self) -> usize {
        self.seq_id.fetch_add(1, Ordering::Relaxed)
    }

    pub fn is_connected(&self) -> bool {
        self.connected
    }

    pub async fn close(&mut self) {
        self.connected = false;
        let _ = self.stream.shutdown().await;
    }
}

fn compute_authenticator(source_addr: &str, password: &str, seq_id: u32) -> [u8; 16] {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    
    let mut hasher = DefaultHasher::new();
    format!("{}+{}+{}", source_addr, password, seq_id).hash(&mut hasher);
    let hash = hasher.finish();
    
    let mut result = [0u8; 16];
    result[..8].copy_from_slice(&hash.to_be_bytes());
    result[8..].copy_from_slice(&(!hash).to_be_bytes());
    result
}