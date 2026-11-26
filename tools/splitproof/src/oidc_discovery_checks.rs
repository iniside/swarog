//! The OIDC discovery document and the JWKS accounts serves under `/accounts/oidc`, proven through each
//! front door: gateway-svc's passthrough into accounts-svc in the split, the monolith's own router in
//! parity. The once-wrong branch is a route that is missing or not proxied, which answers 404.
//!
//! - [SK1] discovery answers 200 with the pinned proof issuer (`processctl::PROOF_ACCOUNTS_ISSUER_SPLIT`
//!   or `_MONOLITH`) and `jwks_uri` = `<issuer>/jwks`.
//! - [SK2] that `jwks_uri`, fetched exactly as served (not rebuilt from the front), answers 200 with a
//!   `keys` array, so the issuer a launcher pins resolves to the front that serves it.
//! - [SK3] the JWKS holds exactly one key, a public ES256 P-256 signing key with no private member, and
//!   its `kid` is the one `active` row of `accounts.signing_keys` with no `retiring` row beside it.
//! - [SK4] (split only) after accounts-svc is killed and respawned from its canonical spec, the JWKS
//!   through gateway-svc still serves that one key, and the table still holds that one active row: the
//!   new process kept the stored key instead of minting one at boot. The old process is gone before the
//!   respawn (`Running`'s drop kills and waits), so its cache cannot serve the answer.
//!
//! The split pass runs after [REPLICAS] and before [LV2], so the respawned accounts-svc is in the
//! liveness sweep and no earlier check meets a gateway connection to the killed process.
//!
//! Break run (a full splitproof run, then reverted): the JWKS route mounted at
//! `/accounts/oidc/jwks-break-run` instead of `JWKS` (`modules/accounts/src/discovery.rs`), run
//! splitproof-90564: SK2, SK3, SK0, SK2m, SK3m and SK0m red as new (`code=404` at the served
//! `jwks_uri`), SK1 and SK1m green, so [SK4] did not run (2026-11-26, step 4).

use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
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

/// [SK1]-[SK4] through gateway-svc, the fleet's ownership lent so accounts-svc can be respawned.
pub(super) async fn split(ctx: &Ctx, pool: &PgPool, fleet: &mut Vec<Running>, p: &mut Proof) {
    println!("\n[splitproof] === [SK] OIDC discovery and JWKS through gateway-svc ===");
    let g = format!("http://127.0.0.1:{}", ctx.http_port("gateway-svc"));
    let kid = match served_key(ctx, pool, p, &g, processctl::PROOF_ACCOUNTS_ISSUER_SPLIT, "").await {
        Ok(kid) => kid,
        Err(e) => {
            p.check("[SK0] OIDC discovery and JWKS pass completes", false, format!("{e:#}"));
            return;
        }
    };
    if let Err(e) = restart_keeps_kid(ctx, pool, fleet, p, &g, &kid).await {
        p.check("[SK4] accounts-svc restart keeps the signing key", false, format!("{e:#}"));
    }
}

/// [SK1m]-[SK3m] on the monolith front `m`.
pub(super) async fn monolith(ctx: &Ctx, pool: &PgPool, p: &mut Proof, m: &str) {
    if let Err(e) = served_key(ctx, pool, p, m, processctl::PROOF_ACCOUNTS_ISSUER_MONOLITH, "m").await {
        p.check("[SK0m] OIDC discovery and JWKS pass completes", false, format!("{e:#}"));
    }
}

