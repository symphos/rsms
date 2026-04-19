use bytes::{BufMut, BytesMut};
use std::io::Read;

pub fn encode_pstring(
    buf: &mut BytesMut,
    value: &str,
    max_len: usize,
    field_name: &'static str,
) -> Result<(), String> {
    let bytes = value.as_bytes();
    if bytes.len() > max_len {
        return Err(format!(
            "field '{}' length {} exceeds max {} bytes",
            field_name,
            bytes.len(),
            max_len
        ));
    }
    buf.put_slice(bytes);
    buf.put_bytes(0, max_len - bytes.len());
    Ok(())
}

pub fn decode_pstring(
    buf: &mut std::io::Cursor<&[u8]>,
    max_len: usize,
) -> Result<String, std::io::Error> {
    let mut bytes = vec![0u8; max_len];
    buf.read_exact(&mut bytes)?;
    let trimmed = bytes
        .iter()
        .rposition(|&b| b != 0)
        .map(|pos| &bytes[..pos + 1])
        .unwrap_or(&[]);
    Ok(String::from_utf8_lossy(trimmed).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn encode_pstring_exact_fit() {
        let mut buf = BytesMut::new();
        encode_pstring(&mut buf, "106900", 6, "test").unwrap();
        assert_eq!(&buf[..], b"106900");
    }

    #[test]
    fn encode_pstring_with_padding() {
        let mut buf = BytesMut::new();
        encode_pstring(&mut buf, "SMS", 10, "test").unwrap();
        assert_eq!(buf.len(), 10);
        assert_eq!(&buf[..3], b"SMS");
        assert_eq!(&buf[3..], &[0u8; 7]);
    }

    #[test]
    fn encode_pstring_empty() {
        let mut buf = BytesMut::new();
        encode_pstring(&mut buf, "", 6, "test").unwrap();
        assert_eq!(&buf[..], &[0u8; 6]);
    }

    #[test]
    fn encode_pstring_overflow_error() {
        let mut buf = BytesMut::new();
        let result = encode_pstring(&mut buf, "this_is_too_long", 10, "service_id");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("service_id"));
    }

    #[test]
    fn decode_pstring_with_null_padding() {
        let data = [b'S', b'M', b'S', 0, 0, 0, 0, 0, 0, 0];
        let mut cursor = Cursor::new(&data[..]);
        let result = decode_pstring(&mut cursor, 10).unwrap();
        assert_eq!(result, "SMS");
    }

    #[test]
    fn decode_pstring_all_nulls() {
        let data = [0u8; 10];
        let mut cursor = Cursor::new(&data[..]);
        let result = decode_pstring(&mut cursor, 10).unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn decode_pstring_exact_fit() {
        let data = *b"106900";
        let mut cursor = Cursor::new(&data[..]);
        let result = decode_pstring(&mut cursor, 6).unwrap();
        assert_eq!(result, "106900");
    }

    #[test]
    fn roundtrip_encode_decode() {
        let mut buf = BytesMut::new();
        encode_pstring(&mut buf, "SP001", 16, "login").unwrap();
        let mut cursor = Cursor::new(&buf[..]);
        let decoded = decode_pstring(&mut cursor, 16).unwrap();
        assert_eq!(decoded, "SP001");
    }
}
