//! The guest/device provider: a credential this backend MINTS rather than verifies
//! against a third party. `create_guest` provisions a player and reveals the ticket
//! exactly once; a returning device replays it through `login_federated("guest", …)`,
//! which resolves this verifier like any other registry entry.
//!
//! The ticket is `"<subject>.<secret>"` — a server-minted UUID and a 32-byte OsRng
//! secret, both base64url/hex alphabets that contain no `.`, so the first `.` is an
//! unambiguous split. Only the secret's SHA-256 digest is stored: the secret carries
//! 256 bits of machine-chosen entropy, so there is no dictionary attack for argon2id
//! to slow down, and argon2 here would spend the module's two `argon_permits` on every
//! guest login (`apikeys` chose SHA-256 for the same reason).

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;
use rand::RngCore as _;
use sha2::{Digest, Sha256};

use crate::oidc::short_id;
use crate::providers::{CredentialVerifier, VerifiedSubject, VerifyError, GUEST};
use crate::store::Store;

const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// The widest guest ticket this provider will look at. A minted ticket is a 36-char
/// UUID + `.` + a 43-char base64url secret = 80 bytes; the cap leaves format headroom
/// while bounding the digest and the indexed lookup an anonymous caller can trigger.
/// Deliberately NOT the OIDC cap — a JWT's size bound says nothing about a ticket.
pub const MAX_GUEST_CREDENTIAL_BYTES: usize = 128;

/// A freshly minted guest ticket: what the client stores and replays, plus the digest
/// this backend keeps. The plaintext secret exists only in this return value.
pub(crate) struct GuestTicket {
    pub(crate) subject: String,
    pub(crate) secret_hash: String,
    pub(crate) credential: String,
}

/// Mints a guest identity: a v4 UUID subject and a 32-byte `OsRng` secret.
pub(crate) fn mint_ticket() -> GuestTicket {
    let subject = new_subject();
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let secret = B64.encode(bytes);
    GuestTicket {
        secret_hash: secret_hash(&secret),
        credential: format!("{subject}.{secret}"),
        subject,
    }
}

/// The stored form of a guest secret — SHA-256, base64url-no-pad (apikeys' encoding,
/// so the repo has one digest spelling and no `hex` dependency).
pub(crate) fn secret_hash(secret: &str) -> String {
    B64.encode(Sha256::digest(secret.as_bytes()))
}

/// A random v4 UUID rendered as text, from `OsRng` — the `accounts.identities.subject`
/// value for a guest. Hand-rendered rather than pulling a `uuid` dependency in for one
/// call site; the version/variant nibbles are set as RFC 4122 requires.
fn new_subject() -> String {
    let mut b = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex = |part: &[u8]| {
        part.iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    format!(
        "{}-{}-{}-{}-{}",
        hex(&b[0..4]),
        hex(&b[4..6]),
        hex(&b[6..8]),
        hex(&b[8..10]),
        hex(&b[10..16])
    )
}

/// Whether `subject` has the shape [`new_subject`] mints: five lowercase-hex groups of
/// 8-4-4-4-12. Pure and zero-I/O by construction, and checked BEFORE any SQL: the
/// subject half of a ticket is the only caller-controlled value this provider binds
/// raw, and Postgres rejecting it as a `text` parameter (a NUL byte is not
/// representable) would arrive as a server error — i.e. an anonymous caller could
/// choose to raise the `Infra` verdict that means "our IdP is down". A value this
/// backend could never have minted is a rejected credential, decided here.
///
/// Shape only, not the v4 version/variant nibbles: rejecting everything outside the
/// alphabet is what closes the defect, and every minted subject satisfies both.
pub(crate) fn is_minted_subject(subject: &str) -> bool {
    const GROUPS: [usize; 5] = [8, 4, 4, 4, 12];
    let mut groups = subject.split('-');
    for len in GROUPS {
        let Some(group) = groups.next() else {
            return false;
        };
        if group.len() != len || !group.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return false;
        }
    }
    groups.next().is_none()
}

/// The guest provider's verification face. Unlike an OIDC provider it needs no
/// configuration — its authority is this module's own store — so it is registered in
/// every process that has the accounts schema.
pub(crate) struct GuestCredentials {
    store: Store,
}

pub(crate) fn guest_credentials(store: Store) -> Arc<dyn CredentialVerifier> {
    Arc::new(GuestCredentials { store })
}

#[async_trait]
impl CredentialVerifier for GuestCredentials {
    fn max_credential_bytes(&self) -> usize {
        MAX_GUEST_CREDENTIAL_BYTES
    }

    /// Of the tickets that REACH this verifier (`login_federated` decides an empty or
    /// over-cap credential as `Invalid` → 400 before resolving anything), a
    /// wrong-shaped ticket, an unknown subject and a wrong secret are ONE answer
    /// (`Rejected` → 401) — and not merely by mapping branches onto one error: the
    /// subject and the digest are matched in a SINGLE query, so this code never learns
    /// that a subject exists. The shape check is not such a return either: it decides
    /// only whether the value could be a subject this backend ever minted, which is
    /// public knowledge about the format.
    async fn verify(&self, credential: &str) -> Result<VerifiedSubject, VerifyError> {
        let rejected = || VerifyError::Rejected(anyhow::anyhow!("invalid guest ticket"));
        let Some((subject, secret)) = credential.split_once('.') else {
            return Err(rejected());
        };
        if !is_minted_subject(subject) {
            return Err(rejected());
        }
        let found = self
            .store
            .guest_identity_matches(subject, &secret_hash(secret))
            .await
            .map_err(|e| VerifyError::Infra(e.into()))?;
        if !found {
            return Err(rejected());
        }
        Ok(VerifiedSubject {
            display_name: format!("{GUEST}:{}", short_id(subject)),
            subject: subject.to_string(),
        })
    }
}
