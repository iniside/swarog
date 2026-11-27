//! The client secret's shape, its digest and the constant-time compare.

use super::{ct_eq, digest, new_client_secret, CLIENT_SECRET_PREFIX};

#[test]
fn a_client_secret_is_cs_and_43_base64url_characters() {
    for _ in 0..32 {
        let secret = new_client_secret();
        let body = secret.strip_prefix(CLIENT_SECRET_PREFIX).expect("the cs_ prefix");
        assert_eq!(body.len(), 43, "32 bytes unpadded");
        assert!(body.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
    }
    assert_ne!(new_client_secret(), new_client_secret());
}

#[test]
fn the_digest_is_unpadded_base64url_sha256() {
    assert_eq!(digest("abc"), "ungWv48Bz-pBQUDeXa4iI7ADYaOWF3qctBD_YfIAFa0");
}

#[test]
fn ct_eq_is_byte_equality() {
    assert!(ct_eq(b"same", b"same"));
    assert!(!ct_eq(b"same", b"samf"));
    assert!(!ct_eq(b"same", b"sam"));
    assert!(ct_eq(b"", b""));
}
