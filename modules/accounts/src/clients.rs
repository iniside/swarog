//! The OIDC clients in `accounts.oidc_clients`: the programs tokens are issued to. The admin
//! portal creates and deletes them; `ACCOUNTS_DEV_SEED` (explicit-only, default OFF) inserts
//! the well-known [`accountsapi::DEV_GAME_CLIENT_ID`] on a dev or proof boot.

use std::ffi::OsString;

use accountsapi::{grant_types, DEV_GAME_CLIENT_ID, SWAROG_AUDIENCE};
use base64::Engine as _;
use rand::RngCore as _;
use sqlx::PgConnection;

use crate::providers;
use crate::store::Store;

pub(crate) const DEV_SEED_ENV: &str = "ACCOUNTS_DEV_SEED";

/// The longest client display name the portal accepts.
pub(crate) const MAX_CLIENT_NAME_BYTES: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClientRow {
    pub(crate) client_id: String,
    pub(crate) display_name: String,
    pub(crate) grant_types: Vec<String>,
    pub(crate) audience: String,
    pub(crate) created: String,
}

type ClientTuple = (String, String, Vec<String>, String, String);

fn client_row((client_id, display_name, grant_types, audience, created): ClientTuple) -> ClientRow {
    ClientRow { client_id, display_name, grant_types, audience, created }
}

const CLIENT_COLUMNS: &str = "client_id, display_name, grant_types, audience, \
     to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI')";

/// `true` only for an explicitly truthy value (`1`/`true`/`on`, case-insensitive).
pub(crate) fn dev_seed_from(value: Option<OsString>) -> bool {
    value.as_deref().and_then(|v| v.to_str()).is_some_and(crate::truthy)
}

/// `c_` and 16 random bytes in base64url without padding: 22 characters.
pub(crate) fn new_client_id() -> String {
    let mut b = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut b);
    format!("c_{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b))
}

/// `grant_types::ALL` filtered to the picked names, so a client's grants are stored in one
/// order whatever order they were picked in.
pub(crate) fn canonical_grants(picked: &[String]) -> Vec<String> {
    grant_types::ALL
        .iter()
        .filter(|g| picked.iter().any(|p| p == *g))
        .map(|g| (*g).to_string())
        .collect()
}

/// Inserts the dev client when absent; an existing row, operator edits included, is kept.
pub(crate) async fn seed_dev_client<'e, E: sqlx::PgExecutor<'e>>(db: E) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO accounts.oidc_clients (client_id, display_name, grant_types, audience) \
         VALUES ($1, $2, $3, $4) ON CONFLICT (client_id) DO NOTHING",
    )
    .bind(DEV_GAME_CLIENT_ID)
    .bind("Dev game")
    .bind(grant_types::ALL.as_slice())
    .bind(SWAROG_AUDIENCE)
    .execute(db)
    .await?;
    Ok(())
}

/// The seed `migrate` runs: the dev client when `on` (`ACCOUNTS_DEV_SEED` read at init),
/// nothing otherwise.
pub(crate) async fn migrate_dev_seed<'e, E: sqlx::PgExecutor<'e>>(on: bool, db: E) -> sqlx::Result<()> {
    if on {
        seed_dev_client(db).await?;
    }
    Ok(())
}

/// The grant a mint through identity `provider` needs its client to list; `None` when the
/// client only has to exist, since the provider itself proves who the user is.
pub(crate) fn grant_for_provider(provider: &str) -> Option<&'static str> {
    match provider {
        providers::PASSWORD => Some(grant_types::PASSWORD),
        providers::GUEST => Some(grant_types::GUEST),
        _ => None,
    }
}

/// What [`Store::client_allows_tx`] found a mint may issue beyond its access token.
pub(crate) struct MintGrants {
    /// The client lists `refresh_token`, so the mint also issues a refresh token.
    pub(crate) refresh_token: bool,
}

