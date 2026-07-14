//! The SQL layer for the `apikeys` schema — pure persistence, no capability/edge
//! knowledge. Two normalized relations:
//!   - `apikeys.roles(name, policy, revision, …)` — a named, reusable policy.
//!   - `apikeys.keys(name, secret_hash, prefix, role → roles.name, revision, …)` — one
//!     credential, referencing exactly one role by name (the FK is the authority; the
//!     effective policy is resolved by a JOIN, so editing a role's policy immediately
//!     changes every key that references it).
//!
//! Secrets are SERVER-GENERATED and stored ONLY as a SHA-256 digest (`secret_hash`) plus
//! a display `prefix`; the plaintext is returned exactly once (from [`Store::create_key`])
//! and never persisted, so `list_keys` can never expose it. Lookup hashes the presented
//! key and matches the digest — an O(1) indexed read.
//!
//! Optimistic concurrency is CAS-by-`revision`: every mutating UPDATE/DELETE carries the
//! `expected_revision` the operator's form was rendered against and touches the row only
//! when it still matches, bumping `revision` on success. A miss (`rows_affected() == 0`)
//! is a [`WriteError::Conflict`], NEVER a not-found — a stale form and a vanished target
//! are the same "your evidence no longer holds" answer, and surfacing them as NotFound
//! would be indistinguishable from the edge's UnknownMethod (see the module docs).

use apikeysapi::KeyRecord;
use base64::Engine;
use sha2::{Digest, Sha256};
use sqlx::PgPool;

/// The base64url-no-pad engine used for BOTH the 32-byte secret body and the SHA-256
/// digest at rest. base64url is the workspace's existing encoder (`base64` crate); a
/// `hex` dependency would be a redundant second encoder. The digest is thus a 43-char
/// base64url string, not 64-char hex — any consumer that recomputes it (e.g. a
/// cross-process proof) MUST use this same `URL_SAFE_NO_PAD.encode(Sha256::digest(..))`.
const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// SHA-256 of `input`, base64url-no-pad encoded — the on-disk `secret_hash` form.
pub(crate) fn secret_hash(input: &str) -> String {
    B64.encode(Sha256::digest(input.as_bytes()))
}

/// The number of characters of the generated secret kept as the display `prefix`
/// (`ak_` + the first 9 base64url chars). A standard credential-prefix convention
/// (Stripe/GitHub-style) — it reveals a bounded, non-load-bearing slice while the
/// remaining ~250 bits of entropy keep the secret unguessable.
const PREFIX_CHARS: usize = 12;

/// Mints a fresh API-key secret: 32 cryptographically random bytes rendered as
/// `ak_<base64url>`, its [`secret_hash`], and its display `prefix`. The generated secret
/// is guarded against [`apikeysapi::MAX_KEY_BYTES`] — the SAME cap the gateway enforces
/// on a PRESENTED key (`modules/gateway/src/keys.rs`) — so a minted key can never be one
/// the gateway would reject as over-length. (`ak_` + 43 base64url chars = 46 bytes, well
/// under the cap; the guard is a belt against a future format change.)
pub(crate) fn generate_secret() -> Result<(String, String, String), WriteError> {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    let mut rng = rand::rngs::OsRng;
    rng.fill_bytes(&mut bytes);
    let secret = format!("ak_{}", B64.encode(bytes));
    if secret.len() > apikeysapi::MAX_KEY_BYTES {
        return Err(WriteError::Invalid(format!(
            "apikeys: generated secret is {} bytes, exceeding apikeysapi::MAX_KEY_BYTES ({})",
            secret.len(),
            apikeysapi::MAX_KEY_BYTES,
        )));
    }
    let hash = secret_hash(&secret);
    let prefix: String = secret.chars().take(PREFIX_CHARS).collect();
    Ok((secret, hash, prefix))
}

/// Loose policy validation (Decision 4): non-empty, and either the literal `full` or a
/// comma-separated list whose every entry is non-blank. Deliberately NOT a strict
/// method-name check — ops evolve, and an operator may pre-authorize a method no process
/// serves yet (Step 7 turns this into a CheckboxGroup sourced from the ops catalog).
fn validate_policy(policy: &str) -> Result<(), WriteError> {
    if policy.trim().is_empty() || policy.split(',').any(|m| m.trim().is_empty()) {
        return Err(WriteError::Invalid(format!(
            "apikeys: invalid policy {policy:?} (must be `full` or a comma-separated method list)"
        )));
    }
    Ok(())
}

