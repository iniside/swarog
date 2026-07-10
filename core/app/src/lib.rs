//! `app` — the reusable boot sequence shared by every `*-svc` entrypoint (port of
//! Go's `internal/app/app.go`). Each binary builds its OWN static list of modules
//! (importing only that service's crates — Cargo/the linker then drops every module
//! a binary never names) and hands it to [`run`]. `run` owns the machinery: open the
//! DB, wire the lifecycle [`Context`], two-phase Build, Migrate, Start, an axum HTTP
//! server, an optional QUIC edge listener, and graceful shutdown. It knows NOTHING
//! about which modules exist — the entrypoint decides the topology by choosing what
//! to pass in.
//!
//! This is the top wiring layer: it may depend on `edge`/`bus` (they never depend on
//! it), and it lives ABOVE `lifecycle` so [`validate_requires`] — a topology concern
//! — stays here rather than in the foundation.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use anyhow::Context as _;
use axum::http::StatusCode;
use axum::routing::get;
use lifecycle::{App, Context, Module};
use sqlx::PgPool;

const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";
const DEFAULT_LISTEN_ADDR: &str = ":8080";
const DEFAULT_EDGE_ADDR: &str = ":9000";
const DEFAULT_PLAYER_EDGE_ADDR: &str = ":9100";
const DEFAULT_EDGE_DRAIN_GRACE_MS: u64 = 5000;
const DEFAULT_HTTP_DRAIN_GRACE_MS: u64 = 5000;

/// The process-level configuration [`run`] needs. Deliberately tiny: everything
/// module-specific (event subscribers, peer edge addrs, admin URLs, …) is read by
/// the module that owns it, not here.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Postgres DSN (`DATABASE_URL`), or `None` for a persistence-free process (e.g.
    /// the pure-transport `gateway-svc`, which hosts no schema). [`Config::from_env`]
    /// always yields `Some`; [`Config::without_db`] is the explicit opt-out.
    pub database_url: Option<String>,
    /// HTTP listen address, e.g. `:8080` (`PORT`).
    pub listen_addr: String,
    /// QUIC edge listen address, e.g. `:9000` (`EDGE_ADDR`) — only used when an edge
    /// server is passed to [`run`].
    pub edge_addr: String,
    /// Player-facing QUIC listen address, e.g. `:9100` (`PLAYER_EDGE_ADDR`) — only
    /// used when a player server is passed to [`run`].
    pub player_edge_addr: String,
    /// The DEFAULT per-IP rate limit `(rps, burst)` applied when `RATE_LIMIT_RPS` is
    /// unset. `None` (the [`Config::from_env`] default) means **opt-in/off** — a
    /// module-hosting process (the monolith, each `*-svc`) runs behind the gateway, so
    /// limiting there would double-count and collapse every client into the gateway's
    /// single bucket; it stays off unless `RATE_LIMIT_RPS` is explicitly set. The
    /// gateway front door sets `Some((20.0, 40))` via [`Config::with_rate_limit_default`]
    /// so it is ALWAYS on (Go's `cmd/gateway-svc` values). Either way `RATE_LIMIT_RPS`
    /// and `RATE_LIMIT_BURST` env override the effective values.
    pub rate_limit_default: Option<(f64, u32)>,
    /// How long teardown waits for in-flight QUIC work (both planes) to drain before
    /// aborting it (`EDGE_DRAIN_GRACE_MS`, default 5000ms). A process/topology knob
    /// read HERE, never by a module.
    pub edge_drain_grace: std::time::Duration,
    /// How long, once the shutdown signal fires, the HTTP graceful drain gets to
    /// finish before `run` abandons in-flight connections and proceeds to ordered
    /// teardown (`HTTP_DRAIN_GRACE_MS`, default 5000ms). A process/topology knob read
    /// HERE, never by a module.
    pub http_drain_grace: std::time::Duration,
}

impl Config {
    /// Reads the standard process env (`DATABASE_URL`, `PORT`, `EDGE_ADDR`,
    /// `PLAYER_EDGE_ADDR`) into a [`Config`], applying the same defaults the Go
    /// monolith used. Both `:8080` and `8080` forms of `PORT` are accepted. The DSN
    /// is always `Some` here — a process that wants no DB calls [`Config::without_db`].
    pub fn from_env() -> Config {
        Config::from_values(
            std::env::var("DATABASE_URL").ok(),
            std::env::var("PORT").ok(),
            std::env::var("EDGE_ADDR").ok(),
            std::env::var("PLAYER_EDGE_ADDR").ok(),
            std::env::var("EDGE_DRAIN_GRACE_MS").ok(),
            std::env::var("HTTP_DRAIN_GRACE_MS").ok(),
        )
    }

