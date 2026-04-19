use rsms_core::Result;
use rsms_codec_cmpp::{Pdu, Connect, Submit, ConnectResp, SubmitResp};
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
        
        let connect = Connect {
            source_addr: source_addr.to_string(),
            version: 0x30,
            authenticator_source: compute_authenticator(source_addr, password, seq_id),
            timestamp: 0,
        };
        
        let pdu: Pdu = connect.into();
        let encoded = pdu.to_pdu_bytes(seq_id);
        
        self.send_pdu(encoded.as_slice()).await?;
        self.read_connect_resp().await
    }

    pub async fn send_submit(&mut self, src_id: &str, dest_id: &str, content: &str) -> Result<SubmitResp> {
        let seq_id = self.next_seq_id() as u32;
        
        let submit = Submit {
            msg_id: [0u8; 8],
            pk_total: 1,
            pk_number: 1,
            registered_delivery: 1,
            msg_level: 1,
            service_id: "SMS".to_string(),
            fee_user_type: 0,
            fee_terminal_id: "".to_string(),
            fee_terminal_type: 0,
            tppid: 0,
            tpudhi: 0,
            msg_fmt: 15,
            msg_src: "".to_string(),
            fee_type: "01".to_string(),
            fee_code: "005".to_string(),
            valid_time: "".to_string(),
            at_time: "".to_string(),
            src_id: src_id.to_string(),
            dest_usr_tl: 1,
            dest_terminal_ids: vec![dest_id.to_string()],
            dest_terminal_type: 0,
            msg_content: content.as_bytes().to_vec(),
            link_id: "".to_string(),
        };
        
        let pdu: Pdu = submit.into();
        let encoded = pdu.to_pdu_bytes(seq_id);
        
        self.send_pdu(encoded.as_slice()).await?;
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