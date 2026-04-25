use bytes::{Buf, BufMut, BytesMut};
use std::io::Cursor;

#[derive(Debug)]
pub enum CstringError {
    Incomplete,
    FieldTooLong { field: &'static str, max_len: usize },
}

impl std::fmt::Display for CstringError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CstringError::Incomplete => write!(f, "buffer incomplete for cstring"),
            CstringError::FieldTooLong { field, max_len } => {
                write!(f, "{} exceeds maximum length of {} bytes", field, max_len - 1)
            }
        }
    }
}

pub fn decode_cstring(
    buf: &mut Cursor<&[u8]>,
    max_len: usize,
    field_name: &'static str,
) -> Result<String, CstringError> {
    let mut string_bytes = Vec::new();
    let mut bytes_read = 0;

    while bytes_read < max_len {
        if !buf.has_remaining() {
            return Err(CstringError::Incomplete);
        }
        let byte = buf.get_u8();
        bytes_read += 1;
        if byte == 0 {
            return Ok(String::from_utf8_lossy(&string_bytes).into_owned());
        }
        string_bytes.push(byte);
    }

    Err(CstringError::FieldTooLong { field: field_name, max_len })
}

pub fn encode_cstring(buf: &mut BytesMut, value: &str, max_len: usize) -> Result<(), CstringError> {
    let bytes = value.as_bytes();
    if bytes.len() >= max_len {
        return Err(CstringError::FieldTooLong { field: "cstring", max_len });
    }
    buf.put(bytes);
    buf.put_u8(0);
    Ok(())
}
