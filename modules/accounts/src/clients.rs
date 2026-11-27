//! The OIDC clients in `accounts.oidc_clients`: the programs tokens are issued to. The admin
//! portal creates and deletes them; `ACCOUNTS_DEV_SEED` (explicit-only, default OFF) inserts
//! the well-known [`accountsapi::DEV_GAME_CLIENT_ID`] on a dev or proof boot.

use std::ffi::OsString;

use accountsapi::grant_types::{AUTHORIZATION_CODE, GUEST, PASSWORD, REFRESH_TOKEN};
use accountsapi::{grant_types, scopes, DEV_GAME_CLIENT_ID, SWAROG_AUDIENCE};
use base64::Engine as _;
use rand::RngCore as _;
use sqlx::PgConnection;

use crate::providers;
use crate::store::{Revocation, Store};

pub(crate) const DEV_SEED_ENV: &str = "ACCOUNTS_DEV_SEED";

/// The longest client display name the portal accepts.
pub(crate) const MAX_CLIENT_NAME_BYTES: usize = 128;
/// The portal's caps on a client's audience, root domains and redirect URIs.
pub(crate) const MAX_AUDIENCE_BYTES: usize = 128;
pub(crate) const MAX_ROOT_DOMAINS: usize = 8;
pub(crate) const MAX_ROOT_DOMAIN_BYTES: usize = 253;
pub(crate) const MAX_REDIRECT_URIS: usize = 10;
pub(crate) const MAX_REDIRECT_URI_BYTES: usize = 512;

/// The grants the dev seed binds and the portal's create form pre-picks.
pub(crate) const DEFAULT_GRANTS: [&str; 3] = [PASSWORD, GUEST, REFRESH_TOKEN];

/// The only grants a confidential client may hold.
pub(crate) const CONFIDENTIAL_GRANTS: [&str; 2] = [AUTHORIZATION_CODE, REFRESH_TOKEN];

/// The bounds of a client's device code lifetime, and the lifetime a client is created with.
pub(crate) const MIN_DEVICE_CODE_TTL_SECS: i32 = 2 * 60;
pub(crate) const MAX_DEVICE_CODE_TTL_SECS: i32 = 30 * 60;
pub(crate) const DEFAULT_DEVICE_CODE_TTL_SECS: i32 = 10 * 60;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClientRow {
    pub(crate) client_id: String,
    pub(crate) display_name: String,
    pub(crate) grant_types: Vec<String>,
    pub(crate) audience: String,
    pub(crate) redirect_uris: Vec<String>,
    pub(crate) root_domains: Vec<String>,
    pub(crate) scopes: Vec<String>,
    /// The client stores a secret digest.
    pub(crate) confidential: bool,
    pub(crate) device_code_ttl_secs: i32,
    pub(crate) created: String,
}

type ClientTuple =
    (String, String, Vec<String>, String, Vec<String>, Vec<String>, Vec<String>, bool, i32, String);

fn client_row(row: ClientTuple) -> ClientRow {
    let (
        client_id,
        display_name,
        grant_types,
        audience,
        redirect_uris,
        root_domains,
        scopes,
        confidential,
        device_code_ttl_secs,
        created,
    ) = row;
    ClientRow {
        client_id,
        display_name,
        grant_types,
        audience,
        redirect_uris,
        root_domains,
        scopes,
        confidential,
        device_code_ttl_secs,
        created,
    }
}

const CLIENT_COLUMNS: &str = "client_id, display_name, grant_types, audience, redirect_uris, root_domains, \
     scopes, secret_digest IS NOT NULL, device_code_ttl_secs, \
     to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI')";

/// A client the portal creates; `secret_digest` is `None` for a public client.
pub(crate) struct NewClient<'a> {
    pub(crate) display_name: &'a str,
    pub(crate) grants: &'a [String],
    pub(crate) audience: &'a str,
    pub(crate) redirect_uris: &'a [String],
    pub(crate) root_domains: &'a [String],
    pub(crate) scopes: &'a [String],
    pub(crate) secret_digest: Option<&'a str>,
    pub(crate) device_code_ttl_secs: i32,
}

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
    canonical(&grant_types::ALL, picked)
}