/// A non-empty, trimmed name (roles + keys). Length is capped so a name can't bloat a
/// hidden `_expected_*_rev_<name>` form field unreasonably.
fn validate_name(what: &str, name: &str) -> Result<(), WriteError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(WriteError::Invalid(format!("apikeys: {what} name must not be blank")));
    }
    if trimmed != name {
        return Err(WriteError::Invalid(format!(
            "apikeys: {what} name {name:?} must not have leading/trailing whitespace"
        )));
    }
    if name.len() > 128 {
        return Err(WriteError::Invalid(format!(
            "apikeys: {what} name is {} bytes, exceeding the 128-byte cap",
            name.len()
        )));
    }
    Ok(())
}

/// A write outcome that distinguishes a DOMAIN conflict (CAS miss, duplicate name, or an
/// FK violation — a referenced role missing, or a role still referenced by a key) from a
/// validation error and from raw store trouble. This is the authority for finding #2:
/// none of these map to `NotFound` — a domain conflict is a [`opsapi::Status::Conflict`]
/// (409) and never masquerades as the edge's UnknownMethod → NotFound → "read-only".
#[derive(Debug, thiserror::Error)]
pub(crate) enum WriteError {
    /// CAS miss / duplicate / FK violation — the operator's evidence no longer holds, or
    /// the write conflicts with existing durable state.
    #[error("conflict: {0}")]
    Conflict(String),
    /// Malformed input (blank/invalid policy, blank name).
    #[error("invalid: {0}")]
    Invalid(String),
    /// Unclassified store failure.
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

impl WriteError {
    /// Reclassifies a raw sqlx error by Postgres SQLSTATE: `23505` (unique_violation) and
    /// `23503` (foreign_key_violation) are DOMAIN conflicts (duplicate name / missing or
    /// still-referenced role), everything else is store trouble. The FK is the real
    /// authority against a create_key↔delete_role race — an `EXISTS` pre-check only buys a
    /// nicer message.
    fn from_db(err: sqlx::Error, conflict_msg: impl Into<String>) -> WriteError {
        if let sqlx::Error::Database(ref db) = err {
            match db.code().as_deref() {
                Some("23505") | Some("23503") => return WriteError::Conflict(conflict_msg.into()),
                _ => {}
            }
        }
        WriteError::Db(err)
    }
}

/// One role as the admin form renders it: its name, current policy, and CAS `revision`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RoleSummary {
    pub name: String,
    pub policy: String,
    pub revision: i64,
}

/// One key as the admin table renders it: NEVER the secret or its hash — only the display
/// `prefix`, the referenced `role`, the CAS `revision`, the `revoked` flag, and a
/// pre-rendered `created_at`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KeySummary {
    pub name: String,
    pub role: String,
    pub prefix: String,
    pub revision: i64,
    pub revoked: bool,
    /// Pre-rendered "Mon DD, HH24:MI" (Postgres `to_char`).
    pub created_at: String,
}

pub(crate) struct Store {
    pub pool: PgPool,
}

impl Store {
    // --- Lookup (the `Keys` capability) -------------------------------------

