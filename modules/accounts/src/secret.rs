//! Client secrets: generation, the stored digest and the constant-time compare every
//! presented secret or CSRF token goes through.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rand::RngCore as _;
use sha2::{Digest, Sha256};

/// The prefix of a generated client secret.
pub(crate) const CLIENT_SECRET_PREFIX: &str = "cs_";

/// `cs_` and 32 `OsRng` bytes in base64url without padding.
pub(crate) fn new_client_secret() -> String {
    let mut b = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut b);
    format!("{CLIENT_SECRET_PREFIX}{}", URL_SAFE_NO_PAD.encode(b))
}

/// The stored form of a secret: SHA-256, base64url without padding (apikeys' encoding).
pub(crate) fn digest(secret: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(secret.as_bytes()))
}

/// Byte equality whose time depends only on the lengths.
pub(crate) fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
#[path = "secret_tests.rs"]
mod tests;
