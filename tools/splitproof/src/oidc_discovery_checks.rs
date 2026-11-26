//! The OIDC discovery document and the JWKS accounts serves under `/accounts/oidc`, proven through each
//! front door: gateway-svc's passthrough into accounts-svc in the split, the monolith's own router in
//! parity. The once-wrong branch is a route that is missing or not proxied, which answers 404.
//!
//! - [SK1] discovery answers 200 with the pinned proof issuer (`processctl::PROOF_ACCOUNTS_ISSUER_SPLIT`
//!   or `_MONOLITH`) and `jwks_uri` = `<issuer>/jwks`.
//! - [SK2] that `jwks_uri`, fetched exactly as served (not rebuilt from the front), answers 200 with a
//!   `keys` array, so the issuer a launcher pins resolves to the front that serves it.
//! - [SK3] the JWKS holds exactly one key, a public ES256 P-256 signing key with no private member, and
//!   its `kid` is the one `active` row of `accounts.signing_keys` with no `retiring` row beside it;
//!   that row's stored `public_jwk` has no private member either.
//! - [SK4] (split only) after accounts-svc is killed and respawned from its canonical spec, the JWKS at
//!   the pinned issuer's `/jwks` (gateway-svc) still serves that one key, and the table still holds that
//!   one active row: the new process kept the stored key instead of minting one at boot. The old process
//!   is gone before the respawn (`Running`'s drop kills and waits), so its cache cannot serve the
//!   answer. When [SK3] is red or [SK0] cut the pass short there is no kid to compare, so the pass
//!   records [SK4] red as "not run" (the cause in its detail) instead of skipping it.
//! - [SK0] / [SK0m] report only a fault no check records: a transport error or an unreadable body on
//!   the discovery GET, or a failed read of `accounts.signing_keys`. A discovery document without a
//!   `jwks_uri`, and a `jwks_uri` GET that fails, still record [SK2] (the error in its detail) and
//!   [SK3] red.
//!
//! The split pass runs after [REPLICAS] and before [LV2], so the respawned accounts-svc is in the
//! liveness sweep and no earlier check meets a gateway connection to the killed process.
//!
//! Break runs (each a full splitproof run, then reverted; 2026-11-26, step 4):
//! - The JWKS route mounted at `/accounts/oidc/jwks-break-run` instead of `JWKS`
//!   (`modules/accounts/src/discovery.rs`), run splitproof-65188: SK2, SK3, "[SK4] not run", SK2m and
//!   SK3m red as the only new reds (`code=404` at the served `jwks_uri`), SK1 and SK1m green, no SK0.
//! - `ensure_active_key` (`modules/accounts/src/signing_keys.rs`) retiring the active row before it
//!   ensures one, so every boot mints a new key, run splitproof-91936: [SK4] the only new red
//!   (`before` and the respawned `kid` differ), SK1-SK3 and SK1m-SK3m green.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::Value;
use sqlx::PgPool;
use splitproof::Running;

use crate::{ensure_no_stale_listener, Ctx, Proof};

const DISCOVERY: &str = "/accounts/oidc/.well-known/openid-configuration";
/// Members a JWK fills only for a private key (EC `d`, RSA `p`..`qi`, symmetric `k`).
const PRIVATE_MEMBERS: [&str; 7] = ["d", "p", "q", "dp", "dq", "qi", "k"];
/// How long [SK4] polls gateway-svc for the JWKS after the respawned accounts-svc is ready: a pooled
/// passthrough connection to the killed process may fail the first request.
const RESPAWN_BUDGET: Duration = Duration::from_secs(15);
/// The one [SK4] name for a split pass that never reached the restart; the cause is in the detail.
const SK4_NOT_RUN: &str = "[SK4] not run: no served kid to compare";

/// [SK1]-[SK4] through gateway-svc, the fleet's ownership lent so accounts-svc can be respawned.
pub(super) async fn split(ctx: &Ctx, pool: &PgPool, fleet: &mut Vec<Running>, p: &mut Proof) {
    println!("\n[splitproof] === [SK] OIDC discovery and JWKS through gateway-svc ===");
    let g = format!("http://127.0.0.1:{}", ctx.http_port("gateway-svc"));
    let kid = match served_key(ctx, pool, p, &g, processctl::PROOF_ACCOUNTS_ISSUER_SPLIT, "").await {
        Ok(Some(kid)) => kid,
        Ok(None) => {
            p.check(SK4_NOT_RUN, false, "[SK3] red, accounts-svc not restarted");
            return;
        }
        Err(e) => {
            p.check("[SK0] OIDC discovery and JWKS pass completes", false, format!("{e:#}"));
            p.check(SK4_NOT_RUN, false, "[SK0] red, accounts-svc not restarted");
            return;
        }
    };
    if let Err(e) = restart_keeps_kid(ctx, pool, fleet, p, &kid).await {
        p.check("[SK4] accounts-svc restart keeps the signing key", false, format!("{e:#}"));
    }
}

