//! `admin-svc` — composition root: the core admin portal plus metrics; no domain
//! module; DB-backed.
//!
//! The core admin portal owns schema `admin` (users / sessions /
//! login_attempts — GameOps identity, argon2id session login, lockout, CSRF), and the
//! DB brings the app-owned durable plane with it, so the `admin.action` audit events
//! append here like anywhere else. It hosts NO edge server and no `gateway` module: it
//! fronts no typed ops (a browser reaches `/admin` through gateway-svc's HTTP
//! passthrough), so it needs no verifier/auth-once boundary. The portal's own
//! session gate (DB-backed, minted by `adminctl`/`install.sh`) guards it.

use lifecycle::ProcessWiring;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    let mods = admin_svc::modules(&ProcessWiring::new());

    // DB on (the core admin portal owns schema `admin`; DB ⇒ durable plane); no edge server.
    app::run(app::Config::from_env(), mods, None, None).await
}
