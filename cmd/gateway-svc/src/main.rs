//! `gateway-svc` — the dedicated front-door process (port of Go's `cmd/gateway-svc`).
//! It is a PURE TRANSPORT process: no DB (`Config::without_db`), no messaging module
//! — the async plane (outbox → `POST /events`) is delivered svc→svc and bypasses the
//! front door entirely, per the `async-fanout-sync-grpc-brokerless` decision. It hosts
//! NO provider module, only `remote::Stub`s for `characters`, `inventory` and
//! `accounts`, so EVERY op it fronts resolves `BackendKind::Remote` and is dialed
//! over the mTLS edge to the owning peer. The `accounts` stub is MANDATORY (Step 6):
//! its factory provides the `accounts.sessions` edge client the gateway's verifier
//! resolves at init — real bearer verification against accounts-svc, no `dev-`
//! tokens (absent the capability the gateway fails startup unless
//! `ACCOUNTS_DEV_AUTH=1` is explicitly set).
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
    // `remote` is generic (Step 4): this composition root injects each provider's
    // swap closures (`<name>rpc::remote_factories()`) explicitly, so `remote` names no
    // provider. It reaches the two `<name>rpc` glue crates (sanctioned for `cmd/*`,
    // rule 5) but never the provider IMPL crates.
    let mods: Vec<Box<dyn Module>> = vec![
        Box::new(gateway::Gateway::new().with_player_edge(player.clone())),
        Box::new(remote::Stub::new(
            "characters",
            &env_addr("CHARACTERS_EDGE_ADDR", "127.0.0.1:9000"),
            charactersrpc::remote_factories(),
        )),
        Box::new(remote::Stub::new(
            "inventory",
            &env_addr("INVENTORY_EDGE_ADDR", "127.0.0.1:9001"),
            inventoryrpc::remote_factories(),
        )),
        Box::new(remote::Stub::new(
            "accounts",
            &env_addr("ACCOUNTS_EDGE_ADDR", "127.0.0.1:9003"),
            accountsrpc::remote_factories(),
        )),
        // Step 10: match + leaderboard front-door routing. Their `remote_factories`
        // contribute only `route_bindings` (no provide), so the front routes
        // `POST /match/report` -> match-svc (:9006) and `GET /leaderboard` ->
        // leaderboard-svc (:9008) Remote over the mTLS edge.
        Box::new(remote::Stub::new(
            "match",
            &env_addr("MATCH_EDGE_ADDR", "127.0.0.1:9006"),
            matchrpc::remote_factories(),
        )),
        Box::new(remote::Stub::new(
            "leaderboard",
            &env_addr("LEADERBOARD_EDGE_ADDR", "127.0.0.1:9008"),
            leaderboardrpc::remote_factories(),
        )),
    ];

    // No edge server: this process serves no provider over the internal mTLS edge, it
    // only DIALS peers (via the stubs). `without_db`: a pure-transport process owns no
    // schema, so `app::run` skips `PgPool::connect` and `/readyz` answers a plain 200.
    // `without_metrics`: the front door carries no Prometheus scrape (Go parity — the
    // gateway binary was the one process that never imported the metrics package), so
    // `GET /metrics` is a 404 here while every module-hosting peer serves it.
    app::run(
        app::Config::from_env().without_db().without_metrics(),
        mods,
        None,
        Some(player),
    )
    .await
}
