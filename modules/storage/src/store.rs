use opsapi::Error;
use sqlx::{PgConnection, PgPool};
use storageapi::{MAX_COLLECTION_BYTES, MAX_KEY_BYTES};

/// The DDL below writes [`MAX_COLLECTION_BYTES`]/[`MAX_KEY_BYTES`] a second time, in SQL,
/// and `write_error` words a `23514` with the RUST cap: a const raised past its CHECK would
/// answer an over-cap input with a limit it never had.
const _: () = assert!(MAX_COLLECTION_BYTES == 64 && MAX_KEY_BYTES == 128);

/// Plain `uuid` owner column, no cross-module FK (constraint #10): the relation to
/// `accounts` is a bare id.
///
/// `storage_value_len_check` measures NORMALIZED jsonb text — whitespace dropped, keys
/// reordered, escapes and numbers rewritten — while `storageapi::MAX_VALUE_BYTES` measures
/// the caller's input string. They are different byte strings, so this CHECK is a BACKSTOP
/// at a deliberately different number, never a twin of the Rust cap, and it is deliberately
/// absent from [`COLUMN_CAPS`].
///
/// `collection` and `key` sit in the primary key, a btree, so an uncapped value raises
/// `54000` — an unmappable 500 where the contract promises 400. Their CHECKs pair with
/// [`COLLECTION_CAP`]/[`KEY_CAP`], which word the Rust and the DB verdict identically.
pub(crate) const SCHEMA_DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS storage;
CREATE TABLE IF NOT EXISTS storage.objects (
    owner_id   uuid        NOT NULL,
    collection text        NOT NULL,
    key        text        NOT NULL,
    value      jsonb       NOT NULL,
    version    bigint      NOT NULL DEFAULT 1,
    acl        text        NOT NULL DEFAULT 'private',
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (owner_id, collection, key),
    CONSTRAINT storage_collection_len_check CHECK (octet_length(collection) <= 64),
    CONSTRAINT storage_key_len_check        CHECK (octet_length(key)        <= 128),
    CONSTRAINT storage_value_len_check      CHECK (octet_length(value::text) <= 131072),
    CONSTRAINT storage_acl_check            CHECK (acl IN ('private','public_read'))
);
CREATE INDEX IF NOT EXISTS storage_list_idx
    ON storage.objects (owner_id, collection, created_at DESC, key DESC);"#;

/// One caller-authored text column: the caller-facing name, the byte ceiling checked in
/// Rust before the statement, and the named column CHECK that backstops it. ONE table
/// feeds both the pre-check and [`write_error`]'s 23514 mapping, so the Rust verdict and
/// the DB's cannot word one limit two ways.
pub(crate) struct ColumnCap {
    pub(crate) what: &'static str,
    pub(crate) max_bytes: usize,
    pub(crate) constraint: &'static str,
}

pub(crate) const COLLECTION_CAP: ColumnCap = ColumnCap {
    what: "collection",
    max_bytes: MAX_COLLECTION_BYTES,
    constraint: "storage_collection_len_check",
};
pub(crate) const KEY_CAP: ColumnCap = ColumnCap {
    what: "key",
    max_bytes: MAX_KEY_BYTES,
    constraint: "storage_key_len_check",
};

/// `storage_value_len_check` is deliberately NOT here: it counts a different byte string
/// (normalized jsonb) than `storageapi::MAX_VALUE_BYTES` does, so pairing the two numbers
/// would state a limit the caller's input never had.
pub(crate) const COLUMN_CAPS: &[&ColumnCap] = &[&COLLECTION_CAP, &KEY_CAP];

/// Reachable with a `value` UNDER `storageapi::MAX_VALUE_BYTES`, because jsonb
/// normalization can expand what the caller sent (a large exponent literal, for one), so
/// it is a caller-input verdict (400) and never store trouble.
const VALUE_LEN_CHECK: &str = "storage_value_len_check";

impl ColumnCap {
    /// The one over-cap verdict, worded identically whether Rust caught it or the column
    /// CHECK did.
    pub(crate) fn rejected(&self) -> Error {
        Error::invalid(format!("{} exceeds {} bytes", self.what, self.max_bytes))
    }

    pub(crate) fn check(&self, value: &str) -> Result<(), Error> {
        if value.len() > self.max_bytes {
            return Err(self.rejected());
        }
        Ok(())
    }

    fn by_constraint(name: &str) -> Option<&'static ColumnCap> {
        COLUMN_CAPS.iter().copied().find(|cap| cap.constraint == name)
    }
}

/// The ONE `sqlx::Error` verdict for the write path. An unnamed failure is store trouble;
/// a named CHECK that guards caller input is the caller's 400. `storage_acl_check` is
/// absent on purpose: `service::validate_acl` decides the same closed set ahead of every
/// statement, so the CHECK firing means the two disagree, which is a bug, not input.
pub(crate) fn write_error(err: sqlx::Error) -> Error {
    if let sqlx::Error::Database(ref db) = err {
        if db.code().as_deref() == Some("23514") {
            if let Some(cap) = db.constraint().and_then(ColumnCap::by_constraint) {
                return cap.rejected();
            }
            if db.constraint() == Some(VALUE_LEN_CHECK) {
                return Error::invalid("value is too large once normalized as json");
            }
        }
    }
    crate::internal(err)
}

