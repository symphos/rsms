#[cfg(test)]
mod tests {
    use bytes::{Buf, BufMut, BytesMut};
    use std::io::Cursor;

    use crate::codec::{CodecError, Decodable, Encodable, PduHeader};
    use crate::datatypes::{
        BindTransmitter, CommandId, CommandStatus, EnquireLink, GenericNack, SubmitSm,
    };

    #[test]
    fn test_command_id_is_response() {
        assert!(!CommandId::BIND_TRANSMITTER.is_response());
        assert!(CommandId::BIND_TRANSMITTER_RESP.is_response());
        assert!(!CommandId::SUBMIT_SM.is_response());
        assert!(CommandId::SUBMIT_SM_RESP.is_response());
        assert!(!CommandId::ENQUIRE_LINK.is_response());
        assert!(CommandId::ENQUIRE_LINK_RESP.is_response());
    }

    #[test]
    fn test_pdu_header_encode_decode() {
        let header = PduHeader {
            command_length: 24,
            command_id: CommandId::ENQUIRE_LINK,
            command_status: CommandStatus::ESME_ROK,
            sequence_number: 42,
        };

        let mut buf = BytesMut::new();
        header.encode(&mut buf).unwrap();

        let mut cursor = Cursor::new(buf.as_ref());
        let decoded = PduHeader::decode(&mut cursor).unwrap();

        assert_eq!(header, decoded);
    }

    #[test]
    fn test_enquire_link_roundtrip() {
        let original = EnquireLink::new();

        let mut buf = BytesMut::new();
        original.encode(&mut buf).unwrap();
        let body_len = buf.len();

        let header = PduHeader {
            command_length: (body_len + PduHeader::SIZE) as u32,
            command_id: CommandId::ENQUIRE_LINK,
            command_status: CommandStatus::ESME_ROK,
            sequence_number: 1,
        };
        header.encode(&mut buf).unwrap();

        let mut cursor = Cursor::new(buf.as_ref());
        let decoded_header = PduHeader::decode(&mut cursor).unwrap();
        assert_eq!(decoded_header.command_id, CommandId::ENQUIRE_LINK);

        let decoded = EnquireLink::decode(decoded_header, &mut cursor).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_bind_transmitter_roundtrip() {
        let original = BindTransmitter::new("test_system", "password", "SMPP", 0x34);

        // Build PDU: header + body
        let mut body_buf = BytesMut::new();
        original.encode(&mut body_buf).unwrap();
        let body_len = body_buf.len();

        let header = PduHeader {
            command_length: (body_len + PduHeader::SIZE) as u32,
            command_id: CommandId::BIND_TRANSMITTER,
            command_status: CommandStatus::ESME_ROK,
            sequence_number: 1,
        };

        let mut full_buf = BytesMut::new();
        header.encode(&mut full_buf).unwrap();
        full_buf.extend_from_slice(&body_buf);

        let mut cursor = Cursor::new(full_buf.as_ref());
        let decoded_header = PduHeader::decode(&mut cursor).unwrap();
        assert_eq!(decoded_header.command_id, CommandId::BIND_TRANSMITTER);
        assert_eq!(decoded_header.command_status, CommandStatus::ESME_ROK);

        let decoded = BindTransmitter::decode(decoded_header, &mut cursor).unwrap();
        assert_eq!(original.system_id, decoded.system_id);
        assert_eq!(original.password, decoded.password);
    }

    #[test]
    fn test_submit_sm_roundtrip() {
        let mut original = SubmitSm::new();
        original.service_type = "CMT".to_string();
        original.source_addr = "1234567890".to_string();
        original.destination_addr = "0987654321".to_string();
        original.short_message = b"Hello, World!".to_vec();

        let mut body_buf = BytesMut::new();
        original.encode(&mut body_buf).unwrap();
        let body_len = body_buf.len();

        let header = PduHeader {
            command_length: (body_len + PduHeader::SIZE) as u32,
            command_id: CommandId::SUBMIT_SM,
            command_status: CommandStatus::ESME_ROK,
            sequence_number: 1,
        };

        let mut full_buf = BytesMut::new();
        header.encode(&mut full_buf).unwrap();
        full_buf.extend_from_slice(&body_buf);

        let mut cursor = Cursor::new(full_buf.as_ref());
        let decoded_header = PduHeader::decode(&mut cursor).unwrap();
        assert_eq!(decoded_header.command_id, CommandId::SUBMIT_SM);

        let decoded = SubmitSm::decode(decoded_header, &mut cursor).unwrap();
        assert_eq!(original.destination_addr, decoded.destination_addr);
        assert_eq!(original.source_addr, decoded.source_addr);
    }

    #[test]
    fn test_codec_error_to_command_status() {
        let err = CodecError::InvalidCommandId(0x9999);
        assert_eq!(err.to_command_status(), CommandStatus::ESME_RINVCMDID);

        let err = CodecError::InvalidPduLength {
            length: 5,
            min: 16,
            max: 65536,
        };
        assert_eq!(err.to_command_status(), CommandStatus::ESME_RINVMSGLEN);
    }

    #[test]
    fn test_generic_nack_decode() {
        let mut encoded = BytesMut::new();
        encoded.put_u32(16); // command_length
        encoded.put_u32(0x80000000u32); // command_id = GENERIC_NACK
        encoded.put_u32(0x00000003); // command_status = InvalidCommandId
        encoded.put_u32(123); // sequence_number

        let mut cursor = Cursor::new(encoded.as_ref());
        let header = PduHeader::decode(&mut cursor).unwrap();
        let decoded = GenericNack::decode(header, &mut cursor).unwrap();

        assert_eq!(decoded, GenericNack::new());
    }
}
