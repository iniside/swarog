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
//! and never persisted, so no key read can expose it. Lookup hashes the presented
//! key and matches the digest — an O(1) indexed read.
//!
//! Optimistic concurrency is CAS-by-`revision` for the edits that replace a value (a
//! role's policy, a key's role): the UPDATE carries the `expected_revision` the operator's
//! form was rendered against and touches the row only when it still matches, bumping
//! `revision` on success. A miss (`rows_affected() == 0`) is a [`WriteError::Conflict`],
//! NEVER a not-found — a stale form and a vanished target are the same "your evidence no
//! longer holds" answer, and surfacing them as NotFound would be indistinguishable from
//! the edge's UnknownMethod (see the module docs).
//!
//! Revoking a key and deleting a role carry no revision (owner decision D1): neither
//! depends on what the operator saw, so each is one guarded statement whose outcome
//! ([`Revoke`], or whether a row was deleted) tells the caller what happened.

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

/// How many times [`Store::delete_role`] runs its DELETE when the FK fires but the count
/// that follows finds no key on the role.
const DELETE_ROLE_ATTEMPTS: usize = 3;

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

/// The upper bound on a role's policy string. `roles.policy` is operator-writable
/// (`create_role`/`set_role_policy`) and rides EVERY gateway key
/// lookup response — `lookup` returns `r.policy`, which the gateway caches for 5s per key
/// — so an unbounded policy would bloat each lookup + the cache. 4 KiB comfortably fits a
/// comma-list of every real wire method (~14 today, ~30 bytes each) with large headroom
/// for future ops and operator pre-authorization; it is purely an anti-bloat ceiling, not
/// a functional limit anyone should hit.
pub const MAX_POLICY_BYTES: usize = 4096;

/// The upper bound on a role or key NAME. Every name is operator-authored — 128 bytes is
/// the same ceiling `accounts` puts on a display name, and no operator-chosen identifier
/// needs more.
pub const MAX_NAME_BYTES: usize = 128;

/// One operator-supplied text column: the operator-facing name, the byte ceiling checked
/// in Rust BEFORE the statement, and the named column CHECK that backstops it. ONE table
/// feeds both the pre-checks and [`WriteError::from_db`]'s 23514 mapping, so the Rust
/// verdict and the DB's cannot word one limit two ways — and adding an operator-writable
/// column cannot cap it in Rust while leaving the CHECK unmapped.
pub(crate) struct ColumnCap {
    pub(crate) what: &'static str,
    pub(crate) max_bytes: usize,
    pub(crate) constraint: &'static str,
}

pub(crate) const ROLE_NAME: ColumnCap = ColumnCap {
    what: "role name",
    max_bytes: MAX_NAME_BYTES,
    constraint: "roles_name_len_check",
};
pub(crate) const KEY_NAME: ColumnCap = ColumnCap {
    what: "key name",
    max_bytes: MAX_NAME_BYTES,
    constraint: "keys_name_len_check",
};
const ROLE_POLICY: ColumnCap = ColumnCap {
    what: "role policy",
    max_bytes: MAX_POLICY_BYTES,
    constraint: "roles_policy_len_check",
};

pub(crate) const COLUMN_CAPS: &[&ColumnCap] = &[&ROLE_NAME, &KEY_NAME, &ROLE_POLICY];

/// The implicit primary-key constraint names [`WriteError::from_db`] reads as a taken name.
const KEYS_PKEY: &str = "keys_pkey";
const ROLES_PKEY: &str = "roles_pkey";

impl ColumnCap {
    /// The one over-cap verdict, worded identically whether Rust caught it or the column
    /// CHECK did. `WriteError::Invalid` — operator input, never store trouble.
    fn rejected(&self) -> WriteError {
        WriteError::Invalid(format!(
            "apikeys: {} exceeds the {}-byte cap",
            self.what, self.max_bytes
        ))
    }

    /// The cap check runs before any other rule so an over-long value is never echoed
    /// back inside an error message.
    fn check(&self, value: &str) -> Result<(), WriteError> {
        if value.len() > self.max_bytes {
            return Err(self.rejected());
        }
        Ok(())
    }

    fn by_constraint(name: &str) -> Option<&'static ColumnCap> {
        COLUMN_CAPS.iter().copied().find(|cap| cap.constraint == name)
    }
}

