//! `gateway-svc` — the dedicated front-door process (port of Go's `cmd/gateway-svc`).
//! It is a PURE TRANSPORT process: no DB (`Config::without_db`), no messaging module
//! — the async plane (outbox → `POST /events`) is delivered svc→svc and bypasses the
//! front door entirely, per the `async-fanout-sync-grpc-brokerless` decision. It hosts
//! NO provider module, only `remote::Stub`s for `characters` and `inventory`, so EVERY
//! op it fronts resolves `BackendKind::Remote` and is dialed over the mTLS edge to the
//! owning peer.
//!
//! Two public planes, one shared `FrontDoor`: HTTP (`PORT`, default `:8082`) and the
//! player-facing QUIC front (`PLAYER_EDGE_ADDR`, default `:9100`) — server-cert-only
//! TLS, bearer-in-envelope auth verified once at the front against the matched op's
//! `AuthReq`, method allow-listed dispatch (never a blind prefix relay). Ports are set
//! by the run scripts, not here.

use std::sync::{Arc, Mutex};

use lifecycle::Module;

/// Reads `env_key`, falling back to `default` when unset or blank — generalizes
/// `characters-svc`'s bespoke `characters_edge_addr()` to any provider's peer
/// address (a NUMERIC `host:port`, e.g. `127.0.0.1:9000`; Rust's `SocketAddr` needs a
/// literal IP, unlike Go's dialer).
fn env_addr(env_key: &str, default: &str) -> String {
    std::env::var(env_key)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    // One shared player-facing QUIC server for this process; `Gateway::with_player_edge`
    // installs the front's dispatch handler onto it during `init`, and `app::run`
    // `listen`s the same handle after Build — this IS the QUIC player front door.
    let player = Arc::new(Mutex::new(edge::PlayerServer::new()));

    // No provider modules: `Stub`s stand in for both `characters` and `inventory`, so
    // this process hosts no schema and every op dispatches Remote over the edge.
    let mods: Vec<Box<dyn Module>> = vec![
        Box::new(gateway::Gateway::new().with_player_edge(player.clone())),
        Box::new(remote::Stub::new(
            "characters",
            &env_addr("CHARACTERS_EDGE_ADDR", "127.0.0.1:9000"),
        )),
        Box::new(remote::Stub::new(
            "inventory",
            &env_addr("INVENTORY_EDGE_ADDR", "127.0.0.1:9001"),
        )),
    ];

    // No edge server: this process serves no provider over the internal mTLS edge, it
    // only DIALS peers (via the stubs). `without_db`: a pure-transport process owns no
    // schema, so `app::run` skips `PgPool::connect` and `/readyz` answers a plain 200.
    app::run(app::Config::from_env().without_db(), mods, None, Some(player)).await
}
