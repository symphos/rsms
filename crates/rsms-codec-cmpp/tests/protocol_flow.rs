use rsms_codec_cmpp::{
    CmppVersion, Connect, ConnectResp, Decodable, Pdu, PduHeader, PduRegistry, Submit,
};

#[test]
fn cmpp_connect_flow_encode_decode() {
    let connect = Connect {
        source_addr: "106900".to_string(),
        authenticator_source: [0x12; 16],
        version: 0x30,
        timestamp: 0x00000000,
    };

    let encoded = Pdu::from(connect).to_pdu_bytes(12345);
    assert_eq!(encoded.len(), 12 + 27);

    let mut cursor = std::io::Cursor::new(encoded.as_slice());
    let header = PduHeader::decode(&mut cursor).expect("decode header failed");
    let decoded = Connect::decode(header, &mut cursor).expect("decode failed");
    assert_eq!(decoded.source_addr, "106900");
    assert_eq!(decoded.authenticator_source, [0x12; 16]);
    assert_eq!(decoded.version, 0x30);
}

#[test]
fn cmpp_connect_resp_encode_decode() {
    let resp = ConnectResp {
        status: 0,
        authenticator_ismg: [0xaa; 16],
        version: 0x30,
    };

    let encoded = Pdu::from(resp).to_pdu_bytes(12345);
    assert_eq!(encoded.len(), 12 + 21);

    let mut cursor = std::io::Cursor::new(encoded.as_slice());
    let header = PduHeader::decode(&mut cursor).expect("decode header failed");
    let decoded = ConnectResp::decode(header, &mut cursor).expect("decode failed");
    assert_eq!(decoded.status, 0);
    assert_eq!(decoded.version, 0x30);
}

#[test]
fn cmpp_submit_with_body_roundtrip() {
    let mut submit = Submit::new();
    submit.msg_content = vec![0x00; 151];

    let encoded = Pdu::from(submit).to_pdu_bytes(999);
    // header(12) + submit body fields + 151 bytes msg_content
    assert!(encoded.len() > 12 + 151);

    let mut cursor = std::io::Cursor::new(encoded.as_slice());
    let header = PduHeader::decode(&mut cursor).expect("decode header failed");
    let decoded = Submit::decode(header, &mut cursor).expect("decode failed");
    assert_eq!(decoded.msg_content.len(), 151);
}

#[test]
fn cmpp_pdu_registry_dispatch() {
    let registry = PduRegistry::for_version(CmppVersion::V30);

    let connect = Connect {
        source_addr: "106900".to_string(),
        authenticator_source: [0x12; 16],
        version: 0x30,
        timestamp: 0x00000000,
    };

    let encoded = Pdu::from(connect).to_pdu_bytes(12345);
    let mut cursor = std::io::Cursor::new(encoded.as_slice());
    let header = PduHeader::decode(&mut cursor).expect("decode header failed");

    let body_start = cursor.position() as usize;
    let body = &encoded.as_slice()[body_start..];

    let pdu = registry.dispatch(header, body).expect("dispatch failed");
    assert!(matches!(pdu, Pdu::Connect(_)));
}
