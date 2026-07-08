//! `admin-svc` — the admin portal fortress process (Step 7). It hosts the `admin`
//! module (the `/admin` portal) plus one `remote::Stub` per provider, so the sidebar
//! composes REMOTE admin items fetched over the mTLS QUIC edge: each stub carries ONLY
//! that provider's admin fan-out factory (`adminrpc::admin_remote_factory`), which
//! contributes an `adminapi::Item { remote_fetch }` dialing the peer's `admin.adminData`.
//!
//! It is a PURE AGGREGATOR (like `gateway-svc`): no DB (`without_db` — it owns no
//! schema), and NO edge server of its own — it only DIALS the four peers. It hosts no
//! `gateway` module either: it fronts no typed ops (a browser reaches `/admin` through
//! gateway-svc's HTTP passthrough), so it needs no verifier/auth-once boundary. The
//! admin module's own Basic-auth gate (`ADMIN_USER`/`ADMIN_PASS`) guards the portal.
//!
//! Peer edge addresses come from `<PROVIDER>_EDGE_ADDR` (defaulting to the split-proof
//! ports); the shared dev CA (`EDGE_CA_CERT`/`EDGE_CA_KEY`) authenticates the dials.

use lifecycle::Module;

/// Reads `env_key`, falling back to `default` when unset or blank (a NUMERIC
/// `host:port`, e.g. `127.0.0.1:9000` — Rust's `SocketAddr` needs a literal IP).
fn env_addr(env_key: &str, default: &str) -> String {
    std::env::var(env_key)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// One admin-only stub for `provider`: it applies JUST the admin fan-out factory (not
/// the provider's full `remote_factories()`), so admin-svc contributes the provider's
/// REMOTE admin item WITHOUT also becoming front-capable for its typed ops.
fn admin_stub(provider: &str, env_key: &str, default: &str) -> Box<dyn Module> {
    Box::new(remote::Stub::new(
        provider,
        &env_addr(env_key, default),
        vec![adminrpc::admin_remote_factory(provider)],
    ))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    // The admin portal + one admin-only stub per provider. Each stub dials its peer's
    // edge lazily on the first /admin request that fetches its item.
    let mods: Vec<Box<dyn Module>> = vec![
        Box::new(metrics::Metrics::new()), // core-infra: mounts GET /metrics + contributes the record layer
        Box::new(admin::Admin::new()),
        admin_stub("characters", "CHARACTERS_EDGE_ADDR", "127.0.0.1:9000"),
        admin_stub("inventory", "INVENTORY_EDGE_ADDR", "127.0.0.1:9001"),
        admin_stub("config", "CONFIG_EDGE_ADDR", "127.0.0.1:9002"),
        admin_stub("accounts", "ACCOUNTS_EDGE_ADDR", "127.0.0.1:9003"),
        admin_stub("audit", "AUDIT_EDGE_ADDR", "127.0.0.1:9004"),
        admin_stub("scheduler", "SCHEDULER_EDGE_ADDR", "127.0.0.1:9005"),
    ];

    // No DB (owns no schema) and no edge server (it only DIALS peers via the stubs).
    app::run(app::Config::from_env().without_db(), mods, None, None).await
}
