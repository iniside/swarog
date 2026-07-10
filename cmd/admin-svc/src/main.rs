//! `admin-svc` — the admin portal fortress process: aggregator + session-auth owner,
//! schema `admin`. It hosts the `admin` module (the `/admin` portal) plus one
//! `remote::Stub` per provider, so the sidebar composes REMOTE admin items fetched
//! over the mTLS QUIC edge: each stub carries ONLY that provider's admin fan-out
//! factory (`adminrpc::admin_remote_factory`), which contributes an
//! `adminapi::Item { remote_fetch }` dialing the peer's `admin.adminData`.
//!
//! Since the admin-hardening rollout it is DB-BACKED: the admin module owns schema
//! `admin` (users / sessions / login_attempts — GameOps identity, argon2id session
//! login, lockout, CSRF), and the DB brings the app-owned durable plane with it, so
//! the portal's `admin.action` audit events append here like anywhere else. It still
//! hosts NO edge server of its own — it only DIALS the seven peers (characters,
//! inventory, config, accounts, audit, scheduler, apikeys) — and no `gateway` module:
//! it fronts no typed ops (a browser reaches `/admin` through gateway-svc's HTTP
//! passthrough), so it needs no verifier/auth-once boundary. The admin module's own
//! session gate (DB-backed, minted by `adminctl`/`install.sh`) guards the portal.
//!
//! Peer edge addresses come from `<PROVIDER>_EDGE_ADDR` (defaulting to the split-proof
//! ports); the shared dev CA (`EDGE_CA_CERT`/`EDGE_CA_KEY`) authenticates the dials.

use lifecycle::ProcessWiring;

/// Reads `env_key`, falling back to `default` when unset or blank (a NUMERIC
/// `host:port`, e.g. `127.0.0.1:9000` — Rust's `SocketAddr` needs a literal IP).
fn env_addr(env_key: &str, default: &str) -> String {
    std::env::var(env_key)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    // Peer edge addresses, resolved from env once here (the composition root owns
    // topology knowledge); `admin_svc::modules` builds the portal + one admin-only
    // stub per provider from this wiring, dialing each peer's edge lazily on the
    // first /admin request that fetches its item.
    let wiring = ProcessWiring::new()
        .with_peer("characters", env_addr("CHARACTERS_EDGE_ADDR", "127.0.0.1:9000"))
        .with_peer("inventory", env_addr("INVENTORY_EDGE_ADDR", "127.0.0.1:9001"))
        .with_peer("config", env_addr("CONFIG_EDGE_ADDR", "127.0.0.1:9002"))
        .with_peer("accounts", env_addr("ACCOUNTS_EDGE_ADDR", "127.0.0.1:9003"))
        .with_peer("audit", env_addr("AUDIT_EDGE_ADDR", "127.0.0.1:9004"))
        .with_peer("scheduler", env_addr("SCHEDULER_EDGE_ADDR", "127.0.0.1:9005"))
        .with_peer("apikeys", env_addr("APIKEYS_EDGE_ADDR", "127.0.0.1:9009"));
    let mods = admin_svc::modules(&wiring);

    // DB on (the admin module owns schema `admin`; DB ⇒ durable plane); still no
    // edge server of its own (it only DIALS peers via the stubs).
    app::run(app::Config::from_env(), mods, None, None).await
}