    /// Resolves a PRESENTED key string to its [`KeyRecord`] by hashing it and JOINing the
    /// referenced role for the effective policy, ignoring revoked keys. `Ok(None)` is a
    /// genuine unknown/revoked key; an `Err` is a store failure. The `policy` returned is
    /// the ROLE's current policy — a role-policy edit propagates to every key at once.
    pub async fn lookup(&self, presented: &str) -> Result<Option<KeyRecord>, sqlx::Error> {
        let hash = secret_hash(presented);
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT k.name, r.policy \
               FROM apikeys.keys k \
               JOIN apikeys.roles r ON r.name = k.role \
              WHERE k.secret_hash = $1 AND k.revoked_at IS NULL",
        )
        .bind(&hash)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(name, policy)| KeyRecord { name, policy }))
    }

    // --- Roles --------------------------------------------------------------

    /// Every role, name-ordered, for the admin form's Selects + policy editor.
    pub async fn list_roles(&self) -> Result<Vec<RoleSummary>, sqlx::Error> {
        let rows: Vec<(String, String, i64)> = sqlx::query_as(
            "SELECT name, policy, revision FROM apikeys.roles ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(name, policy, revision)| RoleSummary { name, policy, revision })
            .collect())
    }

    /// Creates a role. A duplicate name (PK) is a [`WriteError::Conflict`].
    pub async fn create_role(&self, name: &str, policy: &str) -> Result<(), WriteError> {
        validate_name("role", name)?;
        validate_policy(policy)?;
        sqlx::query("INSERT INTO apikeys.roles (name, policy) VALUES ($1, $2)")
            .bind(name)
            .bind(policy)
            .execute(&self.pool)
            .await
            .map_err(|e| WriteError::from_db(e, format!("apikeys: role {name:?} already exists")))?;
        Ok(())
    }

    /// CAS-updates a role's policy: touches the row only when its `revision` still matches
    /// `expected_revision`, bumping `revision`. A miss (stale form OR missing role) is a
    /// [`WriteError::Conflict`] — never a not-found. The change is seen by every key that
    /// references the role on its next lookup (JOIN).
    pub async fn set_role_policy(
        &self,
        name: &str,
        expected_revision: i64,
        policy: &str,
    ) -> Result<(), WriteError> {
        validate_policy(policy)?;
        let affected = sqlx::query(
            "UPDATE apikeys.roles \
                SET policy = $1, revision = revision + 1, updated_at = now() \
              WHERE name = $2 AND revision = $3",
        )
        .bind(policy)
        .bind(name)
        .bind(expected_revision)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if affected != 1 {
            return Err(WriteError::Conflict(format!(
                "apikeys: role {name:?} was changed or removed since the form was rendered"
            )));
        }
        Ok(())
    }

    /// CAS-deletes a role. A key still referencing it raises the FK (`23503`) → a
    /// [`WriteError::Conflict`] ("role in use") — the FK, not the pre-check, is the
    /// authority against a create_key↔delete_role race. A stale/absent revision is also a
    /// conflict.
    pub async fn delete_role(&self, name: &str, expected_revision: i64) -> Result<(), WriteError> {
        let affected = sqlx::query("DELETE FROM apikeys.roles WHERE name = $1 AND revision = $2")
            .bind(name)
            .bind(expected_revision)
            .execute(&self.pool)
            .await
            .map_err(|e| {
                WriteError::from_db(e, format!("apikeys: role {name:?} is still referenced by a key"))
            })?
            .rows_affected();
        if affected != 1 {
            return Err(WriteError::Conflict(format!(
                "apikeys: role {name:?} was changed or removed since the form was rendered"
            )));
        }
        Ok(())
    }

    // --- Keys ---------------------------------------------------------------

    /// Every key, newest first, for the admin table — WITHOUT the secret or its hash.
    pub async fn list_keys(&self) -> Result<Vec<KeySummary>, sqlx::Error> {
        let rows: Vec<(String, String, String, i64, bool, String)> = sqlx::query_as(
            "SELECT name, role, prefix, revision, (revoked_at IS NOT NULL) AS revoked, \
                    to_char(created_at, 'Mon DD, HH24:MI') \
               FROM apikeys.keys \
              ORDER BY created_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(name, role, prefix, revision, revoked, created_at)| KeySummary {
                name,
                role,
                prefix,
                revision,
                revoked,
                created_at,
            })
            .collect())
    }

    /// Mints a new key under an existing `role` and returns `(secret, prefix)` — the
    /// secret is the ONLY time the plaintext exists outside the caller. A duplicate name
    /// (PK) or a missing `role` (FK `23503`) is a [`WriteError::Conflict`] (finding #2:
    /// never a not-found); a digest collision (unique, astronomically unlikely) likewise.
    pub async fn create_key(&self, name: &str, role: &str) -> Result<(String, String), WriteError> {
        validate_name("key", name)?;
        let (secret, hash, prefix) = generate_secret()?;
        sqlx::query(
            "INSERT INTO apikeys.keys (name, secret_hash, prefix, role) VALUES ($1, $2, $3, $4)",
        )
        .bind(name)
        .bind(&hash)
        .bind(&prefix)
        .bind(role)
        .execute(&self.pool)
        .await
        .map_err(|e| {
            WriteError::from_db(
                e,
                format!("apikeys: key {name:?} already exists or role {role:?} does not exist"),
            )
        })?;
        Ok((secret, prefix))
    }

    /// CAS-repoints a key at a different `role`. A missing target role (FK `23503`) or a
    /// stale/absent revision is a [`WriteError::Conflict`].
    pub async fn set_key_role(
        &self,
        name: &str,
        expected_revision: i64,
        role: &str,
    ) -> Result<(), WriteError> {
        let affected = sqlx::query(
            "UPDATE apikeys.keys \
                SET role = $1, revision = revision + 1, updated_at = now() \
              WHERE name = $2 AND revision = $3",
        )
        .bind(role)
        .bind(name)
        .bind(expected_revision)
        .execute(&self.pool)
        .await
        .map_err(|e| WriteError::from_db(e, format!("apikeys: role {role:?} does not exist")))?
        .rows_affected();
        if affected != 1 {
            return Err(WriteError::Conflict(format!(
                "apikeys: key {name:?} was changed or removed since the form was rendered"
            )));
        }
        Ok(())
    }

    /// CAS-revokes a key (sets `revoked_at`), after which [`Store::lookup`] returns `None`.
    /// A stale/absent revision is a [`WriteError::Conflict`].
    pub async fn revoke_key(&self, name: &str, expected_revision: i64) -> Result<(), WriteError> {
        let affected = sqlx::query(
            "UPDATE apikeys.keys \
                SET revoked_at = now(), revision = revision + 1, updated_at = now() \
              WHERE name = $1 AND revision = $2 AND revoked_at IS NULL",
        )
        .bind(name)
        .bind(expected_revision)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if affected != 1 {
            return Err(WriteError::Conflict(format!(
                "apikeys: key {name:?} was changed, removed, or already revoked since the form was rendered"
            )));
        }
        Ok(())
    }

    // --- Dev seed (self-healing upsert) -------------------------------------

    /// Upserts a well-known dev ROLE (Decision 7): resets the policy on conflict so a
    /// stray edit on a shared dev DB can't poison the harness. Idempotent. The `revision`
    /// bumps on every upsert so a form rendered before a re-seed correctly CAS-conflicts
    /// instead of clobbering the freshly-seeded row.
    pub async fn upsert_seed_role(&self, name: &str, policy: &str) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO apikeys.roles (name, policy) VALUES ($1, $2) \
             ON CONFLICT (name) DO UPDATE \
               SET policy = EXCLUDED.policy, revision = apikeys.roles.revision + 1, updated_at = now()",
        )
        .bind(name)
        .bind(policy)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Upserts a well-known dev KEY (Decision 7) with a KNOWN plaintext `secret` so
    /// `X-Api-Key: <secret>` keeps resolving: stores `sha256(secret)` + prefix + role, and
    /// on conflict resets the hash/prefix/role, clears any stray `revoked_at`, and bumps
    /// `revision` (so a form rendered before a re-seed CAS-conflicts). Must be called AFTER
    /// its role is seeded (FK order). Self-healing, idempotent.
    pub async fn upsert_seed_key(
        &self,
        name: &str,
        secret: &str,
        role: &str,
    ) -> Result<(), sqlx::Error> {
        let hash = secret_hash(secret);
        let prefix: String = secret.chars().take(PREFIX_CHARS).collect();
        sqlx::query(
            "INSERT INTO apikeys.keys (name, secret_hash, prefix, role) VALUES ($1, $2, $3, $4) \
             ON CONFLICT (name) DO UPDATE \
               SET secret_hash = EXCLUDED.secret_hash, prefix = EXCLUDED.prefix, \
                   role = EXCLUDED.role, revoked_at = NULL, \
                   revision = apikeys.keys.revision + 1, updated_at = now()",
        )
        .bind(name)
        .bind(&hash)
        .bind(&prefix)
        .bind(role)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