/// `scopes::ALL` filtered to the picked names, in its order.
pub(crate) fn canonical_scopes(picked: &[String]) -> Vec<String> {
    canonical(&scopes::ALL, picked)
}

fn canonical(all: &[&str], picked: &[String]) -> Vec<String> {
    all.iter().filter(|v| picked.iter().any(|p| p == *v)).map(|v| (*v).to_string()).collect()
}

/// The non-blank lines of `raw`, trimmed, each kept once in first-seen order.
pub(crate) fn listed_lines(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in raw.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if !out.iter().any(|o| o == line) {
            out.push(line.to_string());
        }
    }
    out
}

/// Why `uri` cannot be a redirect URI: it must be absolute `https`, or `http` on a loopback
/// host, with no fragment and within [`MAX_REDIRECT_URI_BYTES`].
pub(crate) fn redirect_uri_refusal(uri: &str) -> Option<String> {
    if uri.len() > MAX_REDIRECT_URI_BYTES {
        return Some(format!("{uri:?} is over {MAX_REDIRECT_URI_BYTES} bytes"));
    }
    let Ok(parsed) = url::Url::parse(uri) else {
        return Some(format!("{uri:?} is not an absolute URL"));
    };
    if parsed.fragment().is_some() {
        return Some(format!("{uri:?} has a fragment"));
    }
    let loopback = match parsed.host() {
        Some(url::Host::Domain(d)) => d == "localhost",
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => return Some(format!("{uri:?} has no host")),
    };
    match parsed.scheme() {
        "https" => None,
        "http" if loopback => None,
        _ => Some(format!("{uri:?} is neither https nor http on a loopback host")),
    }
}

/// Why `domain` cannot be a root domain: it must be a lowercase DNS name within
/// [`MAX_ROOT_DOMAIN_BYTES`], each label 1-63 bytes of `a-z`, `0-9` and inner `-`.
pub(crate) fn root_domain_refusal(domain: &str) -> Option<String> {
    if domain.len() > MAX_ROOT_DOMAIN_BYTES {
        return Some(format!("{domain:?} is over {MAX_ROOT_DOMAIN_BYTES} bytes"));
    }
    let label_ok = |l: &str| {
        (1..=63).contains(&l.len())
            && l.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !l.starts_with('-')
            && !l.ends_with('-')
    };
    if domain.split('.').all(label_ok) {
        None
    } else {
        Some(format!("{domain:?} is not a lowercase DNS name"))
    }
}

/// Inserts the dev client when absent; an existing row, operator edits included, is kept.
pub(crate) async fn seed_dev_client<'e, E: sqlx::PgExecutor<'e>>(db: E) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO accounts.oidc_clients (client_id, display_name, grant_types, audience) \
         VALUES ($1, $2, $3, $4) ON CONFLICT (client_id) DO NOTHING",
    )
    .bind(DEV_GAME_CLIENT_ID)
    .bind("Dev game")
    .bind(DEFAULT_GRANTS.as_slice())
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

/// What `/token` authenticates a client against.
pub(crate) struct TokenClient {
    pub(crate) client_id: String,
    pub(crate) grant_types: Vec<String>,
    /// `None` for a public client.
    pub(crate) secret_digest: Option<String>,
    /// The `aud` of the access tokens minted for this client.
    pub(crate) audience: String,
}

/// The client a token family was minted for.
pub(crate) struct FamilyClient {
    pub(crate) client_id: String,
    pub(crate) confidential: bool,
    /// The client lists the grant asked about.
    pub(crate) lists_grant: bool,
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

