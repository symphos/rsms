use rsms_codec_smpp::version::{
    SmppVersion, SMPP_V34_DEST_ADDR_SIZE, SMPP_V34_SOURCE_ADDR_SIZE, SMPP_V50_DEST_ADDR_SIZE,
    SMPP_V50_SOURCE_ADDR_SIZE,
};

#[test]
fn test_smpp_version_from_interface_version_v34() {
    assert_eq!(
        SmppVersion::from_interface_version(0x34),
        Some(SmppVersion::V34)
    );
    assert_eq!(
        SmppVersion::from_interface_version(0x33),
        Some(SmppVersion::V34)
    );
    assert_eq!(
        SmppVersion::from_interface_version(0x32),
        Some(SmppVersion::V34)
    );
}

#[test]
fn test_smpp_version_from_interface_version_v50() {
    assert_eq!(
        SmppVersion::from_interface_version(0x50),
        Some(SmppVersion::V50)
    );
    assert_eq!(
        SmppVersion::from_interface_version(0x51),
        Some(SmppVersion::V50)
    );
}

#[test]
fn test_smpp_version_from_interface_version_invalid() {
    assert_eq!(SmppVersion::from_interface_version(0x00), None);
    assert_eq!(SmppVersion::from_interface_version(0x10), None);
    assert_eq!(SmppVersion::from_interface_version(0xFF), None);
}

#[test]
fn test_smpp_version_source_addr_size() {
    assert_eq!(SmppVersion::V34.source_addr_size(), 21);
    assert_eq!(SmppVersion::V50.source_addr_size(), 65);
}

#[test]
fn test_smpp_version_destination_addr_size() {
    assert_eq!(SmppVersion::V34.destination_addr_size(), 21);
    assert_eq!(SmppVersion::V50.destination_addr_size(), 65);
}

#[test]
fn test_smpp_version_password_size() {
    assert_eq!(SmppVersion::V34.password_size(), 9);
    assert_eq!(SmppVersion::V50.password_size(), 65);
}

#[test]
fn test_smpp_version_system_type_size() {
    assert_eq!(SmppVersion::V34.system_type_size(), 13);
    assert_eq!(SmppVersion::V50.system_type_size(), 32);
}

#[test]
fn test_smpp_version_service_type_size() {
    assert_eq!(SmppVersion::V34.service_type_size(), 6);
    assert_eq!(SmppVersion::V50.service_type_size(), 16);
}

#[test]
fn test_smpp_version_constants_v34() {
    assert_eq!(SMPP_V34_SOURCE_ADDR_SIZE, 21);
    assert_eq!(SMPP_V34_DEST_ADDR_SIZE, 21);
}

#[test]
fn test_smpp_version_constants_v50() {
    assert_eq!(SMPP_V50_SOURCE_ADDR_SIZE, 65);
    assert_eq!(SMPP_V50_DEST_ADDR_SIZE, 65);
}