/// Per-OWNER lock key: `MAX_OBJECTS_PER_OWNER` cannot be enforced by one statement, because
/// `INSERT … WHERE (SELECT count(*)) < 10000` is a count-then-insert however it is spelled —
/// under READ COMMITTED two concurrent creates both read `cap - 1` (neither committed yet)
/// and both land past the cap.
///
/// The namespace prefix keeps it from colliding with another module's key space; a
/// collision would only over-serialize. The id is normalized to Postgres's uuid-EQUALITY
/// form before hashing, because the row SQL is `$1::uuid`: a differently-spelled but
/// DB-equal id must yield the SAME lock.
fn owner_lock_key(owner_id: &str) -> i64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let hex: Vec<u8> = owner_id
        .bytes()
        .filter(u8::is_ascii_hexdigit)
        .map(|b| b.to_ascii_lowercase())
        .collect();
    let normalized: &[u8] = if hex.len() == 32 { &hex } else { owner_id.as_bytes() };
    let mut h = OFFSET_BASIS;
    for b in b"storage.owner/".iter().copied().chain(normalized.iter().copied()) {
        h ^= b as u64;
        h = h.wrapping_mul(PRIME);
    }
    h as i64
}

/// What the CAS statement answers, from ONE snapshot of the address.
pub(crate) enum CasOutcome {
    /// The version the write committed at.
    Updated(i64),
    /// The address holds an object whose version is not the one the caller expected.
    Stale,
    /// The address holds nothing the caller owns.
    Missing,
}

/// The write predicate names the OWNER alone. A `public_read` object is readable by
/// anyone who can name it and writable only by its owner, so this string must never gain
/// the read's acl disjunction — sharing it would make `public_read` mean public WRITE.
///
/// One statement BOUNDS the stale-vs-deleted ambiguity to a delete committing during this
/// very write: the `EXISTS` half reads the command snapshot, so a concurrent `DELETE` that
/// commits while the UPDATE waits on its row lock still answers `Stale`, and the caller
/// re-reads into a 404. A two-statement "update, then look" leaves that window open across
/// two independent READ COMMITTED snapshots.
const CAS_SQL: &str = r#"
WITH upd AS (
    UPDATE storage.objects
       SET value = $4::jsonb,
           acl = COALESCE($6::text, acl),
           version = version + 1,
           updated_at = now()
     WHERE owner_id = $1::uuid AND collection = $2 AND key = $3 AND version = $5
    RETURNING version
)
SELECT (SELECT version FROM upd),
       EXISTS (SELECT 1 FROM storage.objects
                WHERE owner_id = $1::uuid AND collection = $2 AND key = $3)"#;

pub(crate) struct Store {
    pub(crate) pool: PgPool,
}

impl Store {
    pub(crate) async fn migrate(&self) -> anyhow::Result<()> {
        sqlx::raw_sql(SCHEMA_DDL).execute(&self.pool).await?;
        Ok(())
    }

    /// Taken ON THE TX CONNECTION — a separate pool connection would lose the
    /// serialization — and released at commit/rollback.
    pub(crate) async fn lock_owner_tx(
        &self,
        conn: &mut PgConnection,
        owner_id: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(owner_lock_key(owner_id))
            .execute(&mut *conn)
            .await?;
        Ok(())
    }

    pub(crate) async fn count_objects_tx(
        &self,
        conn: &mut PgConnection,
        owner_id: &str,
    ) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar("SELECT count(*)::bigint FROM storage.objects WHERE owner_id = $1::uuid")
            .bind(owner_id)
            .fetch_one(&mut *conn)
            .await
    }

    /// `None` when the address is already taken: `ON CONFLICT DO NOTHING` answers the
    /// create-or-409 rule without ever raising a `23505`.
    pub(crate) async fn insert_object_tx(
        &self,
        conn: &mut PgConnection,
        owner_id: &str,
        collection: &str,
        key: &str,
        value: &str,
        acl: &str,
    ) -> Result<Option<i64>, sqlx::Error> {
        sqlx::query_scalar(
            "INSERT INTO storage.objects (owner_id, collection, key, value, acl) \
             VALUES ($1::uuid, $2, $3, $4::jsonb, $5) \
             ON CONFLICT DO NOTHING RETURNING version",
        )
        .bind(owner_id)
        .bind(collection)
        .bind(key)
        .bind(value)
        .bind(acl)
        .fetch_optional(&mut *conn)
        .await
    }

    /// The CAS write. No advisory lock: a loser blocking on the winner's ROW lock
    /// re-applies its own `version = $5` qualifier to the newly committed row and matches
    /// nothing, which is the entire guarantee.
    ///
    /// `acl` is `None` for "leave it unchanged".
    pub(crate) async fn cas_object(
        &self,
        owner_id: &str,
        collection: &str,
        key: &str,
        value: &str,
        expected_version: i64,
        acl: Option<&str>,
    ) -> Result<CasOutcome, sqlx::Error> {
        let (updated, exists) = sqlx::query_as::<_, (Option<i64>, bool)>(CAS_SQL)
            .bind(owner_id)
            .bind(collection)
            .bind(key)
            .bind(value)
            .bind(expected_version)
            .bind(acl)
            .fetch_one(&self.pool)
            .await?;
        Ok(match (updated, exists) {
            (Some(version), _) => CasOutcome::Updated(version),
            (None, true) => CasOutcome::Stale,
            (None, false) => CasOutcome::Missing,
        })
    }
}
