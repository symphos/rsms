use bytes::Buf;
use rsms_core::{decode_pstring, RsmsError};
use std::io::Cursor;

use crate::datatypes::{
    Bind, BindResp, Deliver, DeliverResp, Report, ReportResp, SgipSequence, Submit, SubmitResp,
    Trace, TraceResp, Unbind, UnbindResp,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SgipMessage {
    Bind(Bind),
    BindResp(BindResp),
    Unbind(Unbind),
    UnbindResp(UnbindResp),
    Submit(Submit),
    SubmitResp(SubmitResp),
    Deliver(Deliver),
    DeliverResp(DeliverResp),
    Report(Report),
    ReportResp(ReportResp),
    Trace(Trace),
    TraceResp(TraceResp),
    Unknown { command_id: u32, body: Vec<u8> },
}

pub fn decode_message(buf: &[u8]) -> Result<SgipMessage, RsmsError> {
    if buf.len() < 20 {
        return Err(RsmsError::Codec(
            "buffer too short for SGIP header".to_string(),
        ));
    }

    let mut cursor = Cursor::new(buf);
    let total_length = cursor.get_u32();
    let command_id = cursor.get_u32();
    let _sequence =
        SgipSequence::decode(&mut cursor).map_err(|e| RsmsError::Codec(e.to_string()))?;

    if (total_length as usize) < 20 || (total_length as usize) > 100_000 {
        return Err(RsmsError::Codec(format!(
            "invalid SGIP length: {}",
            total_length
        )));
    }

    let body = if buf.len() > 20 {
        buf[20..total_length as usize].to_vec()
    } else {
        vec![]
    };

    let msg = match command_id {
        0x00000001 => {
            let mut cursor = Cursor::new(body.as_slice());
            let login_type = cursor.get_u8();
            let login_name = decode_pstring(&mut cursor, 16)?;
            let login_password = decode_pstring(&mut cursor, 16)?;
            let mut reserve = [0u8; 8];
            cursor.copy_to_slice(&mut reserve);
            SgipMessage::Bind(Bind {
                login_type,
                login_name,
                login_password,
                reserve,
            })
        }
        0x80000001 => {
            let mut cursor = Cursor::new(body.as_slice());
            let result = cursor.get_u8();
            let mut reserve = [0u8; 8];
            cursor.copy_to_slice(&mut reserve);
            SgipMessage::BindResp(BindResp { result, reserve })
        }
        0x00000002 => SgipMessage::Unbind(Unbind),
        0x80000002 => SgipMessage::UnbindResp(UnbindResp),
        0x00000003 => {
            let mut cursor = Cursor::new(body.as_slice());
            let sp_number = decode_pstring(&mut cursor, 21)?;
            let charge_number = decode_pstring(&mut cursor, 21)?;
            let user_count = cursor.get_u8();
            let mut user_numbers = Vec::with_capacity(user_count as usize);
            for _ in 0..user_count {
                let user_num = decode_pstring(&mut cursor, 21)?;
                user_numbers.push(user_num);
            }
            let corp_id = decode_pstring(&mut cursor, 5)?;
            let service_type = decode_pstring(&mut cursor, 10)?;
            let fee_type = cursor.get_u8();
            let fee_value = decode_pstring(&mut cursor, 6)?;
            let given_value = decode_pstring(&mut cursor, 6)?;
            let agent_flag = cursor.get_u8();
            let morelate_to_mt_flag = cursor.get_u8();
            let priority = cursor.get_u8();
            let expire_time = decode_pstring(&mut cursor, 16)?;
            let schedule_time = decode_pstring(&mut cursor, 16)?;
            let report_flag = cursor.get_u8();
            let tppid = cursor.get_u8();
            let tpudhi = cursor.get_u8();
            let msg_fmt = cursor.get_u8();
            let message_type = cursor.get_u8();
            let message_length = cursor.get_u32() as usize;
            let message_content = if cursor.remaining() >= message_length {
                let mut content = vec![0u8; message_length];
                cursor.copy_to_slice(&mut content);
                content
            } else {
                vec![]
            };
            let mut reserve = [0u8; 8];
            cursor.copy_to_slice(&mut reserve);
            SgipMessage::Submit(Submit {
                sp_number,
                charge_number,
                user_count,
                user_numbers,
                corp_id,
                service_type,
                fee_type,
                fee_value,
                given_value,
                agent_flag,
                morelate_to_mt_flag,
                priority,
                expire_time,
                schedule_time,
                report_flag,
                tppid,
                tpudhi,
                msg_fmt,
                message_type,
                message_content,
                reserve,
            })
        }
        0x80000003 => {
            let mut cursor = Cursor::new(body.as_slice());
            let result = cursor.get_u32();
            SgipMessage::SubmitResp(SubmitResp { result })
        }
        0x00000004 => {
            let mut cursor = Cursor::new(body.as_slice());
            let user_number = decode_pstring(&mut cursor, 21)?;
            let sp_number = decode_pstring(&mut cursor, 21)?;
            let tppid = cursor.get_u8();
            let tpudhi = cursor.get_u8();
            let msg_fmt = cursor.get_u8();
            let message_length = cursor.get_u32() as usize;
            let message_content = if message_length > 0 && cursor.remaining() >= message_length {
                let mut content = vec![0u8; message_length];
                cursor.copy_to_slice(&mut content);
                content
            } else {
                vec![]
            };
            let mut reserve = [0u8; 8];
            if cursor.remaining() >= 8 {
                cursor.copy_to_slice(&mut reserve);
            }
            SgipMessage::Deliver(Deliver {
                user_number,
                sp_number,
                tppid,
                tpudhi,
                msg_fmt,
                message_content,
                reserve,
            })
        }
        0x80000004 => {
            let mut cursor = Cursor::new(body.as_slice());
            let result = cursor.get_u32();
            SgipMessage::DeliverResp(DeliverResp { result })
        }
        0x00000005 => {
            let mut cursor = Cursor::new(body.as_slice());
            let submit_sequence =
                SgipSequence::decode(&mut cursor).map_err(|e| RsmsError::Codec(e.to_string()))?;
            let report_type = cursor.get_u8();
            let user_number = decode_pstring(&mut cursor, 21)?;
            let state = cursor.get_u8();
            let error_code = cursor.get_u8();
            let mut reserve = [0u8; 8];
            cursor.copy_to_slice(&mut reserve);
            SgipMessage::Report(Report {
                submit_sequence,
                report_type,
                user_number,
                state,
                error_code,
                reserve,
            })
        }
        0x80000005 => SgipMessage::ReportResp(ReportResp { result: 0 }),
        0x00001000 => {
            let trace_value = decode_pstring(&mut cursor, 21)?;
            let mut reserve = [0u8; 8];
            cursor.copy_to_slice(&mut reserve);
            SgipMessage::Trace(Trace {
                trace_value,
                reserve,
            })
        }
        0x80001000 => {
            let result = cursor.get_u32();
            let trace_result = decode_pstring(&mut cursor, 21)?;
            let mut reserve = [0u8; 8];
            cursor.copy_to_slice(&mut reserve);
            SgipMessage::TraceResp(TraceResp {
                result,
                trace_result,
                reserve,
            })
        }
        _ => SgipMessage::Unknown { command_id, body },
    };

    Ok(msg)
}
