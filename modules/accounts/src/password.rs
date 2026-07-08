//! argon2id password hashing for the dev/password provider (port of Go's
//! `modules/accounts/password.go`). The encoded form is the PHC string
//! `$argon2id$v=19$m=65536,t=1,p=4$<b64salt>$<b64key>` — byte-compatible with Go's
//! hand-rolled `fmt.Sprintf` rendering (both sides use standard base64 without
//! padding), so a hash minted by the Go sketch verifies here and vice versa; the
//! parity test pins this with a real Go-produced fixture.

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher as _, PasswordVerifier as _, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};

/// Go's parameters (OWASP-ish defaults): m=64 MiB, t=1, p=4, 32-byte key.
const ARGON_MEMORY_KIB: u32 = 64 * 1024;
const ARGON_TIME: u32 = 1;
const ARGON_THREADS: u32 = 4;
const ARGON_KEY_LEN: usize = 32;

fn hasher() -> Argon2<'static> {
    Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        Params::new(ARGON_MEMORY_KIB, ARGON_TIME, ARGON_THREADS, Some(ARGON_KEY_LEN))
            .expect("static argon2 params are valid"),
    )
}

/// Hashes `pw` with a fresh 16-byte salt into the PHC string above.
pub(crate) fn hash_password(pw: &str) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = hasher()
        .hash_password(pw.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("argon2 hash failed: {e}"))?;
    Ok(hash.to_string())
}

/// Verifies `pw` against an encoded PHC string, reading the params (m/t/p, salt,
/// key length) from the string itself — so hashes minted with other cost settings
/// (or by the Go sketch) still verify. Any malformed/garbage input is simply
/// `false`, mirroring Go's `verifyPassword`. Constant-time compare is inside the
/// `argon2` crate's `verify_password`.
pub(crate) fn verify_password(encoded: &str, pw: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(encoded) else {
        return false;
    };
    Argon2::default().verify_password(pw.as_bytes(), &parsed).is_ok()
}