/// [SK1m]-[SK3m] on the monolith front `m`.
pub(super) async fn monolith(ctx: &Ctx, pool: &PgPool, p: &mut Proof, m: &str) {
    if let Err(e) = served_key(ctx, pool, p, m, processctl::PROOF_ACCOUNTS_ISSUER_MONOLITH, "m").await {
        p.check("[SK0m] OIDC discovery and JWKS pass completes", false, format!("{e:#}"));
    }
}

/// [SK1]-[SK3] on `front`: the served `kid` once [SK3] passes, `None` when [SK3] recorded the failure,
/// and `Err` only for a fault no check records (a transport error, an unreadable body, the table read).
async fn served_key(
    ctx: &Ctx,
    pool: &PgPool,
    p: &mut Proof,
    front: &str,
    issuer: &str,
    tag: &str,
) -> Result<Option<String>> {
    let (code, doc) = get_json(ctx, &format!("{front}{DISCOVERY}")).await?;
    let served_issuer = doc.get("issuer").and_then(Value::as_str).unwrap_or_default().to_string();
    let jwks_uri = doc.get("jwks_uri").and_then(Value::as_str).unwrap_or_default().to_string();
    let want_uri = format!("{issuer}/jwks");
    p.check(
        &format!("[SK1{tag}] GET {front}{DISCOVERY} -> 200, issuer {issuer}, jwks_uri {want_uri}"),
        code == 200 && served_issuer == issuer && jwks_uri == want_uri,
        format!("code={code} issuer={served_issuer} jwks_uri={jwks_uri}"),
    );

    // No served `jwks_uri`, or its GET failed: nothing read, so [SK2] (the error in its detail) and
    // [SK3] record red on `code=0` and no keys.
    let (code, set, error) = match jwks_uri.as_str() {
        "" => (0, Value::Null, String::new()),
        uri => match get_json(ctx, uri).await {
            Ok((code, set)) => (code, set, String::new()),
            Err(e) => (0, Value::Null, format!(" error={e:#}")),
        },
    };
    let keys = set.get("keys").and_then(Value::as_array).cloned();
    let count = keys.as_ref().map_or(-1, |k| k.len() as i64);
    p.check(
        &format!("[SK2{tag}] GET the served jwks_uri -> 200 + a keys array"),
        code == 200 && keys.is_some(),
        format!("code={code} url={jwks_uri} keys={count}{error}"),
    );
    let keys = keys.unwrap_or_default();

    let stored = stored_keys(pool).await?;
    let (ok, kid, detail) = one_stored_key(&keys, &stored);
    p.check(
        &format!("[SK3{tag}] the JWKS is one public ES256 key whose kid is the one active accounts.signing_keys row"),
        ok,
        detail,
    );
    Ok(ok.then_some(kid))
}

