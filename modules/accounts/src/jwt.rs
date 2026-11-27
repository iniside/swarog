//! Player access tokens: ES256 JWTs signed with the active key of `signing_keys`, and
//! their verification against the live keys and the unexpired revocations.

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use accountsapi::{
    Revoked, TokenStateSnapshot, ACCESS_EXP_LEEWAY, ACCESS_TOKEN_TYP, ACCESS_TTL_MINUTES,
    MAX_ACCESS_TOKEN_BYTES, MAX_CLIENT_ID_BYTES, MAX_HANDLE_BYTES, SWAROG_AUDIENCE,
};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool};

use crate::signing_keys::{self, Clock, SigningKek, SigningKeys, KEK_ENV};
use crate::store::MintedAccess;

/// How long the signer uses the active key it read before reading it again.
pub(crate) const ACTIVE_KEY_CACHE_TTL: Duration = Duration::from_secs(60);

pub(crate) const ACCESS_TTL_SECS: i64 = ACCESS_TTL_MINUTES as i64 * 60;

/// The claims of one access token. `sid` is the token family's id, `aud` its client's
/// audience, `preferred_username` the player's handle; `root_domains` is present only when
/// the client lists some.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AccessClaims {
    pub(crate) iss: String,
    pub(crate) sub: String,
    pub(crate) aud: String,
    pub(crate) client_id: String,
    pub(crate) sid: String,
    pub(crate) iat: i64,
    pub(crate) exp: i64,
    pub(crate) jti: String,
    pub(crate) preferred_username: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) root_domains: Vec<String>,
}

/// The `typ` header of an id_token. Both access verifiers accept only [`ACCESS_TOKEN_TYP`], so
/// an id_token is never taken for an access token.
#[cfg_attr(not(test), expect(dead_code, reason = "the /token endpoint is its only signer"))]
pub(crate) const ID_TOKEN_TYP: &str = "JWT";

/// The claims of one id_token (OIDC Core 1.0 section 2). `aud` is the client's id,
/// `auth_time` when the player logged in, `nonce` the authorization request's when it sent
/// one.
#[cfg_attr(not(test), expect(dead_code, reason = "the /token endpoint is its only signer"))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct IdClaims {
    pub(crate) iss: String,
    pub(crate) sub: String,
    pub(crate) aud: String,
    pub(crate) iat: i64,
    pub(crate) exp: i64,
    pub(crate) auth_time: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) nonce: Option<String>,
    pub(crate) preferred_username: String,
}

/// `(family_id, player_id, client_id, revoked_at_unix)` of one revoked subject.
type RevocationRow = (Option<String>, Option<String>, Option<String>, i64);

struct ActiveKey {
    kid: String,
    key: EncodingKey,
}

/// The issuer's access tokens: the signer, which reads the active key at most once per
/// [`ACTIVE_KEY_CACHE_TTL`], and the verification state over the live keys and revocations.
pub(crate) struct Tokens {
    issuer: String,
    pool: PgPool,
    schema: String,
    kek: Arc<OnceLock<SigningKek>>,
    keys: Arc<SigningKeys>,
    now: Clock,
    active: tokio::sync::Mutex<Option<(Instant, Arc<ActiveKey>)>>,
}

impl Tokens {
    /// `schema` holds `signing_keys`; `kek` is filled by migrate before anything signs.
    pub(crate) fn new(
        pool: PgPool,
        schema: &str,
        issuer: String,
        kek: Arc<OnceLock<SigningKek>>,
    ) -> Tokens {
        Tokens::with_clock(pool, schema, issuer, kek, Arc::new(Instant::now))
    }

    pub(crate) fn with_clock(
        pool: PgPool,
        schema: &str,
        issuer: String,
        kek: Arc<OnceLock<SigningKek>>,
        now: Clock,
    ) -> Tokens {
        Tokens {
            keys: Arc::new(SigningKeys::with_clock(pool.clone(), schema, now.clone())),
            issuer,
            pool,
            schema: schema.to_string(),
            kek,
            now,
            active: tokio::sync::Mutex::new(None),
        }
    }

    /// The live public keys, shared with the JWKS route so both read through one cache.
    pub(crate) fn keys(&self) -> Arc<SigningKeys> {
        self.keys.clone()
    }

    /// The claims of the token `minted` stamped in family `sid` for player `sub`.
    pub(crate) fn claims(&self, sub: &str, sid: &str, minted: &MintedAccess) -> AccessClaims {
        AccessClaims {
            iss: self.issuer.clone(),
            sub: sub.to_string(),
            aud: minted.audience.clone(),
            client_id: minted.client_id.clone(),
            sid: sid.to_string(),
            iat: minted.iat,
            exp: minted.iat.saturating_add(ACCESS_TTL_SECS),
            jti: crate::store::new_token(),
            preferred_username: minted.handle.clone(),
            root_domains: minted.root_domains.clone(),
        }
    }