    pub(crate) async fn token_client(&self, client_id: &str) -> sqlx::Result<Option<TokenClient>> {
        let row: Option<(String, Vec<String>, Option<String>, String)> = sqlx::query_as(
            "SELECT client_id, grant_types, secret_digest, audience FROM accounts.oidc_clients WHERE client_id = $1",
        )
        .bind(client_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(client_id, grant_types, secret_digest, audience)| TokenClient {
            client_id,
            grant_types,
            secret_digest,
            audience,
        }))
    }

    /// A new client under a generated id, which it returns.
    pub(crate) async fn insert_client(&self, new: &NewClient<'_>) -> sqlx::Result<String> {
        let client_id = new_client_id();
        sqlx::query(
            "INSERT INTO accounts.oidc_clients \
               (client_id, display_name, grant_types, audience, redirect_uris, root_domains, scopes, secret_digest, \
                device_code_ttl_secs) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(&client_id)
        .bind(new.display_name)
        .bind(new.grants)
        .bind(new.audience)
        .bind(new.redirect_uris)
        .bind(new.root_domains)
        .bind(new.scopes)
        .bind(new.secret_digest)
        .bind(new.device_code_ttl_secs)
        .execute(&self.pool)
        .await?;
        Ok(client_id)
    }

    /// Sets the device code lifetime of `client_id`; `false` when no such client exists.
    pub(crate) async fn set_device_code_ttl(&self, client_id: &str, secs: i32) -> sqlx::Result<bool> {
        let done = sqlx::query("UPDATE accounts.oidc_clients SET device_code_ttl_secs = $2 WHERE client_id = $1")
            .bind(client_id)
            .bind(secs)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() == 1)
    }

    /// What a mint for `client_id` may issue: `None` unless it names a client listing
    /// `grant` (any client for `None`) that is public or `grant` is `authorization_code`,
    /// else whether that client also lists `refresh_token`, read in the same statement. Takes
    /// the row FOR KEY SHARE on the caller's tx, so a delete of that client waits for the mint.
    pub(crate) async fn client_allows_tx(
        &self,
        conn: &mut PgConnection,
        client_id: &str,
        grant: Option<&str>,
    ) -> sqlx::Result<Option<MintGrants>> {
        let row: Option<(bool,)> = sqlx::query_as(
            "SELECT $3 = ANY(grant_types) FROM accounts.oidc_clients \
              WHERE client_id = $1 AND ($2::text IS NULL OR $2 = ANY(grant_types)) \
                AND (secret_digest IS NULL OR $2 = $4) \
                FOR KEY SHARE",
        )
        .bind(client_id)
        .bind(grant)
        .bind(REFRESH_TOKEN)
        .bind(AUTHORIZATION_CODE)
        .fetch_optional(&mut *conn)
        .await?;
        Ok(row.map(|(refresh_token,)| MintGrants { refresh_token }))
    }

    /// The client of family `family_id`, and whether it lists `grant`; `None` for no family.
    pub(crate) async fn family_client_tx(
        &self,
        conn: &mut PgConnection,
        family_id: &str,
        grant: &str,
    ) -> sqlx::Result<Option<FamilyClient>> {
        let row: Option<(String, bool, bool)> = sqlx::query_as(
            "SELECT c.client_id, c.secret_digest IS NOT NULL, $2 = ANY(c.grant_types) \
               FROM accounts.token_families f \
               JOIN accounts.oidc_clients c ON c.client_id = f.client_id \
              WHERE f.id = $1::uuid",
        )
        .bind(family_id)
        .bind(grant)
        .fetch_optional(&mut *conn)
        .await?;
        Ok(row.map(|(client_id, confidential, lists_grant)| FamilyClient { client_id, confidential, lists_grant }))
    }

    /// Whether a row was deleted. Lock order: the client row first, so no login adds a
    /// family under it after the next lock, then its families in ascending id (the order of
    /// [`Store::kill_player_families_tx`]), then one client revocation and the delete, whose
    /// cascade takes the families.
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
        self.insert_revocation_tx(&mut tx, Revocation::Client(client_id)).await?;
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