    /// Drops the DB requirement, leaving everything else intact — the pure-transport
    /// `gateway-svc` uses this so [`run`] opens no pool and `/readyz` skips the DB ping.
    pub fn without_db(self) -> Config {
        Config {
            database_url: None,
            ..self
        }
    }

    /// Turns per-IP rate limiting ALWAYS on with the given `(rps, burst)` default — the
    /// gateway front door uses `20.0, 40` (Go's `cmd/gateway-svc`). Module-hosting
    /// processes leave this unset (opt-in via `RATE_LIMIT_RPS`); `RATE_LIMIT_RPS` /
    /// `RATE_LIMIT_BURST` env still override the effective values.
    pub fn with_rate_limit_default(self, rps: f64, burst: u32) -> Config {
        Config {
            rate_limit_default: Some((rps, burst)),
            ..self
        }
    }

    /// The pure core of [`Config::from_env`] — env values in, config out. Split out so
    /// the default/override logic is unit-testable without mutating process-global env.
    fn from_values(
        dsn: Option<String>,
        port: Option<String>,
        edge: Option<String>,
        player_edge: Option<String>,
        drain_grace_ms: Option<String>,
        http_drain_grace_ms: Option<String>,
    ) -> Config {
        let database_url = match dsn {
            Some(v) if !v.trim().is_empty() => v,
            _ => DEFAULT_DSN.to_string(),
        };
        let edge_addr = match edge {
            Some(v) if !v.trim().is_empty() => v.trim().to_string(),
            _ => DEFAULT_EDGE_ADDR.to_string(),
        };
        let player_edge_addr = match player_edge {
            Some(v) if !v.trim().is_empty() => v.trim().to_string(),
            _ => DEFAULT_PLAYER_EDGE_ADDR.to_string(),
        };
        // Unset/blank/unparseable falls back to the default (the env_* helpers' shape).
        let edge_drain_grace_ms = drain_grace_ms
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_EDGE_DRAIN_GRACE_MS);
        let http_drain_grace_ms = http_drain_grace_ms
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_HTTP_DRAIN_GRACE_MS);
        Config {
            database_url: Some(database_url),
            listen_addr: normalize_addr(port.as_deref().unwrap_or_default()),
            edge_addr,
            player_edge_addr,
            rate_limit_default: None,
            edge_drain_grace: std::time::Duration::from_millis(edge_drain_grace_ms),
            http_drain_grace: std::time::Duration::from_millis(http_drain_grace_ms),
        }
    }
}

/// Accepts both `:8080` and `8080` forms and returns `:8080`; empty → the default.
fn normalize_addr(port: &str) -> String {
    let port = port.trim();
    if port.is_empty() {
        return DEFAULT_LISTEN_ADDR.to_string();
    }
    if port.starts_with(':') {
        return port.to_string();
    }
    format!(":{port}")
}

/// Turns a Go-style `:PORT` bind spec into one Rust's socket APIs accept
/// (`0.0.0.0:PORT`); a fully-qualified `host:port` passes through unchanged.
fn to_bind_addr(addr: &str) -> String {
    let addr = addr.trim();
    match addr.strip_prefix(':') {
        Some(port) => format!("0.0.0.0:{port}"),
        None => addr.to_string(),
    }
}

/// Asserts every module's declared [`Module::requires`] is satisfied by a provider
/// present in this process's module set — a real module OR a remote stub (both
/// report the dependency name from [`Module::name`]). A gap is a wiring bug in the
/// entrypoint's static list, better loud at startup than a `require` panic deep in
/// Build. Kept HERE, not in `lifecycle`, because "is the process's module set
/// complete?" is a topology concern the boot layer owns.
pub fn validate_requires(modules: &[Box<dyn Module>]) -> anyhow::Result<()> {
    let present: std::collections::HashSet<&str> = modules.iter().map(|m| m.name()).collect();
    for m in modules {
        for dep in m.requires() {
            if !present.contains(dep.as_str()) {
                anyhow::bail!(
                    "module {:?} requires {:?}, but no provider is present in this process",
                    m.name(),
                    dep,
                );
            }
        }
    }
    Ok(())
}