    /// Signs access token `claims` as `typ` [`ACCESS_TOKEN_TYP`] with the active key.
    pub(crate) async fn sign(&self, conn: &mut PgConnection, claims: &AccessClaims) -> anyhow::Result<String> {
        self.sign_typed(conn, ACCESS_TOKEN_TYP, claims).await
    }

    /// Signs `claims` as `typ` with the active key. A cache refresh reads the key on `conn`,
    /// never on a second pool connection: a mint signing inside its transaction passes that
    /// transaction's connection, so it cannot wait on the pool while holding one of its own.
    pub(crate) async fn sign_typed<C: Serialize>(
        &self,
        conn: &mut PgConnection,
        typ: &str,
        claims: &C,
    ) -> anyhow::Result<String> {
        let active = self.active_key(conn).await?;
        let mut header = Header::new(Algorithm::ES256);
        header.typ = Some(typ.to_string());
        header.kid = Some(active.kid.clone());
        Ok(jsonwebtoken::encode(&header, claims, &active.key)?)
    }

    async fn active_key(&self, conn: &mut PgConnection) -> anyhow::Result<Arc<ActiveKey>> {
        let mut cached = self.active.lock().await;
        let now = (self.now)();
        if let Some((read_at, key)) = cached.as_ref() {
            if now.saturating_duration_since(*read_at) < ACTIVE_KEY_CACHE_TTL {
                return Ok(key.clone());
            }
        }
        let key = Arc::new(self.read_active_key(conn).await?);
        *cached = Some((now, key.clone()));
        Ok(key)
    }

    async fn read_active_key(&self, conn: &mut PgConnection) -> anyhow::Result<ActiveKey> {
        let kek = self
            .kek
            .get()
            .ok_or_else(|| anyhow::anyhow!("accounts: sign before migrate resolved {KEK_ENV}"))?;
        let (kid, sealed, nonce): (String, Vec<u8>, Vec<u8>) = sqlx::query_as(&format!(
            "SELECT kid, sealed_key, nonce FROM {}.signing_keys WHERE status = 'active'",
            self.schema
        ))
        .fetch_one(conn)
        .await?;
        let der = signing_keys::open(kek, &kid, &nonce, &sealed).ok_or_else(|| {
            anyhow::anyhow!("accounts: the active signing key {kid} does not open under {KEK_ENV}")
        })?;
        Ok(ActiveKey { kid, key: EncodingKey::from_ec_der(&der) })
    }

    /// The length of the longest token this issuer signs for a client of `audience` and
    /// `root_domains`: the longest player id, client id and JSON-escaped handle. Reads a stale
    /// signing key on `conn`.
    pub(crate) async fn longest_token_bytes(
        &self,
        conn: &mut PgConnection,
        audience: &str,
        root_domains: &[String],
    ) -> anyhow::Result<usize> {
        let uuid = "ffffffff-ffff-ffff-ffff-ffffffffffff";
        let minted = MintedAccess {
            client_id: "x".repeat(MAX_CLIENT_ID_BYTES),
            iat: i64::MAX - ACCESS_TTL_SECS,
            audience: audience.to_string(),
            root_domains: root_domains.to_vec(),
            // serde_json escapes U+0001 in six bytes, its widest escape (serde_json 1.0.150
            // src/ser.rs:2142-2150).
            handle: "\u{1}".repeat(MAX_HANDLE_BYTES),
        };
        Ok(self.sign(conn, &self.claims(uuid, uuid, &minted)).await?.len())
    }

    /// Fails when an access token of this issuer can be over [`MAX_ACCESS_TOKEN_BYTES`]: for a
    /// client of the default [`SWAROG_AUDIENCE`] and no root domains, or for any client stored
    /// in `oidc_clients`, read on `conn`.
    pub(crate) async fn check_token_size(&self, conn: &mut PgConnection) -> anyhow::Result<()> {
        let len = self.longest_token_bytes(conn, SWAROG_AUDIENCE, &[]).await?;
        if len > MAX_ACCESS_TOKEN_BYTES {
            anyhow::bail!(
                "accounts: an access token for issuer {} can be {len} bytes, over the {MAX_ACCESS_TOKEN_BYTES} \
                 a verifier accepts. Remedy: a shorter ACCOUNTS_OIDC_ISSUER.",
                self.issuer
            );
        }
        let clients: Vec<(String, String, Vec<String>)> = sqlx::query_as(
            "SELECT client_id, audience, root_domains FROM accounts.oidc_clients ORDER BY client_id",
        )
        .fetch_all(&mut *conn)
        .await?;
        for (client_id, audience, root_domains) in clients {
            let len = self.longest_token_bytes(conn, &audience, &root_domains).await?;
            if len > MAX_ACCESS_TOKEN_BYTES {
                anyhow::bail!(
                    "accounts: an access token for client {client_id} can be {len} bytes, over the \
                     {MAX_ACCESS_TOKEN_BYTES} a verifier accepts. Remedy: delete the client and create it \
                     again with a shorter audience or fewer root domains."
                );
            }
        }
        Ok(())
    }

