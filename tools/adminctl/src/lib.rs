//! `adminctl` operator logic over the shared `admin` schema. The mutating verbs live
//! here (not in `main.rs`) so they are unit-testable against live Postgres; `main.rs`
//! is a thin arg dispatcher (the `tools/` style — no clap). The users-table shape and
//! the argon2id hashing parameters come from the `admin` module crate ([`admin::USERS_DDL`],
//! [`admin::hash_password`], [`admin::verify_password`]) — one source of truth, so the
//! installer and the login path can never drift.

use anyhow::{anyhow, Context, Result};
use sqlx::{PgPool, Row};

/// Dev-default DSN (mirrors CLAUDE.md); overridden by `DATABASE_URL`.
pub const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

/// The connection string every command uses: `DATABASE_URL` or the dev default.
pub fn dsn() -> String {
    std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string())
}

/// One `admin.users` row for the `list` table.
pub struct UserRow {
    pub username: String,
    pub created_at: String,
}

/// Ensures schema `admin` and the `admin.users` table exist. Runs the SAME
/// `admin::USERS_DDL` the module applies in `migrate` — the CLI creates admin users on
/// a fresh database (the installer precondition), so it must be self-sufficient without
/// having booted the module. Both statements are idempotent (`IF NOT EXISTS`).
pub async fn ensure_schema(pool: &PgPool) -> Result<()> {
    sqlx::raw_sql("CREATE SCHEMA IF NOT EXISTS admin;")
        .execute(pool)
        .await
        .context("adminctl: create schema admin")?;
    sqlx::raw_sql(admin::USERS_DDL)
        .execute(pool)
        .await
        .context("adminctl: create table admin.users")?;
    Ok(())
}

/// `create-user`: upsert one admin login. Ensures the schema/table, argon2id-hashes
/// `password` with the module's own [`admin::hash_password`], then
/// `INSERT … ON CONFLICT (username) DO UPDATE` — so a repeat call for an existing
/// username is a password RESET, not an error. Returns `true` when a new row was
/// inserted, `false` when an existing user's password was reset. Rejects an empty
/// password (an unusable login).
///
/// The username is normalized through [`admin::normalize_username`] — the SAME
/// authority the login handler trims/caps against — before it is bound. Binding the
/// raw argv value (the pre-fix behavior) could mint a row like `"  alice  "` that
/// `login_submit`'s trim+cap would then never match, i.e. a permanently unusable
/// ("zombie") account; `install.sh`/`install.ps1` pass argv straight through, so
/// this is the only enforcement point. Legacy rows minted by the pre-normalization
/// adminctl (padded/over-cap usernames) are unreachable by both login and normalized
/// delete: find them with `SELECT username FROM admin.users WHERE username <>
/// btrim(username)` and remove via psql or a schema wipe + reseed (wipe-over-migrations,
/// no data-migration machinery).
pub async fn create_user(pool: &PgPool, username: &str, password: &str) -> Result<bool> {
    let username = admin::normalize_username(username)
        .map_err(|error| anyhow!("adminctl: {error}"))?;
    if password.is_empty() {
        return Err(anyhow!("adminctl: password must not be empty"));
    }
    ensure_schema(pool).await?;
    let hash = admin::hash_password(password).context("adminctl: hash password")?;
    // `xmax = 0` on the returned row iff this was an INSERT (no prior tuple version);
    // a DO UPDATE leaves the pre-existing tuple's xmax non-zero. Lets us report
    // "created" vs "password reset" without a separate existence query.
    let row = sqlx::query(
        "INSERT INTO admin.users (username, pass_hash) VALUES ($1, $2) \
         ON CONFLICT (username) DO UPDATE SET pass_hash = EXCLUDED.pass_hash \
         RETURNING (xmax = 0) AS inserted",
    )
    .bind(&username)
    .bind(&hash)
    .fetch_one(pool)
    .await
    .context("adminctl: upsert admin user")?;
    Ok(row.get::<bool, _>("inserted"))
}

/// `list`: every admin user with its creation time, ordered by username.
pub async fn list_users(pool: &PgPool) -> Result<Vec<UserRow>> {
    ensure_schema(pool).await?;
    let rows = sqlx::query(
        "SELECT username, created_at::text AS created_at \
         FROM admin.users ORDER BY username",
    )
    .fetch_all(pool)
    .await
    .context("adminctl: query admin users")?;
    Ok(rows
        .into_iter()
        .map(|r| UserRow {
            username: r.get("username"),
            created_at: r.get("created_at"),
        })
        .collect())
}

/// `delete`: remove one admin login by username. Returns `true` iff a row was deleted
/// (`false` = no such user), so the CLI can report the difference rather than pretend
/// success. Sessions cascade off `admin.users` (FK `ON DELETE CASCADE`). Normalizes
/// through the same [`admin::normalize_username`] authority as `create_user` so
/// `delete-user "  alice  "` deletes the row `create-user "  alice  "` actually
/// stored (both bind the trimmed name).
pub async fn delete_user(pool: &PgPool, username: &str) -> Result<bool> {
    let username = admin::normalize_username(username)
        .map_err(|error| anyhow!("adminctl: {error}"))?;
    ensure_schema(pool).await?;
    let done = sqlx::query("DELETE FROM admin.users WHERE username = $1")
        .bind(&username)
        .execute(pool)
        .await
        .context("adminctl: delete admin user")?;
    Ok(done.rows_affected() > 0)
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod lib_tests;
