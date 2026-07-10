//! `gateway-svc` — the dedicated front-door process (port of Go's `cmd/gateway-svc`).
//! It is a PURE TRANSPORT process: no DB (`Config::without_db`), so no durable-events
//! plane — the plane is app-owned and exists only where there is a DB (DB ⇒ plane);
//! durable events live in the shared Postgres log and are pulled svc-side, bypassing
//! the front door entirely. It hosts
//! NO provider module, only `remote::Stub`s for `characters`, `inventory` and
//! `accounts`, so EVERY op it fronts resolves `BackendKind::Remote` and is dialed
//! over the mTLS edge to the owning peer. The `accounts` stub is MANDATORY (Step 6):
//! its factory provides the `accounts.sessions` edge client the gateway's verifier
//! resolves at init — real bearer verification against accounts-svc, no `dev-`
//! tokens (absent the capability the gateway fails startup unless
//! `ACCOUNTS_DEV_AUTH=1` is explicitly set). The `apikeys` stub is likewise MANDATORY
//! (Step 3, api key policy): its factory provides the `apikeys.keys` edge client the
//! gateway's key verifier resolves at init — real key verification against
//! apikeys-svc, absent the capability the gateway fails startup unless
//! `APIKEYS_DEV_ALLOW=1` is explicitly set.
//!
//! Two public planes, one shared `FrontDoor`: HTTP (`PORT`, default `:8082`) and the
//! player-facing QUIC front (`PLAYER_EDGE_ADDR`, default `:9100`) — server-cert-only
//! TLS, bearer-in-envelope auth verified once at the front against the matched op's
//! `AuthReq`, method allow-listed dispatch (never a blind prefix relay). Ports are set
//! by the run scripts, not here.

use std::sync::{Arc, Mutex};

use lifecycle::ProcessWiring;

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
    //
    // Passthrough origins are resolved HERE (the composition root owns topology), not
    // read inside the module: `/admin` → admin-svc, `/accounts/epic` → the Epic web
    // OAuth flow on accounts-svc. A blank default drops the prefix (the proxy table
    // skips empties), so an unset var leaves that route a 404.
    let wiring = ProcessWiring::new()
        .with_peer("characters", env_addr("CHARACTERS_EDGE_ADDR", "127.0.0.1:9000"))
        .with_peer("inventory", env_addr("INVENTORY_EDGE_ADDR", "127.0.0.1:9001"))
        .with_peer("accounts", env_addr("ACCOUNTS_EDGE_ADDR", "127.0.0.1:9003"))
        .with_peer("apikeys", env_addr("APIKEYS_EDGE_ADDR", "127.0.0.1:9009"))
        // Step 10: match + leaderboard front-door routing. Their `remote_factories`
        // contribute only `route_bindings` (no provide), so the front routes
        // `POST /match/report` -> match-svc (:9006) and `GET /leaderboard` ->
        // leaderboard-svc (:9008) Remote over the mTLS edge.
        .with_peer("match", env_addr("MATCH_EDGE_ADDR", "127.0.0.1:9006"))
        .with_peer("leaderboard", env_addr("LEADERBOARD_EDGE_ADDR", "127.0.0.1:9008"))
        .with_passthrough("/admin", env_addr("ADMIN_HTTP_ADDR", ""))
        .with_passthrough("/accounts/epic", env_addr("ACCOUNTS_HTTP_ADDR", ""));
    let mods = gateway_svc::modules(&wiring, Some(player.clone()));

    // No edge server: this process serves no provider over the internal mTLS edge, it
    // only DIALS peers (via the stubs). `without_db`: a pure-transport process owns no
    // schema, so `app::run` skips `PgPool::connect` and `/readyz` answers a plain 200.
    // The `metrics` module in `mods` gives the front door `GET /metrics` + the record
    // layer, so its op traffic IS measured now (the old `without_metrics` Go-parity
    // exemption lost its rationale once peers stopped fronting HTTP; ops dispatch through
    // the axum fallback, so they record under `path="unmatched"`).
    // `with_rate_limit_default(20.0, 40)`: the front door ALWAYS rate limits (Go's
    // `cmd/gateway-svc` values), unlike a module host where it is opt-in. `RATE_LIMIT_RPS`
    // / `RATE_LIMIT_BURST` / `TRUSTED_PROXY_CIDRS` env still override. The limiter fronts
    // the HTTP plane (ops + `/admin`+`/accounts/epic` passthrough alike); the player QUIC
    // front is not rate limited (HTTP-plane concern, Go parity).
    app::run(
        app::Config::from_env()
            .without_db()
            .with_rate_limit_default(20.0, 40),
        mods,
        None,
        Some(player),
    )
    .await
}