impl Store {
    /// Every client, newest first.
    pub(crate) async fn clients(&self) -> sqlx::Result<Vec<ClientRow>> {
        let rows: Vec<ClientTuple> = sqlx::query_as(&format!(
            "SELECT {CLIENT_COLUMNS} FROM accounts.oidc_clients ORDER BY created_at DESC, client_id"
        ))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(client_row).collect())
    }

    pub(crate) async fn client(&self, client_id: &str) -> sqlx::Result<Option<ClientRow>> {
        let row: Option<ClientTuple> = sqlx::query_as(&format!(
            "SELECT {CLIENT_COLUMNS} FROM accounts.oidc_clients WHERE client_id = $1"
        ))
        .bind(client_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(client_row))
    }

    /// A new client for the Swarog audience under a generated id, which it returns.
    pub(crate) async fn insert_client(&self, display_name: &str, grants: &[String]) -> sqlx::Result<String> {
        let client_id = new_client_id();
        sqlx::query(
            "INSERT INTO accounts.oidc_clients (client_id, display_name, grant_types, audience) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(&client_id)
        .bind(display_name)
        .bind(grants)
        .bind(SWAROG_AUDIENCE)
        .execute(&self.pool)
        .await?;
        Ok(client_id)
    }

    /// What a mint for `client_id` may issue: `None` unless it names a client listing
    /// `grant` (any client for `None`), else whether that client also lists `refresh_token`,
    /// read in the same statement. Takes the row FOR KEY SHARE on the caller's tx, so a
    /// delete of that client waits for the mint.
    pub(crate) async fn client_allows_tx(
        &self,
        conn: &mut PgConnection,
        client_id: &str,
        grant: Option<&str>,
    ) -> sqlx::Result<Option<MintGrants>> {
        let row: Option<(bool,)> = sqlx::query_as(
            "SELECT $3 = ANY(grant_types) FROM accounts.oidc_clients \
              WHERE client_id = $1 AND ($2::text IS NULL OR $2 = ANY(grant_types)) \
                FOR KEY SHARE",
        )
        .bind(client_id)
        .bind(grant)
        .bind(grant_types::REFRESH_TOKEN)
        .fetch_optional(&mut *conn)
        .await?;
        Ok(row.map(|(refresh_token,)| MintGrants { refresh_token }))
    }

    /// Whether the client of family `family_id` lists `grant`.
    pub(crate) async fn family_client_allows_tx(
        &self,
        conn: &mut PgConnection,
        family_id: &str,
        grant: &str,
    ) -> sqlx::Result<bool> {
        let row: Option<(i32,)> = sqlx::query_as(
            "SELECT 1 FROM accounts.token_families f \
               JOIN accounts.oidc_clients c ON c.client_id = f.client_id \
              WHERE f.id = $1::uuid AND $2 = ANY(c.grant_types)",
        )
        .bind(family_id)
        .bind(grant)
        .fetch_optional(&mut *conn)
        .await?;
        Ok(row.is_some())
    }

    /// Whether a row was deleted. Lock order: the client row first, so no login adds a
    /// family under it after the next lock, then its families in ascending id (the order of
    /// [`Store::kill_player_families_tx`]), then the delete, whose cascade takes them.
    pub(crate) async fn delete_client(&self, client_id: &str) -> sqlx::Result<bool> {
        let mut tx = self.pool.begin().await?;
        let found: Option<(i32,)> =
            sqlx::query_as("SELECT 1 FROM accounts.oidc_clients WHERE client_id = $1 FOR UPDATE")
                .bind(client_id)
                .fetch_optional(&mut *tx)
                .await?;
        if found.is_none() {
            return Ok(false);
        }
        sqlx::query("SELECT 1 FROM accounts.token_families WHERE client_id = $1 ORDER BY id FOR UPDATE")
            .bind(client_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM accounts.oidc_clients WHERE client_id = $1")
            .bind(client_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }
}

#[cfg(test)]
#[path = "clients_tests.rs"]
mod tests;