    /// The issuer, the live keys and every revocation that can still refuse an unexpired
    /// token; a subject's several cut-offs fold into its latest.
    pub(crate) async fn snapshot(&self) -> anyhow::Result<TokenStateSnapshot> {
        let keys = self.keys.jwks().await?.keys.clone();
        let rows: Vec<RevocationRow> = sqlx::query_as(
            "SELECT family_id::text, player_id::text, client_id, \
                    ceil(extract(epoch FROM max(revoked_at)))::bigint \
               FROM accounts.token_revocations WHERE expires_at > now() \
              GROUP BY family_id, player_id, client_id ORDER BY 1, 2, 3",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut snapshot = TokenStateSnapshot { issuer: self.issuer.clone(), keys, ..Default::default() };
        for (family, player, client, revoked_at_unix) in rows {
            match (family, player, client) {
                (Some(family), _, _) => snapshot.revoked_families.push(family),
                (None, Some(id), _) => snapshot.revoked_players.push(Revoked { id, revoked_at_unix }),
                (None, None, Some(id)) => snapshot.revoked_clients.push(Revoked { id, revoked_at_unix }),
                (None, None, None) => {}
            }
        }
        Ok(snapshot)
    }
}

#[cfg_attr(not(test), expect(dead_code, reason = "the /token endpoint is its only signer"))]
impl Tokens {
    /// The claims of the id_token issued to `client_id` at `iat` for player `sub` of `handle`,
    /// who logged in at `auth_time`. It lives as long as an access token.
    pub(crate) fn id_claims(
        &self,
        sub: &str,
        client_id: &str,
        iat: i64,
        auth_time: i64,
        nonce: Option<&str>,
        handle: &str,
    ) -> IdClaims {
        IdClaims {
            iss: self.issuer.clone(),
            sub: sub.to_string(),
            aud: client_id.to_string(),
            iat,
            exp: iat.saturating_add(ACCESS_TTL_SECS),
            auth_time,
            nonce: nonce.map(str::to_string),
            preferred_username: handle.to_string(),
        }
    }

    /// Signs id_token `claims` as `typ` [`ID_TOKEN_TYP`] with the active key, read on `conn`.
    pub(crate) async fn sign_id(&self, conn: &mut PgConnection, claims: &IdClaims) -> anyhow::Result<String> {
        self.sign_typed(conn, ID_TOKEN_TYP, claims).await
    }
}

impl Tokens {
    /// The claims of `token` when [`Tokens::verify_against`] accepts it under the current
    /// [`Tokens::snapshot`]; `Err` only when the keys or revocations cannot be read.
    pub(crate) async fn verify_access(&self, token: &str) -> anyhow::Result<Option<AccessClaims>> {
        if token.len() > MAX_ACCESS_TOKEN_BYTES {
            return Ok(None);
        }
        let snapshot = self.snapshot().await?;
        Ok(Tokens::verify_against(&snapshot, token, Tokens::unix_now()))
    }

    /// The claims of `token` when it is an ES256 `at+jwt` signed by a key of `snapshot`, of
    /// its issuer and the Swarog audience, not past `exp` by more than [`ACCESS_EXP_LEEWAY`]
    /// at `now` (Unix seconds), and not refused by a revocation.
    pub(crate) fn verify_against(snapshot: &TokenStateSnapshot, token: &str, now: i64) -> Option<AccessClaims> {
        if token.len() > MAX_ACCESS_TOKEN_BYTES {
            return None;
        }
        let header = jsonwebtoken::decode_header(token).ok()?;
        if header.alg != Algorithm::ES256 || header.typ.as_deref() != Some(ACCESS_TOKEN_TYP) {
            return None;
        }
        let kid = header.kid?;
        let jwk = snapshot.keys.iter().find(|k| k.kid == kid)?;
        let key = DecodingKey::from_ec_components(&jwk.x, &jwk.y).ok()?;
        let mut validation = Validation::new(Algorithm::ES256);
        validation.set_issuer(&[&snapshot.issuer]);
        validation.set_audience(&[SWAROG_AUDIENCE]);
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        validation.validate_exp = false;
        let claims = jsonwebtoken::decode::<AccessClaims>(token, &key, &validation).ok()?.claims;
        if now > claims.exp.saturating_add(ACCESS_EXP_LEEWAY.as_secs() as i64) {
            return None;
        }
        let cut_off = |list: &[Revoked], id: &str| {
            list.iter().any(|r| r.id == id && claims.iat <= r.revoked_at_unix)
        };
        if snapshot.revoked_families.contains(&claims.sid)
            || cut_off(&snapshot.revoked_players, &claims.sub)
            || cut_off(&snapshot.revoked_clients, &claims.client_id)
        {
            return None;
        }
        Some(claims)
    }

    pub(crate) fn unix_now() -> i64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
    }
}
