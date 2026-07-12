use std::io::Cursor;

use super::linux::read_identity;

#[test]
fn identity_handshake_rejects_truncation() {
    let mut truncated = Cursor::new([1u8, 0, 0, 0]);
    assert_eq!(
        read_identity(&mut truncated).unwrap_err().kind(),
        std::io::ErrorKind::UnexpectedEof
    );
}

#[test]
fn identity_handshake_rejects_injected_unbounded_path() {
    let mut message = Vec::new();
    message.extend_from_slice(&1u32.to_ne_bytes());
    message.extend_from_slice(&2u64.to_ne_bytes());
    message.extend_from_slice(&(1024u32 * 1024 + 1).to_ne_bytes());
    let mut injected = Cursor::new(message);
    assert_eq!(
        read_identity(&mut injected).unwrap_err().kind(),
        std::io::ErrorKind::InvalidData
    );
}