/// Policy validation (Decision 4): within [`MAX_POLICY_BYTES`] and canonical — exactly
/// `full`, or a comma-separated list of distinct, non-empty entries with no surrounding
/// whitespace and no `full` entry. Canonical because the gateway matcher grants all only
/// for exactly `full` and compares listed entries verbatim, while the admin form folds and
/// trims: a stored policy both read differently would change meaning on an untouched save.
/// A method NAME is still only "non-blank", not checked against a catalog — ops evolve, and
/// an operator may pre-authorize a method no process serves yet. The byte cap is an
/// ADDITIONAL upper bound, not a replacement.
pub(crate) fn validate_policy(policy: &str) -> Result<(), WriteError> {
    ROLE_POLICY.check(policy)?;
    if policy == "full" {
        return Ok(());
    }
    let mut seen = std::collections::BTreeSet::new();
    let canonical = policy
        .split(',')
        .all(|m| !m.trim().is_empty() && m.trim() == m && m != "full" && seen.insert(m));
    if !canonical {
        return Err(WriteError::Invalid(format!(
            "apikeys: invalid policy {policy:?} (must be exactly `full` or a comma-separated list of \
             distinct methods, with no blank or padded entry and no `full` entry)"
        )));
    }
    Ok(())
}

/// A capped, non-empty, trimmed name. Called by EVERY writer that binds a role or key
/// name — as an inserted value, an updated value, or a `WHERE` predicate — so no
/// operator-authored name reaches a statement unbounded.
pub(crate) fn validate_name(cap: &ColumnCap, name: &str) -> Result<(), WriteError> {
    cap.check(name)?;
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(WriteError::Invalid(format!("apikeys: {} must not be blank", cap.what)));
    }
    if trimmed != name {
        return Err(WriteError::Invalid(format!(
            "apikeys: {} {name:?} must not have leading/trailing whitespace",
            cap.what
        )));
    }
    Ok(())
}

/// A write outcome that distinguishes a taken name, a DOMAIN conflict (CAS miss or an FK
/// violation — a referenced role missing, or a role still referenced by a key), a
/// validation error and raw store trouble. This is the authority for finding #2:
/// none of these map to `NotFound` — a domain conflict is a [`opsapi::Status::Conflict`]
/// (409) and never masquerades as the edge's UnknownMethod → NotFound → "read-only".
#[derive(Debug, thiserror::Error)]
pub(crate) enum WriteError {
    /// CAS miss / FK violation — the operator's evidence no longer holds, or the write
    /// conflicts with existing durable state.
    #[error("conflict: {0}")]
    Conflict(String),
    /// The insert's name is already a key's or role's primary key (`23505` on
    /// [`KEYS_PKEY`] or [`ROLES_PKEY`]).
    #[error("taken: a row with this name already exists")]
    Taken,
    /// Malformed input (blank/invalid policy, blank name).
    #[error("invalid: {0}")]
    Invalid(String),
    /// Unclassified store failure.
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

impl WriteError {
    /// Reclassifies a raw sqlx error by Postgres SQLSTATE: `23505` (unique_violation) on a
    /// name primary key is [`WriteError::Taken`]; any other `23505` and `23503`
    /// (foreign_key_violation) are DOMAIN conflicts (digest collision / missing or
    /// still-referenced role); everything else is store trouble. The FK is the real
    /// authority against a create_key↔delete_role race — an `EXISTS` pre-check only buys a
    /// nicer message.
    ///
    /// A `23514` naming one of [`COLUMN_CAPS`]' constraints is the same operator-input
    /// verdict its Rust pre-check gives, so it is `Invalid`, never store trouble. An
    /// UNMAPPED 23514 stays `Db`: a constraint nothing here knows about fired, which is a
    /// defect rather than operator input.
    pub(crate) fn from_db(err: sqlx::Error, conflict_msg: impl Into<String>) -> WriteError {
        if let sqlx::Error::Database(ref db) = err {
            match db.code().as_deref() {
                Some("23514") => {
                    if let Some(cap) = db.constraint().and_then(ColumnCap::by_constraint) {
                        return cap.rejected();
                    }
                }
                Some("23505") if matches!(db.constraint(), Some(KEYS_PKEY | ROLES_PKEY)) => return WriteError::Taken,
                Some("23505") | Some("23503") => return WriteError::Conflict(conflict_msg.into()),
                _ => {}
            }
        }
        WriteError::Db(err)
    }
}

/// One role with the number of keys referencing it, revoked keys included.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RoleRow {
    pub name: String,
    pub policy: String,
    pub revision: i64,
    pub keys: i64,
}

/// One key as the admin table renders it: never the secret or its hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KeyRow {
    pub name: String,
    pub prefix: String,
    pub role: String,
    pub revision: i64,
    pub revoked: bool,
    /// `YYYY-MM-DD` in UTC.
    pub created: String,
    /// `created_at` in microseconds since the epoch, the keyset cursor's first half.
    pub created_us: i64,
}

