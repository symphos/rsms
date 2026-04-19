//! SMGP 消息解析与编码（统一使用 datatypes 中的定义）。

use crate::codec::{Decodable, PduHeader};
use crate::datatypes::{
    ActiveTest, ActiveTestResp, CommandId, Deliver, DeliverResp, Exit, ExitResp, Login, LoginResp,
    Submit, SubmitResp,
};
use rsms_core::RsmsError;
use std::io::Cursor;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmgpMessage {
    Login {
        sequence_id: u32,
        login: Login,
    },
    LoginResp {
        sequence_id: u32,
        resp: LoginResp,
    },
    Submit {
        sequence_id: u32,
        submit: Submit,
    },
    SubmitResp {
        sequence_id: u32,
        resp: SubmitResp,
    },
    Deliver {
        sequence_id: u32,
        deliver: Deliver,
    },
    DeliverResp {
        sequence_id: u32,
        resp: DeliverResp,
    },
    ActiveTest {
        sequence_id: u32,
    },
    ActiveTestResp {
        sequence_id: u32,
    },
    Exit {
        sequence_id: u32,
    },
    ExitResp {
        sequence_id: u32,
    },
    Unknown {
        command_id: u32,
        sequence_id: u32,
        body: Vec<u8>,
    },
}

impl SmgpMessage {
    pub fn sequence_id(&self) -> u32 {
        match self {
            SmgpMessage::Login { sequence_id, .. } => *sequence_id,
            SmgpMessage::LoginResp { sequence_id, .. } => *sequence_id,
            SmgpMessage::Submit { sequence_id, .. } => *sequence_id,
            SmgpMessage::SubmitResp { sequence_id, .. } => *sequence_id,
            SmgpMessage::Deliver { sequence_id, .. } => *sequence_id,
            SmgpMessage::DeliverResp { sequence_id, .. } => *sequence_id,
            SmgpMessage::ActiveTest { sequence_id } => *sequence_id,
            SmgpMessage::ActiveTestResp { sequence_id } => *sequence_id,
            SmgpMessage::Exit { sequence_id } => *sequence_id,
            SmgpMessage::ExitResp { sequence_id } => *sequence_id,
            SmgpMessage::Unknown { sequence_id, .. } => *sequence_id,
        }
    }
}

pub fn decode_message(pdu: &[u8]) -> Result<SmgpMessage, RsmsError> {
    let mut cursor = Cursor::new(pdu);
    let header = PduHeader::decode(&mut cursor).map_err(|e| RsmsError::Codec(e.to_string()))?;
    let body = &pdu[PduHeader::SIZE..];
    let seq = header.sequence_id;

    match header.command_id {
        CommandId::Login => {
            let login = Login::decode(header, &mut Cursor::new(body))?;
            Ok(SmgpMessage::Login {
                sequence_id: seq,
                login,
            })
        }
        CommandId::LoginResp => {
            let resp = LoginResp::decode(header, &mut Cursor::new(body))?;
            Ok(SmgpMessage::LoginResp {
                sequence_id: seq,
                resp,
            })
        }
        CommandId::Submit => {
            let submit = Submit::decode(header, &mut Cursor::new(body))?;
            Ok(SmgpMessage::Submit {
                sequence_id: seq,
                submit,
            })
        }
        CommandId::SubmitResp => {
            let resp = SubmitResp::decode(header, &mut Cursor::new(body))?;
            Ok(SmgpMessage::SubmitResp {
                sequence_id: seq,
                resp,
            })
        }
        CommandId::Deliver => {
            let deliver = Deliver::decode(header, &mut Cursor::new(body))?;
            Ok(SmgpMessage::Deliver {
                sequence_id: seq,
                deliver,
            })
        }
        CommandId::DeliverResp => {
            let resp = DeliverResp::decode(header, &mut Cursor::new(body))?;
            Ok(SmgpMessage::DeliverResp {
                sequence_id: seq,
                resp,
            })
        }
        CommandId::ActiveTest => {
            ActiveTest::decode(header, &mut Cursor::new(body))?;
            Ok(SmgpMessage::ActiveTest { sequence_id: seq })
        }
        CommandId::ActiveTestResp => {
            ActiveTestResp::decode(header, &mut Cursor::new(body))?;
            Ok(SmgpMessage::ActiveTestResp { sequence_id: seq })
        }
        CommandId::Exit => {
            Exit::decode(header, &mut Cursor::new(body))?;
            Ok(SmgpMessage::Exit { sequence_id: seq })
        }
        CommandId::ExitResp => {
            ExitResp::decode(header, &mut Cursor::new(body))?;
            Ok(SmgpMessage::ExitResp { sequence_id: seq })
        }
        other => Ok(SmgpMessage::Unknown {
            command_id: other as u32,
            sequence_id: seq,
            body: body.to_vec(),
        }),
    }
}

pub fn encode_message(msg: &SmgpMessage) -> Result<Vec<u8>, RsmsError> {
    let seq = msg.sequence_id();
    let pdu = match msg {
        SmgpMessage::Login { login, .. } => crate::codec::Pdu::Login(login.clone()),
        SmgpMessage::LoginResp { resp, .. } => crate::codec::Pdu::LoginResp(resp.clone()),
        SmgpMessage::Submit { submit, .. } => crate::codec::Pdu::Submit(submit.clone()),
        SmgpMessage::SubmitResp { resp, .. } => crate::codec::Pdu::SubmitResp(resp.clone()),
        SmgpMessage::Deliver { deliver, .. } => crate::codec::Pdu::Deliver(deliver.clone()),
        SmgpMessage::DeliverResp { resp, .. } => crate::codec::Pdu::DeliverResp(resp.clone()),
        SmgpMessage::ActiveTest { .. } => crate::codec::Pdu::ActiveTest(ActiveTest),
        SmgpMessage::ActiveTestResp { .. } => {
            crate::codec::Pdu::ActiveTestResp(ActiveTestResp { reserved: 0 })
        }
        SmgpMessage::Exit { .. } => crate::codec::Pdu::Exit(Exit),
        SmgpMessage::ExitResp { .. } => crate::codec::Pdu::ExitResp(ExitResp { reserved: 0 }),
        SmgpMessage::Unknown {
            command_id, body, ..
        } => {
            let body_len = body.len();
            let total = (12 + body_len) as u32;
            let mut v = Vec::with_capacity(total as usize);
            v.extend_from_slice(&total.to_be_bytes());
            v.extend_from_slice(&command_id.to_be_bytes());
            v.extend_from_slice(&seq.to_be_bytes());
            v.extend_from_slice(body);
            return Ok(v);
        }
    };
    Ok(pdu.to_pdu_bytes(seq).to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_active_test_hex() {
        let hex = "0000000c0000000400000001";
        let raw: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        let m = decode_message(&raw).unwrap();
        match m {
            SmgpMessage::ActiveTest { sequence_id } => assert_eq!(sequence_id, 1),
            _ => panic!("expected active test"),
        }
        let out = encode_message(&m).unwrap();
        assert_eq!(out, raw);
    }
}