/// Applies every [`edge::EdgeReg`] contributed to [`edge::EDGE_SLOT`] onto `server`,
/// returning how many were applied. Called by [`run`] after Build (so every module's
/// `init` has contributed) and before `listen` — and ONLY when this process actually
/// has an internal edge server; the monolith never calls it, so the contributions
/// are silently dropped there. Each registration is one-shot ([`edge::EdgeReg::apply`]
/// consumes the closure), so a re-drain cannot double-register.
fn apply_edge_registrations(ctx: &Context, server: &mut edge::Server) -> usize {
    let regs = ctx.contributions::<edge::EdgeReg>(edge::EDGE_SLOT);
    for reg in &regs {
        reg.apply(server);
    }
    regs.len()
}

/// Applies every [`httpmw::HttpLayer`] contributed to [`httpmw::LAYER_SLOT`] onto `router`,
/// in CONTRIBUTION ORDER (first contributed = innermost, last = outermost), returning the
/// wrapped router. Called by [`run`] AFTER the merged router is rate-limited, so a
/// contributed layer (the `metrics` recorder) wraps the limiter and records the `429`s it
/// issues. Each contribution is one-shot ([`httpmw::HttpLayer::apply`] consumes the
/// closure), so a re-drain cannot double-wrap. A process with no contributor (none list the
/// `metrics` module) gets the router back unchanged.
fn apply_http_layers(ctx: &Context, mut router: axum::Router) -> axum::Router {
    let layers = ctx.contributions::<httpmw::HttpLayer>(httpmw::LAYER_SLOT);
    for layer in &layers {
        router = layer.apply(router);
    }
    if !layers.is_empty() {
        tracing::info!(applied = layers.len(), "applied contributed HTTP layers");
    }
    router
}