/// What [`Store::revoke_key`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Revoke {
    /// This call revoked the key.
    Revoked,
    /// The key exists and was revoked before this call; nothing was written.
    AlreadyRevoked,
    /// No key has this name.
    Absent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyStatus {
    Active,
    Revoked,
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

    /// Every role, name-ordered, with its key count (revoked keys included).
    pub async fn roles_with_counts(&self) -> Result<Vec<RoleRow>, sqlx::Error> {
        let rows: Vec<(String, String, i64, i64)> = sqlx::query_as(
            "SELECT r.name, r.policy, r.revision, count(k.name) \
               FROM apikeys.roles r \
               LEFT JOIN apikeys.keys k ON k.role = r.name \
              GROUP BY r.name, r.policy, r.revision \
              ORDER BY r.name",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(name, policy, revision, keys)| RoleRow { name, policy, revision, keys })
            .collect())
    }

    /// One role with its key count (revoked keys included), or `None` when absent.
    pub async fn role(&self, name: &str) -> Result<Option<RoleRow>, sqlx::Error> {
        let row: Option<(String, String, i64, i64)> = sqlx::query_as(
            "SELECT r.name, r.policy, r.revision, \
                    (SELECT count(*) FROM apikeys.keys k WHERE k.role = r.name) \
               FROM apikeys.roles r \
              WHERE r.name = $1",
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(name, policy, revision, keys)| RoleRow { name, policy, revision, keys }))
    }

    /// Creates a role. A duplicate name (PK) is [`WriteError::Taken`].
    pub async fn create_role(&self, name: &str, policy: &str) -> Result<(), WriteError> {
        validate_name(&ROLE_NAME, name)?;
        validate_policy(policy)?;
        sqlx::query("INSERT INTO apikeys.roles (name, policy) VALUES ($1, $2)")
            .bind(name)
            .bind(policy)
            .execute(&self.pool)
            .await
            .map_err(|e| WriteError::from_db(e, format!("apikeys: role {name:?} could not be created")))?;
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
        validate_name(&ROLE_NAME, name)?;
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
        .await
        .map_err(|e| {
            WriteError::from_db(
                e,
                format!("apikeys: role {name:?} was changed or removed since the form was rendered"),
            )
        })?
        .rows_affected();
        if affected != 1 {
            return Err(WriteError::Conflict(format!(
                "apikeys: role {name:?} was changed or removed since the form was rendered"
            )));
        }
        Ok(())
    }

    /// Deletes a role; `Ok(false)` when no role has this name. A key still referencing it,
    /// revoked or not, raises the FK (`23503`) → a [`WriteError::Conflict`] naming how many
    /// keys use it. The FK, not a pre-check, is the authority against a
    /// create_key↔delete_role race; revoked keys keep their role so their audit trail does.
    /// The count is a second statement, so a key moved off the role in between can leave it
    /// at 0: then the DELETE runs again, at most [`DELETE_ROLE_ATTEMPTS`] times in all.
    pub async fn delete_role(&self, name: &str) -> Result<bool, WriteError> {
        validate_name(&ROLE_NAME, name)?;
        for _ in 0..DELETE_ROLE_ATTEMPTS {
            let result = sqlx::query("DELETE FROM apikeys.roles WHERE name = $1")
                .bind(name)
                .execute(&self.pool)
                .await;
            match result {
                Ok(done) => return Ok(done.rows_affected() == 1),
                Err(sqlx::Error::Database(db)) if db.code().as_deref() == Some("23503") => {
                    let keys: i64 =
                        sqlx::query_scalar("SELECT count(*) FROM apikeys.keys WHERE role = $1")
                            .bind(name)
                            .fetch_one(&self.pool)
                            .await?;
                    if keys > 0 {
                        return Err(WriteError::Conflict(format!(
                            "role {name} is still used by {keys} key(s), revoked keys included"
                        )));
                    }
                }
                Err(e) => {
                    return Err(WriteError::from_db(
                        e,
                        format!("apikeys: role {name:?} could not be deleted"),
                    ));
                }
            }
        }
        Err(WriteError::Conflict(format!(
            "role {name} could not be deleted: its keys kept moving to other roles; try again"
        )))
    }

    // --- Keys ---------------------------------------------------------------

    /// One key, or `None` when absent. Never the secret or its hash.
    pub async fn key(&self, name: &str) -> Result<Option<KeyRow>, sqlx::Error> {
        let row: Option<(String, String, String, i64, bool, String, i64)> = sqlx::query_as(
            "SELECT name, prefix, role, revision, (revoked_at IS NOT NULL), \
                    to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD'), \
                    (EXTRACT(EPOCH FROM created_at) * 1000000)::int8 \
               FROM apikeys.keys \
              WHERE name = $1",
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(name, prefix, role, revision, revoked, created, created_us)| KeyRow {
            name,
            prefix,
            role,
            revision,
            revoked,
            created,
            created_us,
        }))
    }

    /// One page of keys, newest first, keyset on `(created_at, name)` DESC: `after` is the
    /// last row's `(created_us, name)`. The needle matches name or prefix as a
    /// case-insensitive substring; `strpos` takes it literally, so `%` and `_` need no
    /// escaping.
    pub async fn keys_page(
        &self,
        needle: Option<&str>,
        status: Option<KeyStatus>,
        role: Option<&str>,
        after: Option<(i64, &str)>,
        limit: i64,
    ) -> Result<Vec<KeyRow>, sqlx::Error> {
        let rows: Vec<(String, String, String, i64, bool, String, i64)> = sqlx::query_as(
            "SELECT name, prefix, role, revision, (revoked_at IS NOT NULL), \
                    to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD'), \
                    (EXTRACT(EPOCH FROM created_at) * 1000000)::int8 \
               FROM apikeys.keys \
              WHERE ($1::text IS NULL \
                     OR strpos(lower(name), lower($1)) > 0 \
                     OR strpos(lower(prefix), lower($1)) > 0) \
                AND ($2::bool IS NULL OR (revoked_at IS NOT NULL) = $2) \
                AND ($3::text IS NULL OR role = $3) \
                AND ($4::int8 IS NULL \
                     OR (created_at, name) < (to_timestamp(0) + $4 * interval '1 microsecond', $5::text)) \
              ORDER BY created_at DESC, name DESC \
              LIMIT $6",
        )
        .bind(needle)
        .bind(status.map(|s| s == KeyStatus::Revoked))
        .bind(role)
        .bind(after.map(|(us, _)| us))
        .bind(after.map(|(_, name)| name))
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(name, prefix, role, revision, revoked, created, created_us)| KeyRow {
                name,
                prefix,
                role,
                revision,
                revoked,
                created,
                created_us,
            })
            .collect())
    }

    /// `(total, active)` over every key. Exact `count(*)`: there is no index on
    /// `created_at` or `role`.
    pub async fn key_counts(&self) -> Result<(i64, i64), sqlx::Error> {
        sqlx::query_as(
            "SELECT count(*), count(*) FILTER (WHERE revoked_at IS NULL) FROM apikeys.keys",
        )
        .fetch_one(&self.pool)
        .await
    }

    /// Mints a new key under an existing `role` and returns `(secret, prefix)` — the
    /// secret is the ONLY time the plaintext exists outside the caller. A duplicate name
    /// (PK) is [`WriteError::Taken`]; a missing `role` (FK `23503`) is a
    /// [`WriteError::Conflict`] (finding #2: never a not-found), as is a digest collision
    /// (unique, astronomically unlikely).
    pub async fn create_key(&self, name: &str, role: &str) -> Result<(String, String), WriteError> {
        validate_name(&KEY_NAME, name)?;
        validate_name(&ROLE_NAME, role)?;
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
        .map_err(|e| WriteError::from_db(e, format!("apikeys: role {role:?} does not exist")))?;
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
        validate_name(&KEY_NAME, name)?;
        validate_name(&ROLE_NAME, role)?;
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

    /// Revokes a key (sets `revoked_at`), after which [`Store::lookup`] returns `None`.
    /// The `revoked_at IS NULL` guard makes a repeat write nothing. The existence read that
    /// tells [`Revoke::AlreadyRevoked`] from [`Revoke::Absent`] is in the same statement as
    /// the UPDATE, so both share one snapshot: a key created in between is never reported
    /// as already revoked.
    pub async fn revoke_key(&self, name: &str) -> Result<Revoke, WriteError> {
        validate_name(&KEY_NAME, name)?;
        let (revoked, exists): (bool, bool) = sqlx::query_as(
            "WITH u AS ( \
                UPDATE apikeys.keys \
                   SET revoked_at = now(), revision = revision + 1, updated_at = now() \
                 WHERE name = $1 AND revoked_at IS NULL \
             RETURNING 1) \
             SELECT EXISTS (SELECT 1 FROM u), \
                    EXISTS (SELECT 1 FROM apikeys.keys WHERE name = $1)",
        )
        .bind(name)
        .fetch_one(&self.pool)
        .await?;
        Ok(match (revoked, exists) {
            (true, _) => Revoke::Revoked,
            (false, true) => Revoke::AlreadyRevoked,
            (false, false) => Revoke::Absent,
        })
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
