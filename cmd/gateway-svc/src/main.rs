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

mod addrs;
mod tlsenv;
#[cfg(test)]
mod tlsenv_tests;

/// Parses `CREDENTIAL_ADMISSION_TIMEOUT_MS` — the front door's whole-credential-
/// admission deadline (api-key check + session verify, both public planes). Env is
/// read HERE in the composition root (the gateway module never reads env).
fn admission_budget_from_env() -> anyhow::Result<Option<std::time::Duration>> {
    admission_budget_from_value(std::env::var("CREDENTIAL_ADMISSION_TIMEOUT_MS").ok().as_deref())
}

/// The testable parser body. Each front main (`cmd/gateway-svc` here and `cmd/server`)
/// keeps its OWN copy of this fn per the repo's env-in-main convention — there is no
/// shared config crate for `cmd/*` roots.
///
/// Unset/blank/unparseable → `Ok(None)`: the module's 5000ms default applies (the same
/// lenient trim/parse shape `core/app`'s grace knobs use). An EXPLICIT `0` FAILS
/// STARTUP LOUDLY: unlike the sibling knobs where `0` means "disable", this deadline
/// guards an always-on security surface — a zero budget would time out every admission
/// instantly (every credentialed request 503s, a silently bricked front door), and
/// mapping `0` to "no bound" would reintroduce the unbounded-hang defect, so neither
/// meaning is acceptable to infer silently.
fn admission_budget_from_value(raw: Option<&str>) -> anyhow::Result<Option<std::time::Duration>> {
    let Some(v) = raw.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    match v.parse::<u64>() {
        Ok(0) => anyhow::bail!(
            "CREDENTIAL_ADMISSION_TIMEOUT_MS=0 is invalid: 0 would time out every \
             admission instantly (every credentialed request would 503); running \
             without a bound is not supported — unset the var for the 5000ms default"
        ),
        Ok(ms) => Ok(Some(std::time::Duration::from_millis(ms))),
        Err(_) => Ok(None),
    }
}

#[cfg(test)]
mod admission_budget_tests;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    // Pin the process-global rustls crypto provider to ring BEFORE any TLS config is
    // built (admin hardening Step 4 — the crypto-provider trap): the default-provider
    // builders inside axum-server/rustls-acme then resolve to ring, matching the
    // workspace's ring-only rustls. `install_default` errs only when a default is
    // already installed — same provider here, so ignoring it is safe (the `.ok()`
    // idempotent-install shape).
    let _ = rustls::crypto::ring::default_provider().install_default();

    // TLS front for the public HTTP plane: TLS_MODE=off (default) | files | acme —
    // parsed HERE in the composition root ([R12]: no other cmd/* main is TLS-aware;
    // core/app carries only the mechanism). Partial config fails startup loudly.
    let tls = tlsenv::tls_front_from_env()?;

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
    // Peer edge addresses and passthrough origins are resolved HERE (the composition
    // root owns topology), never read inside the module: `/admin` → admin-svc,
    // `/accounts/epic` → the Epic web OAuth flow on accounts-svc.
    //
    // WHERE those eight addresses come from is `addrs`'s one decision, made once:
    // `ORCHESTRATOR_URL` set ⇒ managed (ask the local weles agent over
    // `remote::resolve_peer`), unset ⇒ standalone (the eight env vars, byte-identical
    // to before). The modes are disjoint — a managed boot never falls back to env —
    // and everything below this line is blind to which one ran: `ResolvedAddrs::to_wiring`
    // builds the same `ProcessWiring`, so `gateway_svc::modules` and its `Stub`s are
    // handed addresses exactly as they always were.
    //
    // The credential-admission budget (`CREDENTIAL_ADMISSION_TIMEOUT_MS`) is likewise
    // resolved here: one deadline bounding the front door's api-key + session verify
    // on both planes (unset → the gateway module's 5s default). It, `PLAYER_EDGE_ADDR`
    // and the TLS front stay env-sourced in BOTH modes — they are this process's own
    // config, not a peer's address, so the agent has nothing to say about them.
    let source = addrs::addr_source_from_env()?;
    // Empty in standalone, where the closure below is never called (`AddrSource::Env`
    // carries no URL — the mode is structurally unable to ask anyone).
    let agent_url = source.agent_url().unwrap_or_default().to_string();
    let mut wiring = addrs::gateway_addrs(
        &source,
        |env_key| std::env::var(env_key).ok(),
        |provider, kind| remote::resolve_peer(&agent_url, provider, kind),
    )
    .await?
    .to_wiring();
    if let Some(budget) = admission_budget_from_env()? {
        wiring = wiring.with_admission_budget(budget);
    }
    // Managed boot (A5): hand each edge stub a live re-resolver bound to the agent URL,
    // so its reconnecting caller picks up a moved peer without restarting this front
    // door. Standalone boot (`AddrSource::Env`, `agent_url()` is `None`) passes `None`
    // — constant resolvers, byte-identical to before. The boot snapshot the gateway
    // route table reads was already resolved once above by `gateway_addrs`.
    let edge_mk = if source.agent_url().is_some() {
        Some(move |provider: &'static str| addrs::edge_resolver(&agent_url, provider))
    } else {
        None
    };
    let edge_mk_ref: Option<&dyn Fn(&'static str) -> remote::PeerResolver> = edge_mk
        .as_ref()
        .map(|f| f as &dyn Fn(&'static str) -> remote::PeerResolver);
    let mods = gateway_svc::modules(&wiring, Some(player.clone()), edge_mk_ref);

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
            .with_rate_limit_default(20.0, 40)
            .with_tls(tls),
        mods,
        None,
        Some(player),
    )
    .await
}