/// Boots a service from a static list of modules. Opens the DB (when configured),
/// wires the lifecycle [`Context`], then, in EXACTLY this order:
///
/// 1. open a [`PgPool`] from `cfg.database_url` — SKIPPED when it is `None` (a
///    pure-transport process like `gateway-svc` owns no schema),
/// 2. construct the durable-events plane ([`asyncevents::Plane`]) AND the broadcast
///    cache-invalidation plane ([`invalidation::InvalidationPlane`]) iff a pool was
///    opened — both process infrastructure owned HERE, like the HTTP/edge planes,
///    never a module — then build the [`Context`]: DB-backed with the durable plane's
///    transport injected at construction ([`Context::with_db_and_transport`]) and the
///    invalidation handle swapped in ([`Context::with_invalidation`]), or DB-less (no
///    planes; any `on_tx`/`register` there is inert),
/// 3. [`validate_requires`] — fail loud on an incomplete module set,
/// 4. [`App::build`] (two-phase register → init), with the plane's worker-health
///    probe contributed to `/readyz`,
/// 5. the plane's own-schema migration, then [`App::migrate`] — the event log
///    exists before any module's first `emit_tx`,
/// 6. [`App::start`], then the durable plane's start (subscription reconcile — the
///    snapshot is taken AFTER all module inits and stub registers — pull workers,
///    NOTIFY wake-up, metrics), then the invalidation plane's start (each refresh
///    callback's first run synchronously — fail loud — then its NOTIFY listener + poll),
/// 7. if `edge_server` is `Some`, bind the internal mutual-TLS QUIC listener, and if
///    `player_server` is `Some`, bind the player-facing server-cert-only QUIC listener
///    — both AFTER build (so every handler a module registered during init exists),
///    sharing the same dev CA,
/// 8. serve HTTP (the router the modules merged into the [`Context`], plus
///    `/healthz`/`/readyz`) on `cfg.listen_addr` — `/readyz` pings the DB only when a
///    pool exists, else answers a plain 200,
/// 9. block until SIGINT (Ctrl-C — cross-platform),
/// 10. graceful shutdown: stop accepting HTTP → drain-then-close the player listener
///     ([`edge::RunningServer::shutdown`]: stop admitting new connections/streams,
///     wait up to `EDGE_DRAIN_GRACE_MS` — default 5000ms — for in-flight handlers to
///     finish, then abort stragglers) → the same drain for the internal edge listener
///     → stop the durable plane (delivery halts before anything tears down) → stop
///     the invalidation plane → drain the bus → [`App::stop`] (reverse registration
///     order). The bus drains BEFORE any module `stop`, so a stopping module never
///     emits.
///
/// `modules` is the WHOLE topology of this process — real modules plus any remote
/// stubs standing in for peers. `edge_server` is `None` for an all-local process and
/// `Some` only when this process exposes edge-backed services; `player_server` is
/// `Some` only when this process fronts external players over QUIC (the gateway).
///
/// The INTERNAL edge server is wired topology-blind: domain modules contribute
/// [`edge::EdgeReg`] registrations to [`edge::EDGE_SLOT`] unconditionally during
/// `init`, and `run` applies them here — after Build, before `listen` — iff the
/// entrypoint passed an edge server. In the monolith the contributions are simply
/// never applied; no module holds an `Option<edge::Server>` or knows the topology.
///
/// The PLAYER server is still passed as an `Arc<Mutex<…>>` — the SAME handle the
/// hosting module (`gateway::with_player_edge`) was constructed with, so its `init`
/// can install the dispatch handler onto it during Build. After Build completes,
/// `run` takes the fully-wired server out of the shared handle (via
/// `std::mem::take`, the module never touches it again) and `listen`s it.
pub async fn run(
    cfg: Config,
    modules: Vec<Box<dyn Module>>,
    edge_server: Option<Arc<Mutex<edge::Server>>>,
    player_server: Option<Arc<Mutex<edge::PlayerServer>>>,
) -> anyhow::Result<()> {
    // 1. Open the pool when a DSN is configured; a pure-transport process (no DSN)
    //    skips it. `PgPool::connect` establishes an initial connection, so an
    //    unreachable DB fails here (Go's explicit ping equivalent).
    let pool = match &cfg.database_url {
        Some(dsn) => Some(
            PgPool::connect(dsn)
                .await
                .with_context(|| format!("open db {dsn}"))?,
        ),
        None => None,
    };

    // 2. Construct the durable-events plane iff the process has a DB (DB ⇔ plane —
    //    the transport must share the caller's transaction, so it is constitutively
    //    co-hosted; a DB-less process hosts none). The plane's LISTEN connection gets
    //    the SAME authoritative DSN the pool opened — never a second env read.
    let mut plane = match (&pool, &cfg.database_url) {
        (Some(p), Some(dsn)) => Some(asyncevents::Plane::new(p.clone(), dsn.clone())?),
        _ => None,
    };

    //    The broadcast cache-invalidation plane, same DB ⇒ plane rule (a DB-less process
    //    hosts no cache consumers). It carries no durable checkpoint: it promises
    //    FRESHNESS (a committed change re-runs every registered refresh), not delivery.
    //    Its handle is injected at `Context` construction so a module's wiring-time
    //    `ctx.invalidation().register` records onto the plane; it starts after module
    //    start (the snapshot must see every registration) and stops before module stop.
    let mut invalidation = match (&pool, &cfg.database_url) {
        (Some(_), Some(dsn)) => Some(invalidation::InvalidationPlane::new(dsn.clone())),
        _ => None,
    };

    //    Wire the shared context; the same Arc is handed to App (which drives the
    //    modules) and kept here for the router + bus drain. DB-backed with the
    //    plane's transport injected AT CONSTRUCTION (so every module's wiring-time
    //    on_tx finds a live durable plane) and the invalidation handle swapped in,
    //    DB-less and plane-less otherwise.
    let ctx = Arc::new(match (pool.clone(), &plane, &invalidation) {
        (Some(p), Some(pl), Some(inv)) => {
            Context::with_db_and_transport(p, pl.transport()).with_invalidation(inv.handle())
        }
        _ => Context::new(),
    });

    // 3. Fail loud if this process's module set is internally incoherent.
    validate_requires(&modules)?;

    // 4. Two-phase Build (all registers before any init).
    let mut app = App::new(ctx.clone());
    for m in modules {
        app.add(m);
    }
    app.build().context("startup failed")?;

    // The plane's worker-health probe joins `/readyz`: a process whose pull
    // workers died (panic) must stop taking traffic that expects their effects.
    if let Some(p) = &plane {
        let liveness = p.liveness();
        ctx.contribute(
            httpmw::READINESS_SLOT,
            httpmw::ReadyCheck::new("asyncevents-worker", move || {
                let liveness = liveness.clone();
                async move {
                    if liveness.dead() {
                        Err("asyncevents worker task died".to_string())
                    } else {
                        Ok(())
                    }
                }
            }),
        );
    }

    // The invalidation plane's freshness probe joins `/readyz`: a cache whose refresh
    // callback has not succeeded for 60s is stale, so the process stops taking traffic
    // that expects fresh reads. (Before `start` seeds the clock the set is empty ⇒ ready,
    // but HTTP serves only after `start`, so that window is never observable.)
    if let Some(p) = &invalidation {
        // Register the plane's series into the process's private registry (it can't
        // depend on core/metrics itself — see the plane's Cargo.toml).
        for collector in p.collectors() {
            let _ = metrics::register(collector);
        }
        let health = p.readiness();
        ctx.contribute(
            httpmw::READINESS_SLOT,
            httpmw::ReadyCheck::new("invalidation", move || {
                let health = health.clone();
                async move {
                    let stale = health.stale(invalidation::STALE_AFTER);
                    if stale.is_empty() {
                        Ok(())
                    } else {
                        Err(format!("invalidation callbacks stale >60s: {}", stale.join(", ")))
                    }
                }
            }),
        );
    }

    // 5. Own-schema migrations — the plane's first (a module's first emit_tx must
    //    find `asyncevents.append_event`), then 6. background work: modules first,
    //    then the plane (its subscription snapshot must see every wiring-time on_tx).
    //
    //    Everything from here through the end of HTTP serve is fallible and runs
    //    inside ONE block: its `Err` falls through to the SAME ordered teardown the
    //    happy path uses (see [`ordered_teardown`]), truncated to what was actually
    //    created/started, so a partial startup never strands started modules, plane
    //    workers, the scheduler's advisory lock, or an open listener. Listener
    //    handles land in these outer slots the moment they exist so a LATER failure
    //    still closes them.
    let mut running_edge: Option<edge::RunningServer> = None;
    let mut running_player: Option<edge::RunningServer> = None;
    let mut modules_started = false;
    let outcome: anyhow::Result<()> = async {
        if let Some(p) = &plane {
            p.migrate().await.context("asyncevents migrate failed")?;
        }
        app.migrate().await.context("migrate failed")?;
        // Double-stop rule: on Err, `App::start` has ALREADY stopped its started
        // prefix internally, so the unwind must skip `app.stop()` (which would run
        // `stop` on never-started modules) — `modules_started` stays false. A
        // migrate failure above skips it too (nothing started); only a failure
        // AFTER this line unwinds with `app.stop()`.
        app.start().await.context("start failed")?;
        modules_started = true;
        if let Some(p) = &mut plane {
            p.start().await.context("asyncevents start failed")?;
        }
        // After module start (so the snapshot sees every wiring-time `register`): run
        // each callback's FIRST refresh synchronously — a failure fails startup loudly —
        // then launch the NOTIFY listener + poll fallback.
        if let Some(p) = &mut invalidation {
            p.start().await.context("invalidation start failed")?;
        }

        // 7. Bring up the shared edge server AFTER every module init has contributed
        //    its registrations. One listener, all edge methods, mutual TLS via the
        //    shared dev CA. This is where the EDGE_SLOT contributions land: modules
        //    contributed unconditionally during init; only a process that actually has
        //    an edge server applies them (the monolith reaches the `None` arm and they
        //    are dropped).
        running_edge = match edge_server {
            Some(shared) => {
                // Take the server out of the shared handle (`mem::take` leaves an empty
                // one behind), then install every contributed registration on it.
                let mut server = std::mem::take(&mut *shared.lock().unwrap());
                let applied = apply_edge_registrations(&ctx, &mut server);
                tracing::info!(applied, "installed contributed edge registrations");
                let ca = edge::shared_dev_ca().context("edge ca")?;
                let edge_bind: SocketAddr = to_bind_addr(&cfg.edge_addr)
                    .parse()
                    .with_context(|| format!("parse edge addr {:?}", cfg.edge_addr))?;
                let running = server.listen(edge_bind, &ca).context("edge listen")?;
                tracing::info!(addr = %running.local_addr(), "edge listening (mutual TLS)");
                Some(running)
            }
            None => None,
        };

        // 7b. Bring up the player-facing QUIC front, same lifecycle as the internal
        //     edge: the gateway registered its dispatch handler onto this shared handle
        //     during Build, so `mem::take` hands `listen` the fully-wired server.
        //     Server-cert-only TLS (no client cert) off the SAME dev CA —
        //     `server_tls_public` derives from it — so external players can handshake;
        //     per-call auth is the front's job.
        running_player = match player_server {
            Some(shared) => {
                let player = std::mem::take(&mut *shared.lock().unwrap());
                let ca = edge::shared_dev_ca().context("edge ca")?;
                let player_bind: SocketAddr = to_bind_addr(&cfg.player_edge_addr)
                    .parse()
                    .with_context(|| format!("parse player edge addr {:?}", cfg.player_edge_addr))?;
                let running = player.listen(player_bind, &ca).context("player edge listen")?;
                tracing::info!(addr = %running.local_addr(), "player edge listening (server-cert-only TLS)");
                Some(running)
            }
            None => None,
        };

        // 8. Serve HTTP: the router the modules merged into the Context, plus liveness
        //    (`/healthz`, always 200 — a restart can't fix a down DB) and readiness
        //    (`/readyz`). Readiness pings the DB when a pool exists (controls whether a
        //    load balancer sends traffic); a DB-less process has nothing to ping, so it
        //    answers a plain 200. Modules must not themselves mount these two routes (axum
        //    `merge`/`route` panics on a duplicate, exactly like Go's ServeMux).
        // `/readyz` folds in the baseline DB ping (when a pool exists) PLUS every
        // `httpmw::ReadyCheck` a module contributed to `READINESS_SLOT` — read lazily, per
        // request, so by request time every module's `init` (where checks are contributed)
        // has run. Any failure → 503 with a per-failed-check JSON body (Go's readyzHandler).
        let ready_pool = pool.clone();
        let ready_ctx = ctx.clone();
        let router = ctx
            .take_router()
            .route("/healthz", get(|| async { "ok" }))
            .route(
                "/readyz",
                get(move || {
                    let pool = ready_pool.clone();
                    let ctx = ready_ctx.clone();
                    async move {
                        let checks =
                            ctx.contributions::<httpmw::ReadyCheck>(httpmw::READINESS_SLOT);
                        readyz_response(pool.as_ref(), checks).await
                    }
                }),
            );

        // Rate limiting (Step 13): OPT-IN for module hosts (`RATE_LIMIT_RPS` default 0 = off —
        // a split peer runs BEHIND the gateway, so limiting here would double-count and
        // collapse every client into the gateway's single bucket), ALWAYS on for the gateway
        // front door (`Config::with_rate_limit_default(20, 40)`). Layered UNDER the metrics
        // layer below so a 429 the limiter issues is still counted (Go's
        // `metrics(ratelimit(mux))` — the last `.layer` added is the outermost). Skips
        // `/healthz|/readyz|/metrics` (`httpmw::skip_infra`); keys per resolved client IP
        // (trust-aware XFF walk over `TRUSTED_PROXY_CIDRS`). The QUIC planes are NOT wrapped —
        // rate limiting is an HTTP-plane concern (Go parity).
        let (default_rps, default_burst) = cfg.rate_limit_default.unwrap_or((0.0, 40));
        let rps = env_f64("RATE_LIMIT_RPS").unwrap_or(default_rps);
        let burst = env_u32("RATE_LIMIT_BURST").unwrap_or(default_burst);
        let router = if rps > 0.0 {
            let trusted =
                httpmw::parse_cidrs(&std::env::var("TRUSTED_PROXY_CIDRS").unwrap_or_default())
                    .map_err(|e| anyhow::anyhow!("parse TRUSTED_PROXY_CIDRS: {e}"))?;
            let limiter = httpmw::IpLimiter::new(rps, burst);
            limiter.spawn_eviction();
            tracing::info!(rps, burst, trusted_cidrs = trusted.len(), "http rate limiting enabled");
            httpmw::mount(router, limiter, Arc::new(trusted))
        } else {
            tracing::info!("http rate limiting disabled (RATE_LIMIT_RPS<=0; expected behind the gateway)");
            router
        };

        // Apply every contributed HTTP layer (`httpmw::LAYER_SLOT`) LAST, over the whole
        // rate-limited surface, in contribution order. This is where the `metrics` module's
        // recording layer lands (it also mounted `GET /metrics` during init): applied AFTER the
        // rate limiter, it wraps it, so a `429` the limiter issues is still recorded — Go's
        // `metrics(ratelimit(mux))`. A process serves `/metrics` iff it listed the `metrics`
        // module (was `Config::without_metrics`, now module presence). The layer labels each
        // request by its MATCHED route pattern and exempts the infra endpoints (see
        // `core/metrics`). The QUIC planes are NOT wrapped (HTTP-plane concern).
        let router = apply_http_layers(&ctx, router);

        let bind = to_bind_addr(&cfg.listen_addr);
        let listener = tokio::net::TcpListener::bind(&bind)
            .await
            .with_context(|| format!("bind http {bind}"))?;
        tracing::info!(addr = %bind, "listening");

        // 9. `with_graceful_shutdown` returns once the shutdown signal fires AND in-flight
        //    HTTP has drained — so the await below IS "stop accepting HTTP". Serve WITH
        //    connection info so the gateway's passthrough can set `X-Forwarded-For` (and
        //    Step 13's rate limiter can key per client IP); handlers that don't need it
        //    ignore it.
        //
        //    The signal fans out over a `watch` (NOT a `Notify`): two consumers wait on it
        //    — the graceful-shutdown future AND the drain-grace timer below — and it can
        //    fire before either starts awaiting. `watch` is level-triggered and
        //    multi-receiver, so a signal that flipped `true` early is still observed by a
        //    receiver that only starts `wait_for` afterwards. A `Notify` would lose the
        //    wake-up (at most one stored permit, `notify_waiters` no-ops with no waiter).
        let (sig_tx, sig_rx) = tokio::sync::watch::channel(false);
        tokio::spawn(async move {
            shutdown_signal().await;
            let _ = sig_tx.send(true);
        });
        let graceful_rx = sig_rx.clone();
        let serve_fut = axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            // `wait_for(|v| *v)` returns immediately if the signal already flipped, else
            // awaits the flip — covering the fire-before-serve-starts race either way.
            let mut rx = graceful_rx;
            let _ = rx.wait_for(|v| *v).await;
        });

        // The HTTP drain is time-bounded: once the signal fires, `with_graceful_shutdown`
        // gets `http_drain_grace` to finish draining in-flight connections. A hung
        // connection can no longer stall shutdown forever before the (already time-bounded)
        // QUIC drain and module stop begin — the timeout arm abandons the drain and lets
        // `ordered_teardown` proceed. The timer only STARTS after the signal, so normal
        // serving is unaffected.
        tokio::select! {
            r = serve_fut => r.context("http serve"),
            _ = async {
                let mut rx = sig_rx;
                let _ = rx.wait_for(|v| *v).await;
                tokio::time::sleep(cfg.http_drain_grace).await;
            } => {
                tracing::warn!("http drain grace expired; abandoning in-flight connections");
                Ok(())
            }
        }
    }
    .await;

    // 10. Ordered teardown — one sequence for BOTH graceful shutdown and startup
    //     failure (see [`ordered_teardown`] for the order and its rationale).
    match &outcome {
        Ok(()) => tracing::info!("shutting down"),
        Err(err) => tracing::error!(err = %format!("{err:#}"), "startup failed; unwinding"),
    }
    ordered_teardown(
        running_player.take(),
        running_edge.take(),
        cfg.edge_drain_grace,
        &mut plane,
        &mut invalidation,
        &ctx,
        modules_started.then_some(&app),
    )
    .await;
    outcome?;
    tracing::info!("bye");
    Ok(())
}

