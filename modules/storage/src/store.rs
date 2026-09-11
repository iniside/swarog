use sqlx::PgPool;

/// Plain `uuid` owner column, no cross-module FK (constraint #10): the relation to
/// `accounts` is a bare id.
///
/// `storage_value_len_check` measures NORMALIZED jsonb text — whitespace dropped, keys
/// reordered, escapes and numbers rewritten — while `storageapi::MAX_VALUE_BYTES` measures
/// the caller's input string. They are different byte strings, so this CHECK is a BACKSTOP
/// at a deliberately different number, never a twin of the Rust cap, and it must not be
/// paired with one.
///
/// `collection` and `key` sit in the primary key, a btree, so an uncapped value raises
/// `54000` — an unmappable 500 where the contract promises 400. Their named CHECKs pair
/// with `storageapi::MAX_COLLECTION_BYTES`/`MAX_KEY_BYTES`, which the service applies
/// first.
const SCHEMA_DDL: &str = r#"
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

pub(crate) struct Store {
    pub(crate) pool: PgPool,
}

impl Store {
    pub(crate) async fn migrate(&self) -> anyhow::Result<()> {
        sqlx::raw_sql(SCHEMA_DDL).execute(&self.pool).await?;
        Ok(())
    }
}