/// [SK4]: kill accounts-svc, respawn it from its canonical spec, and read the JWKS at the pinned split
/// issuer, which is gateway-svc's front.
async fn restart_keeps_kid(
    ctx: &Ctx,
    pool: &PgPool,
    fleet: &mut Vec<Running>,
    p: &mut Proof,
    kid: &str,
) -> Result<()> {
    let idx = fleet
        .iter()
        .position(|running| running.name == "accounts-svc")
        .context("accounts-svc missing from fleet (preflight_fleet should have caught this)")?;
    // Drop kills and waits; then let the OS free its HTTP and edge ports before rebinding.
    fleet.remove(idx);
    tokio::time::sleep(Duration::from_millis(800)).await;
    let spec = ctx.service("accounts-svc").clone();
    ensure_no_stale_listener(spec.name, spec.http_port)?;
    let mut running = ctx.spawn(&spec)?;
    ctx.wait_healthy(&spec, &mut running.child).await?;
    fleet.insert(idx, running);
    println!("[splitproof] accounts-svc respawned on :{}", spec.http_port);

    let url = format!("{}/jwks", processctl::PROOF_ACCOUNTS_ISSUER_SPLIT);
    let deadline = Instant::now() + RESPAWN_BUDGET;
    let (code, set) = loop {
        let answer = get_json(ctx, &url).await;
        if matches!(answer, Ok((200, _))) || Instant::now() >= deadline {
            break answer?;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    let keys = set.get("keys").and_then(Value::as_array).cloned().unwrap_or_default();
    let stored = stored_keys(pool).await?;
    let (ok, after, detail) = one_stored_key(&keys, &stored);
    p.check(
        "[SK4] accounts-svc killed and respawned -> the JWKS through gateway-svc serves the same one kid",
        code == 200 && ok && after == kid,
        format!("code={code} before={kid} {detail}"),
    );
    Ok(())
}

/// Whether `keys` is exactly one public ES256 key and `stored` exactly one `active` row with its `kid`,
/// no stored `public_jwk` holding a private member; the served `kid` and the detail either way.
fn one_stored_key(keys: &[Value], stored: &[StoredKey]) -> (bool, String, String) {
    let member = |key: &Value, name: &str| key.get(name).and_then(Value::as_str).unwrap_or_default().to_string();
    let key = keys.first().cloned().unwrap_or(Value::Null);
    let kid = member(&key, "kid");
    let (kty, crv, alg, use_) = (member(&key, "kty"), member(&key, "crv"), member(&key, "alg"), member(&key, "use"));
    let coords = !member(&key, "x").is_empty() && !member(&key, "y").is_empty();
    let private = private_members(&key);
    let rows: Vec<(&str, &str)> = stored.iter().map(|row| (row.kid.as_str(), row.status.as_str())).collect();
    let stored_private: Vec<(&str, Vec<&str>)> = stored
        .iter()
        .map(|row| (row.kid.as_str(), private_members(&row.public_jwk)))
        .filter(|(_, members)| !members.is_empty())
        .collect();
    let ok = keys.len() == 1
        && !kid.is_empty()
        && (kty.as_str(), crv.as_str(), alg.as_str(), use_.as_str()) == ("EC", "P-256", "ES256", "sig")
        && coords
        && private.is_empty()
        && rows == [(kid.as_str(), "active")]
        && stored_private.is_empty();
    let detail = format!(
        "keys={} kid={kid} kty={kty} crv={crv} alg={alg} use={use_} x_y={coords} private={private:?} \
         stored={rows:?} stored_private={stored_private:?}",
        keys.len()
    );
    (ok, kid, detail)
}

/// The members of `jwk` that only a private key fills.
fn private_members(jwk: &Value) -> Vec<&'static str> {
    PRIVATE_MEMBERS.iter().copied().filter(|name| jwk.get(*name).is_some()).collect()
}

/// One `active` or `retiring` row of `accounts.signing_keys`.
struct StoredKey {
    kid: String,
    status: String,
    public_jwk: Value,
}

/// Every `active` and `retiring` row of `accounts.signing_keys`, the rows the JWKS serves.
async fn stored_keys(pool: &PgPool) -> Result<Vec<StoredKey>> {
    let rows = sqlx::query_as::<_, (String, String, String)>(
        "SELECT kid, status, public_jwk::text FROM accounts.signing_keys \
         WHERE status IN ('active', 'retiring') ORDER BY kid",
    )
    .fetch_all(pool)
    .await
    .context("read accounts.signing_keys")?;
    rows.into_iter()
        .map(|(kid, status, jwk)| {
            let public_jwk = serde_json::from_str(&jwk).with_context(|| format!("parse the public_jwk of {kid}"))?;
            Ok(StoredKey { kid, status, public_jwk })
        })
        .collect()
}

/// The status and JSON body of a GET; a body that is not JSON reads as `null`.
async fn get_json(ctx: &Ctx, url: &str) -> Result<(u16, Value)> {
    let response = ctx.http.get(url).send().await.with_context(|| format!("GET {url}"))?;
    let code = response.status().as_u16();
    let body = response.text().await.with_context(|| format!("read GET {url}"))?;
    Ok((code, serde_json::from_str(&body).unwrap_or(Value::Null)))
}

#[cfg(test)]
#[path = "oidc_discovery_checks_tests.rs"]
mod tests;