/// Ordered teardown shared by graceful shutdown and every startup-failure unwind:
/// drain-then-close the player front (external players drain first — REAL drain:
/// [`edge::RunningServer::shutdown`] stops admitting new connections/streams, waits
/// up to `grace` for in-flight handlers to finish, then aborts stragglers) → the
/// same drain for the internal edge → stop the durable plane (delivery halts before
/// ANY module tears down — structurally what the old "messaging registers last,
/// stops first" convention hand-ordered) → stop the invalidation plane → drain the
/// in-process bus → stop modules (reverse registration order, inside `App::stop`).
///
/// The same order serves ANY prefix of startup because every step degrades to a no-op
/// for what was never created/started: listeners drain only when `Some` (an idle
/// listener short-circuits, so the unwind path pays ~0 and needs no special-casing),
/// both planes' `stop` are `Option::take`-guarded (idempotent), `Bus::close` is
/// idempotent, and `app` is `None` when no module `start` succeeded — after an
/// `App::start` failure the started prefix was already stopped INSIDE `App::start`,
/// and running `App::stop` here would call `stop` on never-started modules (outside
/// the `Module` contract).
async fn ordered_teardown(
    running_player: Option<edge::RunningServer>,
    running_edge: Option<edge::RunningServer>,
    grace: std::time::Duration,
    plane: &mut Option<asyncevents::Plane>,
    invalidation: &mut Option<invalidation::InvalidationPlane>,
    ctx: &Context,
    app: Option<&App>,
) {
    if let Some(running) = running_player {
        running.shutdown(grace).await;
    }
    if let Some(running) = running_edge {
        running.shutdown(grace).await;
    }
    if let Some(p) = plane {
        p.stop().await;
    }
    if let Some(p) = invalidation {
        p.stop().await;
    }
    ctx.bus().close().await;
    if let Some(app) = app {
        app.stop().await;
    }
}