/// [SK1]-[SK3] on `front`; the served `kid` once [SK3] passes.
async fn served_key(ctx: &Ctx, pool: &PgPool, p: &mut Proof, front: &str, issuer: &str, tag: &str) -> Result<String> {
    let (code, doc) = get_json(ctx, &format!("{front}{DISCOVERY}")).await?;
    let served_issuer = doc.get("issuer").and_then(Value::as_str).unwrap_or_default().to_string();
    let jwks_uri = doc.get("jwks_uri").and_then(Value::as_str).unwrap_or_default().to_string();
    let want_uri = format!("{issuer}/jwks");
    p.check(
        &format!("[SK1{tag}] GET {front}{DISCOVERY} -> 200, issuer {issuer}, jwks_uri {want_uri}"),
        code == 200 && served_issuer == issuer && jwks_uri == want_uri,
        format!("code={code} issuer={served_issuer} jwks_uri={jwks_uri}"),
    );
    if jwks_uri.is_empty() {
        bail!("discovery served no jwks_uri (code={code})");
    }

    let (code, set) = get_json(ctx, &jwks_uri).await?;
    let keys = set.get("keys").and_then(Value::as_array).cloned();
    p.check(
        &format!("[SK2{tag}] GET the served jwks_uri -> 200 + a keys array"),
        code == 200 && keys.is_some(),
        format!("code={code} url={jwks_uri} keys={}", keys.as_ref().map_or(-1, |k| k.len() as i64)),
    );
    let keys = keys.unwrap_or_default();

    let stored = stored_keys(pool).await?;
    let (ok, kid, detail) = one_stored_key(&keys, &stored);
    p.check(
        &format!("[SK3{tag}] the JWKS is one public ES256 key whose kid is the one active accounts.signing_keys row"),
        ok,
        detail,
    );
    if !ok {
        bail!("[SK3{tag}] failed; no kid to carry");
    }
    Ok(kid)
}

/// [SK4]: kill accounts-svc, respawn it from its canonical spec, and read the JWKS through gateway-svc.
async fn restart_keeps_kid(
    ctx: &Ctx,
    pool: &PgPool,
    fleet: &mut Vec<Running>,
    p: &mut Proof,
    g: &str,
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

    let url = format!("{g}/accounts/oidc/jwks");
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

/// Whether `keys` is exactly one public ES256 key and `stored` exactly one `active` row with its `kid`;
/// the served `kid` and the detail either way.
fn one_stored_key(keys: &[Value], stored: &[(String, String)]) -> (bool, String, String) {
    let member = |key: &Value, name: &str| key.get(name).and_then(Value::as_str).unwrap_or_default().to_string();
    let key = keys.first().cloned().unwrap_or(Value::Null);
    let kid = member(&key, "kid");
    let (kty, crv, alg, use_) = (member(&key, "kty"), member(&key, "crv"), member(&key, "alg"), member(&key, "use"));
    let coords = !member(&key, "x").is_empty() && !member(&key, "y").is_empty();
    let private: Vec<&str> = PRIVATE_MEMBERS.iter().copied().filter(|name| key.get(*name).is_some()).collect();
    let ok = keys.len() == 1
        && !kid.is_empty()
        && (kty.as_str(), crv.as_str(), alg.as_str(), use_.as_str()) == ("EC", "P-256", "ES256", "sig")
        && coords
        && private.is_empty()
        && stored.len() == 1
        && stored[0] == (kid.clone(), "active".to_string());
    let detail = format!(
        "keys={} kid={kid} kty={kty} crv={crv} alg={alg} use={use_} x_y={coords} private={private:?} stored={stored:?}",
        keys.len()
    );
    (ok, kid, detail)
}

/// Every `active` and `retiring` row of `accounts.signing_keys`, the rows the JWKS serves.
async fn stored_keys(pool: &PgPool) -> Result<Vec<(String, String)>> {
    sqlx::query_as::<_, (String, String)>(
        "SELECT kid, status FROM accounts.signing_keys WHERE status IN ('active', 'retiring') ORDER BY kid",
    )
    .fetch_all(pool)
    .await
    .context("read accounts.signing_keys")
}

/// The status and JSON body of a GET; a body that is not JSON reads as `null`.
async fn get_json(ctx: &Ctx, url: &str) -> Result<(u16, Value)> {
    let response = ctx.http.get(url).send().await.with_context(|| format!("GET {url}"))?;
    let code = response.status().as_u16();
    let body = response.text().await.with_context(|| format!("read GET {url}"))?;
    Ok((code, serde_json::from_str(&body).unwrap_or(Value::Null)))
}
