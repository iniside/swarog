//! `admin-svc` — composition root: the core admin portal plus metrics; no domain
//! module; DB-backed.
//!
//! The core admin portal owns schema `admin` (users / sessions / login_attempts —
//! GameOps identity, argon2id session login, lockout, CSRF), and the DB brings the
//! app-owned durable plane with it, so the `admin.action` audit events append here like
//! anywhere else. It hosts NO edge server and no `gateway` module: a browser reaches
//! `/admin` through gateway-svc's HTTP passthrough. It only DIALS the thirteen peers'
//! `cp.*` faces (characters, inventory, config, accounts, audit, scheduler, apikeys,
//! wallet, notifications, mail, friends, groups, storage).
//!
//! Peer edge addresses come from `<PEER>_EDGE_ADDR` (defaulting to the split-proof
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

    let wiring = ProcessWiring::new()
        .with_peer("characters", env_addr("CHARACTERS_EDGE_ADDR", "127.0.0.1:9000"))
        .with_peer("inventory", env_addr("INVENTORY_EDGE_ADDR", "127.0.0.1:9001"))
        .with_peer("config", env_addr("CONFIG_EDGE_ADDR", "127.0.0.1:9002"))
        .with_peer("accounts", env_addr("ACCOUNTS_EDGE_ADDR", "127.0.0.1:9003"))
        .with_peer("audit", env_addr("AUDIT_EDGE_ADDR", "127.0.0.1:9004"))
        .with_peer("scheduler", env_addr("SCHEDULER_EDGE_ADDR", "127.0.0.1:9005"))
        .with_peer("apikeys", env_addr("APIKEYS_EDGE_ADDR", "127.0.0.1:9009"))
        .with_peer("wallet", env_addr("WALLET_EDGE_ADDR", "127.0.0.1:9010"))
        .with_peer("notifications", env_addr("NOTIFICATIONS_EDGE_ADDR", "127.0.0.1:9011"))
        .with_peer("mail", env_addr("MAIL_EDGE_ADDR", "127.0.0.1:9012"))
        .with_peer("friends", env_addr("FRIENDS_EDGE_ADDR", "127.0.0.1:9014"))
        .with_peer("groups", env_addr("GROUPS_EDGE_ADDR", "127.0.0.1:9015"))
        .with_peer("storage", env_addr("STORAGE_EDGE_ADDR", "127.0.0.1:9016"));
    let mods = admin_svc::modules(&wiring);

    // DB on (the core admin portal owns schema `admin`; DB ⇒ durable plane); no edge server.
    app::run(app::Config::from_env(), mods, None, None).await
}