/// Builds the `/readyz` response: the baseline DB ping (when a pool exists) plus every
/// contributed [`httpmw::ReadyCheck`]. All green → `200 ok`; any failure → `503` with a
/// JSON body mapping each FAILED check's name to its error string (Go's `readyzHandler`
/// shape, refined to named checks instead of `readiness[i]` indices). Kept as a free
/// function so it is unit-testable without a live DB (pass `None` + failing checks).
async fn readyz_response(
    pool: Option<&PgPool>,
    checks: Vec<httpmw::ReadyCheck>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let mut failures: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    if let Some(pool) = pool {
        if let Err(err) = sqlx::query("SELECT 1").execute(pool).await {
            tracing::warn!(%err, "readyz db check failed");
            failures.insert("db".to_string(), err.to_string());
        }
    }
    for check in checks {
        if let Err(err) = check.run().await {
            failures.insert(check.name().to_string(), err);
        }
    }
    if failures.is_empty() {
        (StatusCode::OK, "ok").into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, axum::Json(failures)).into_response()
    }
}

/// Reads `key` as an `f64`, or `None` when unset/blank/unparseable — the caller supplies
/// the default (Go's `envFloat` shape, split so the default is explicit at each site).
fn env_f64(key: &str) -> Option<f64> {
    let v = std::env::var(key).ok()?;
    let v = v.trim();
    if v.is_empty() {
        return None;
    }
    v.parse().ok()
}

