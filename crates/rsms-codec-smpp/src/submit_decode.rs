use bytes::Buf;
use rsms_core::RsmsError;
use std::io::Cursor;

use crate::codec::decode_cstring;
use crate::datatypes::{DeliverSm, SubmitSm};
use crate::version::SmppVersion;

fn default_version(version: Option<SmppVersion>) -> SmppVersion {
    version.unwrap_or(SmppVersion::V34)
}

pub fn decode_submit_sm(
    version: Option<SmppVersion>,
    _header_length: u32,
    body: &[u8],
) -> Result<SubmitSm, RsmsError> {
    let v = default_version(version);
    let mut cursor = Cursor::new(body);

    if body.len() < 10 {
        return Err(RsmsError::Codec("SubmitSm body too short".to_string()));
    }

    let service_type = decode_cstring(&mut cursor, v.service_type_size(), "service_type")
        .map_err(|e| RsmsError::Codec(e.to_string()))?;
    let source_addr_ton = cursor.get_u8();
    let source_addr_npi = cursor.get_u8();
    let source_addr = decode_cstring(&mut cursor, v.source_addr_size(), "source_addr")
        .map_err(|e| RsmsError::Codec(e.to_string()))?;
    let dest_addr_ton = cursor.get_u8();
    let dest_addr_npi = cursor.get_u8();
    let destination_addr =
        decode_cstring(&mut cursor, v.destination_addr_size(), "destination_addr")
            .map_err(|e| RsmsError::Codec(e.to_string()))?;
    let esm_class = cursor.get_u8();
    let protocol_id = cursor.get_u8();
    let priority_flag = cursor.get_u8();
    let schedule_delivery_time = decode_cstring(&mut cursor, 17, "schedule_delivery_time")
        .map_err(|e| RsmsError::Codec(e.to_string()))?;
    let validity_period = decode_cstring(&mut cursor, 17, "validity_period")
        .map_err(|e| RsmsError::Codec(e.to_string()))?;
    let registered_delivery = cursor.get_u8();
    let replace_if_present_flag = cursor.get_u8();
    let data_coding = cursor.get_u8();
    let sm_default_msg_id = cursor.get_u8();
    let sm_length = cursor.get_u8();
    let short_message = if sm_length > 0 && cursor.remaining() >= sm_length as usize {
        let mut msg = vec![0u8; sm_length as usize];
        cursor.copy_to_slice(&mut msg);
        msg
    } else {
        vec![]
    };

    Ok(SubmitSm {
        service_type,
        source_addr_ton,
        source_addr_npi,
        source_addr,
        dest_addr_ton,
        dest_addr_npi,
        destination_addr,
        esm_class,
        protocol_id,
        priority_flag,
        schedule_delivery_time,
        validity_period,
        registered_delivery,
        replace_if_present_flag,
        data_coding,
        sm_default_msg_id,
        short_message,
        tlvs: vec![],
    })
}

pub fn decode_deliver_sm(
    version: Option<SmppVersion>,
    _header_length: u32,
    body: &[u8],
) -> Result<DeliverSm, RsmsError> {
    let v = default_version(version);
    let mut cursor = Cursor::new(body);

    if body.len() < 6 {
        return Err(RsmsError::Codec("DeliverSm body too short".to_string()));
    }

    let service_type = decode_cstring(&mut cursor, v.service_type_size(), "service_type")
        .map_err(|e| RsmsError::Codec(e.to_string()))?;
    let source_addr_ton = cursor.get_u8();
    let source_addr_npi = cursor.get_u8();
    let source_addr = decode_cstring(&mut cursor, v.source_addr_size(), "source_addr")
        .map_err(|e| RsmsError::Codec(e.to_string()))?;
    let dest_addr_ton = cursor.get_u8();
    let dest_addr_npi = cursor.get_u8();
    let destination_addr =
        decode_cstring(&mut cursor, v.destination_addr_size(), "destination_addr")
            .map_err(|e| RsmsError::Codec(e.to_string()))?;
    let esm_class = cursor.get_u8();
    let protocol_id = cursor.get_u8();
    let priority_flag = cursor.get_u8();
    let schedule_delivery_time = decode_cstring(&mut cursor, 17, "schedule_delivery_time")
        .map_err(|e| RsmsError::Codec(e.to_string()))?;
    let validity_period = decode_cstring(&mut cursor, 17, "validity_period")
        .map_err(|e| RsmsError::Codec(e.to_string()))?;
    let registered_delivery = cursor.get_u8();
    let replace_if_present_flag = cursor.get_u8();
    let data_coding = cursor.get_u8();
    let sm_default_msg_id = cursor.get_u8();
    let sm_length = cursor.get_u8();
    let short_message = if sm_length > 0 && cursor.remaining() >= sm_length as usize {
        let mut msg = vec![0u8; sm_length as usize];
        cursor.copy_to_slice(&mut msg);
        msg
    } else {
        vec![]
    };

    Ok(DeliverSm {
        service_type,
        source_addr_ton,
        source_addr_npi,
        source_addr,
        dest_addr_ton,
        dest_addr_npi,
        destination_addr,
        esm_class,
        protocol_id,
        priority_flag,
        schedule_delivery_time,
        validity_period,
        registered_delivery,
        replace_if_present_flag,
        data_coding,
        sm_default_msg_id,
        short_message,
        tlvs: vec![],
    })
}
