//! Boots accounts' migrate over an `accounts.oidc_codes` of the shape before `session_digest`
//! and expects the DROP SCHEMA refusal, not the raw error of the DDL step that indexes the
//! column. Its own test binary: it swaps the shared `accounts.oidc_codes` for the old shape,
//! which would break the lib tests using the table, and cargo runs test binaries one at a time.

use accounts::Accounts;
use lifecycle::{Context, Module};
use sqlx::PgPool;

/// `accounts.oidc_codes` as it was before `session_digest`.
const OLD_CODES: &str = "CREATE TABLE accounts.oidc_codes (
	code_digest    bytea PRIMARY KEY,
	client_id      text        NOT NULL REFERENCES accounts.oidc_clients(client_id) ON DELETE CASCADE,
	player_id      uuid        NOT NULL REFERENCES accounts.players(id) ON DELETE CASCADE,
	redirect_uri   text        NOT NULL,
	code_challenge text        NOT NULL,
	scope          text        NOT NULL,
	nonce          text,
	auth_time      timestamptz NOT NULL,
	created_at     timestamptz NOT NULL DEFAULT now(),
	redeemed_at    timestamptz,
	family_id      uuid
)";

/// One accounts boot: register, then migrate.
async fn boot(pool: &PgPool) -> anyhow::Result<()> {
    let ctx = Context::with_db(pool.clone());
    let accounts = Accounts::new();
    accounts.register(&ctx)?;
    accounts.migrate(&ctx).await
}

#[tokio::test(flavor = "multi_thread")]
async fn migrate_refuses_a_codes_table_without_a_session_digest() {
    let Some(pool) = testdb::test_pool().await else { return };
    std::env::set_var("ACCOUNTS_SIGNING_KEK", testdb::DEV_ACCOUNTS_SIGNING_KEK);
    boot(&pool).await.unwrap();
    for sql in ["DROP TABLE accounts.oidc_codes", OLD_CODES] {
        sqlx::query(sql).execute(&pool).await.unwrap();
    }
    let refused = boot(&pool).await;
    sqlx::query("DROP TABLE accounts.oidc_codes").execute(&pool).await.unwrap();
    boot(&pool).await.unwrap();

    let err = format!("{:#}", refused.expect_err("an old codes table must refuse boot"));
    assert!(err.contains("accounts.oidc_codes has no session_digest column"), "{err}");
    assert!(err.contains("DROP SCHEMA accounts CASCADE;"), "{err}");
}
