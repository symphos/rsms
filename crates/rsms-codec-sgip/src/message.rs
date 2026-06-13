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

#[cfg(test)]
mod ice_truncation_tests {
    use super::*;

    // 已证实当前会 panic：解码器中未受保护的 get_u8()/get_u32() 在 body 过短时越界 panic
    // （独立于 copy_to_slice 的更广面）。修复全量解码器健壮性后取消 ignore。
    #[test]
    fn decode_message_short_body_never_panics() {
        // 回归：实际服务端解码路径 message.rs::decode_message 对任意过短 body 不得 panic
        let cmds = [
            0x00000001u32, 0x80000001, 0x00000003, 0x00000004, 0x00000005, 0x00001000, 0x80001000,
        ];
        for cmd in cmds {
            for body_len in 0..48usize {
                let total = 20 + body_len;
                let mut pdu = vec![0u8; total];
                pdu[0..4].copy_from_slice(&(total as u32).to_be_bytes());
                pdu[4..8].copy_from_slice(&cmd.to_be_bytes());
                let _ = decode_message(&pdu); // 不得 panic
            }
        }
    }
}

/// 解码一条 SGIP PDU 字节序列为 `SgipMessage`。
///
/// SGIP 头部为 20 字节（4B 长度 + 4B 命令字 + 12B `SgipSequence`），
/// `sequence_id` 偏移为 12（与 SMPP 一致，不同于 CMPP/SMGP 的偏移 8）。
/// 未知命令字映射为 `SgipMessage::Unknown`，不返回错误。
pub fn decode_message(buf: &[u8]) -> Result<SgipMessage, RsmsError> {
    if buf.len() < 20 {
        return Err(RsmsError::Codec(
            "buffer too short for SGIP header".to_string(),
        ));
    }

    let mut cursor = Cursor::new(buf);
    let total_length = cursor
        .try_get_u32()
        .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
    let command_id = cursor
        .try_get_u32()
        .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
    let _sequence =
        SgipSequence::decode(&mut cursor).map_err(|e| RsmsError::Codec(e.to_string()))?;

    if (total_length as usize) < 20 || (total_length as usize) > 100_000 {
        return Err(RsmsError::Codec(format!(
            "invalid SGIP length: {}",
            total_length
        )));
    }

    let body_end = (total_length as usize).min(buf.len());
    let body = if body_end > 20 {
        buf[20..body_end].to_vec()
    } else {
        vec![]
    };

    let msg = match command_id {
        0x00000001 => {
            let mut cursor = Cursor::new(body.as_slice());
            let login_type = cursor
                .try_get_u8()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            let login_name = decode_pstring(&mut cursor, 16)?;
            let login_password = decode_pstring(&mut cursor, 16)?;
            let mut reserve = [0u8; 8];
            if cursor.remaining() >= 8 {
                cursor.copy_to_slice(&mut reserve);
            }
            SgipMessage::Bind(Bind {
                login_type,
                login_name,
                login_password,
                reserve,
            })
        }
        0x80000001 => {
            let mut cursor = Cursor::new(body.as_slice());
            let result = cursor
                .try_get_u8()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            let mut reserve = [0u8; 8];
            if cursor.remaining() >= 8 {
                cursor.copy_to_slice(&mut reserve);
            }
            SgipMessage::BindResp(BindResp { result, reserve })
        }
        0x00000002 => SgipMessage::Unbind(Unbind),
        0x80000002 => SgipMessage::UnbindResp(UnbindResp),
        0x00000003 => {
            let mut cursor = Cursor::new(body.as_slice());
            let sp_number = decode_pstring(&mut cursor, 21)?;
            let charge_number = decode_pstring(&mut cursor, 21)?;
            let user_count = cursor
                .try_get_u8()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            let mut user_numbers = Vec::with_capacity(user_count as usize);
            for _ in 0..user_count {
                let user_num = decode_pstring(&mut cursor, 21)?;
                user_numbers.push(user_num);
            }
            let corp_id = decode_pstring(&mut cursor, 5)?;
            let service_type = decode_pstring(&mut cursor, 10)?;
            let fee_type = cursor
                .try_get_u8()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            let fee_value = decode_pstring(&mut cursor, 6)?;
            let given_value = decode_pstring(&mut cursor, 6)?;
            let agent_flag = cursor
                .try_get_u8()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            let morelate_to_mt_flag = cursor
                .try_get_u8()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            let priority = cursor
                .try_get_u8()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            let expire_time = decode_pstring(&mut cursor, 16)?;
            let schedule_time = decode_pstring(&mut cursor, 16)?;
            let report_flag = cursor
                .try_get_u8()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            let tppid = cursor
                .try_get_u8()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            let tpudhi = cursor
                .try_get_u8()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            let msg_fmt = cursor
                .try_get_u8()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            let message_type = cursor
                .try_get_u8()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            let message_length = cursor
                .try_get_u32()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?
                as usize;
            let message_content = if cursor.remaining() >= message_length {
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
            let result = cursor
                .try_get_u32()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            SgipMessage::SubmitResp(SubmitResp { result })
        }
        0x00000004 => {
            let mut cursor = Cursor::new(body.as_slice());
            let user_number = decode_pstring(&mut cursor, 21)?;
            let sp_number = decode_pstring(&mut cursor, 21)?;
            let tppid = cursor
                .try_get_u8()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            let tpudhi = cursor
                .try_get_u8()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            let msg_fmt = cursor
                .try_get_u8()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            let message_length = cursor
                .try_get_u32()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?
                as usize;
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
            let result = cursor
                .try_get_u32()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            SgipMessage::DeliverResp(DeliverResp { result })
        }
        0x00000005 => {
            let mut cursor = Cursor::new(body.as_slice());
            let submit_sequence =
                SgipSequence::decode(&mut cursor).map_err(|e| RsmsError::Codec(e.to_string()))?;
            let report_type = cursor
                .try_get_u8()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            let user_number = decode_pstring(&mut cursor, 21)?;
            let state = cursor
                .try_get_u8()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            let error_code = cursor
                .try_get_u8()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            let mut reserve = [0u8; 8];
            if cursor.remaining() >= 8 {
                cursor.copy_to_slice(&mut reserve);
            }
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
            let mut cursor = Cursor::new(body.as_slice());
            let trace_value = decode_pstring(&mut cursor, 21)?;
            let mut reserve = [0u8; 8];
            if cursor.remaining() >= 8 {
                cursor.copy_to_slice(&mut reserve);
            }
            SgipMessage::Trace(Trace {
                trace_value,
                reserve,
            })
        }
        0x80001000 => {
            let mut cursor = Cursor::new(body.as_slice());
            let result = cursor
                .try_get_u32()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            let trace_result = decode_pstring(&mut cursor, 21)?;
            let mut reserve = [0u8; 8];
            if cursor.remaining() >= 8 {
                cursor.copy_to_slice(&mut reserve);
            }
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
