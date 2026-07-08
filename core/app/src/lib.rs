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

/// The process-level configuration [`run`] needs. Deliberately tiny: everything
/// module-specific (event subscribers, peer edge addrs, admin URLs, …) is read by
/// the module that owns it, not here.
#[derive(Debug, Clone, PartialEq, Eq)]
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

    /// The pure core of [`Config::from_env`] — env values in, config out. Split out so
    /// the default/override logic is unit-testable without mutating process-global env.
    fn from_values(
        dsn: Option<String>,
        port: Option<String>,
        edge: Option<String>,
        player_edge: Option<String>,
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
        Config {
            database_url: Some(database_url),
            listen_addr: normalize_addr(port.as_deref().unwrap_or_default()),
            edge_addr,
            player_edge_addr,
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

/// Boots a service from a static list of modules. Opens the DB (when configured),
/// wires the lifecycle [`Context`], then, in EXACTLY this order (port of Go's
/// `app.Run`):
///
/// 1. open a [`PgPool`] from `cfg.database_url` — SKIPPED when it is `None` (a
///    pure-transport process like `gateway-svc` owns no schema),
/// 2. build the [`Context`] backed by that pool (or a DB-less one when there is none),
/// 3. [`validate_requires`] — fail loud on an incomplete module set,
/// 4. [`App::build`] (two-phase register → init),
/// 5. [`App::migrate`],
/// 6. [`App::start`],
/// 7. if `edge_server` is `Some`, bind the internal mutual-TLS QUIC listener, and if
///    `player_server` is `Some`, bind the player-facing server-cert-only QUIC listener
///    — both AFTER build (so every handler a module registered during init exists),
///    sharing the same dev CA,
/// 8. serve HTTP (the router the modules merged into the [`Context`], plus
///    `/healthz`/`/readyz`) on `cfg.listen_addr` — `/readyz` pings the DB only when a
///    pool exists, else answers a plain 200,
/// 9. block until SIGINT (Ctrl-C — cross-platform),
/// 10. graceful shutdown in Go's order: stop accepting HTTP → close the player
///     listener (players drain first) → close the internal edge listener → drain the
///     bus → [`App::stop`] (reverse registration order). The bus drains BEFORE any
///     module `stop`, so a stopping module never emits.
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

    // 2. Wire the shared context; the same Arc is handed to App (which drives the
    //    modules) and kept here for the router + bus drain. DB-backed when a pool was
    //    opened, DB-less otherwise (`lifecycle::Context` supports both).
    let ctx = Arc::new(match pool.clone() {
        Some(p) => Context::with_db(p),
        None => Context::new(),
    });

    // 3. Fail loud if this process's module set is internally incoherent.
    validate_requires(&modules)?;

    // 4. Two-phase Build (all registers before any init).
    let mut app = App::new(ctx.clone());
    for m in modules {
        app.add(m);
    }
    app.build().context("startup failed")?;

    // 5. Own-schema migrations, then 6. background work.
    app.migrate().await.context("migrate failed")?;
    app.start().await.context("start failed")?;

    // 7. Bring up the shared edge server AFTER every module init has contributed its
    //    registrations. One listener, all edge methods, mutual TLS via the shared dev
    //    CA. This is where the EDGE_SLOT contributions land: modules contributed
    //    unconditionally during init; only a process that actually has an edge server
    //    applies them (the monolith reaches the `None` arm and they are dropped).
    let running_edge = match edge_server {
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

    // 7b. Bring up the player-facing QUIC front, same lifecycle as the internal edge:
    //     the gateway registered its dispatch handler onto this shared handle during
    //     Build, so `mem::take` hands `listen` the fully-wired server. Server-cert-only
    //     TLS (no client cert) off the SAME dev CA — `server_tls_public` derives from
    //     it — so external players can handshake; per-call auth is the front's job.
    let running_player = match player_server {
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
    let ready_pool = pool.clone();
    let router = ctx
        .take_router()
        .route("/healthz", get(|| async { "ok" }))
        .route(
            "/readyz",
            get(move || {
                let pool = ready_pool.clone();
                async move {
                    let Some(pool) = pool else {
                        return (StatusCode::OK, "ok");
                    };
                    match sqlx::query("SELECT 1").execute(&pool).await {
                        Ok(_) => (StatusCode::OK, "ok"),
                        Err(err) => {
                            tracing::warn!(%err, "readyz db check failed");
                            (StatusCode::SERVICE_UNAVAILABLE, "db unavailable")
                        }
                    }
                }
            }),
        );

    let bind = to_bind_addr(&cfg.listen_addr);
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .with_context(|| format!("bind http {bind}"))?;
    tracing::info!(addr = %bind, "listening");

    // 9. `with_graceful_shutdown` returns once SIGINT fires AND in-flight HTTP has
    //    drained — so the await below IS "stop accepting HTTP". Serve WITH connection
    //    info so the gateway's passthrough can set `X-Forwarded-For` (and Step 13's
    //    rate limiter can key per client IP); handlers that don't need it ignore it.
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("http serve")?;

    // 10. Ordered teardown: HTTP already stopped → close the player front (external
    //     players drain first) → close the internal edge → drain bus → stop modules
    //     (reverse registration order, inside App::stop).
    tracing::info!("shutting down");
    if let Some(running) = running_player {
        running.close();
    }
    if let Some(running) = running_edge {
        running.close();
    }
    ctx.bus().close().await;
    app.stop().await;
    tracing::info!("bye");
    Ok(())
}

/// Resolves once SIGINT (Ctrl-C) is received. Cross-platform (this repo runs on
/// Windows); a failure to install the handler is logged and treated as "shut down".
async fn shutdown_signal() {
    if let Err(err) = tokio::signal::ctrl_c().await {
        tracing::error!(%err, "failed to listen for ctrl-c; shutting down");
    }
}

#[cfg(test)]
mod tests;
