//! argon2id password hashing for the admin GameOps identity — copied from
//! `modules/accounts/src/password.rs` (the pattern source; tables and identity
//! domains stay separate, so the code is duplicated rather than shared through a
//! contract crate). The encoded form is the PHC string
//! `$argon2id$v=19$m=65536,t=1,p=4$<b64salt>$<b64key>`.
//!
//! Both fns are `pub`: `tools/adminctl` (the operator CLI that mints admin users)
//! calls them so the installer and the login path can never drift on parameters.

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher as _, PasswordVerifier as _, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};

/// OWASP-ish defaults (accounts parity): m=64 MiB, t=1, p=4, 32-byte key.
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
pub fn hash_password(pw: &str) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = hasher()
        .hash_password(pw.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("argon2 hash failed: {e}"))?;
    Ok(hash.to_string())
}

/// Verifies `pw` against an encoded PHC string, reading the params (m/t/p, salt,
/// key length) from the string itself — hashes minted with other cost settings
/// still verify. Any malformed/garbage input is simply `false`. Constant-time
/// compare is inside the `argon2` crate's `verify_password`.
pub fn verify_password(encoded: &str, pw: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(encoded) else {
        return false;
    };
    Argon2::default().verify_password(pw.as_bytes(), &parsed).is_ok()
}

/// Test-only: exposes this module's argon2 cost parameters (memory KiB, time,
/// parallelism, output key length) so `cmd/server`'s cross-module parity test can
/// assert accounts' and admin's security-cost twins never drift silently.
pub(crate) fn argon2_params() -> (u32, u32, u32, usize) {
    (ARGON_MEMORY_KIB, ARGON_TIME, ARGON_THREADS, ARGON_KEY_LEN)
}