/// Reads `key` as a `u32`, or `None` when unset/blank/unparseable (Go's `envInt`).
fn env_u32(key: &str) -> Option<u32> {
    let v = std::env::var(key).ok()?;
    let v = v.trim();
    if v.is_empty() {
        return None;
    }
    v.parse().ok()
}

/// Resolves once a shutdown signal is received, so the same graceful path (HTTP
/// drain, QUIC drain, plane halt, ordered module stop) runs for `kill`/systemd/
/// container stops as for an interactive Ctrl-C.
///
/// - unix: SIGINT (Ctrl-C) *or* SIGTERM (the default `kill`, `docker stop`,
///   `systemctl stop`).
/// - windows: Ctrl-C, `CTRL_CLOSE_EVENT` (console window closed), or
///   `CTRL_SHUTDOWN_EVENT` (system shutdown).
///
/// A failure to install a handler is logged and treated as "shut down".
///
/// **Windows limitation:** `Stop-Process -Force` / `taskkill /F` map to
/// `TerminateProcess` — no console control event ever reaches the process, so no
/// graceful path can run for a forced kill. A graceful stop on Windows needs a
/// *non-forced* console event (e.g. interactive Ctrl-C). The repo scripts
/// (`run.ps1`, `split-proof.ps1`) stay hard-kill by design; this is a documented
/// limitation, not a bug in this handler.
#[cfg(unix)]
async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};

    let mut term = match signal(SignalKind::terminate()) {
        Ok(term) => term,
        Err(err) => {
            tracing::error!(%err, "failed to listen for SIGTERM; shutting down");
            return;
        }
    };

    tokio::select! {
        r = tokio::signal::ctrl_c() => {
            if let Err(err) = r {
                tracing::error!(%err, "failed to listen for ctrl-c; shutting down");
            }
        }
        _ = term.recv() => {}
    }
}

/// See the unix variant for the shared contract and the `taskkill /F` limitation.
#[cfg(windows)]
async fn shutdown_signal() {
    use tokio::signal::windows::{ctrl_close, ctrl_shutdown};

    let mut close = match ctrl_close() {
        Ok(close) => close,
        Err(err) => {
            tracing::error!(%err, "failed to listen for CTRL_CLOSE_EVENT; shutting down");
            return;
        }
    };
    let mut shutdown = match ctrl_shutdown() {
        Ok(shutdown) => shutdown,
        Err(err) => {
            tracing::error!(%err, "failed to listen for CTRL_SHUTDOWN_EVENT; shutting down");
            return;
        }
    };

    tokio::select! {
        r = tokio::signal::ctrl_c() => {
            if let Err(err) = r {
                tracing::error!(%err, "failed to listen for ctrl-c; shutting down");
            }
        }
        _ = close.recv() => {}
        _ = shutdown.recv() => {}
    }
}

#[cfg(test)]
mod tests;
