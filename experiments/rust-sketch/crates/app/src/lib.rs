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
use std::sync::Arc;

use anyhow::Context as _;
use axum::http::StatusCode;
use axum::routing::get;
use lifecycle::{App, Context, Module};
use sqlx::PgPool;

const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";
const DEFAULT_LISTEN_ADDR: &str = ":8080";
const DEFAULT_EDGE_ADDR: &str = ":9000";

/// The process-level configuration [`run`] needs. Deliberately tiny: everything
/// module-specific (event subscribers, peer edge addrs, admin URLs, …) is read by
/// the module that owns it, not here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Postgres DSN (`DATABASE_URL`).
    pub database_url: String,
    /// HTTP listen address, e.g. `:8080` (`PORT`).
    pub listen_addr: String,
    /// QUIC edge listen address, e.g. `:9000` (`EDGE_ADDR`) — only used when an edge
    /// server is passed to [`run`].
    pub edge_addr: String,
}

impl Config {
    /// Reads the standard process env (`DATABASE_URL`, `PORT`, `EDGE_ADDR`) into a
    /// [`Config`], applying the same defaults the Go monolith used. Both `:8080` and
    /// `8080` forms of `PORT` are accepted.
    pub fn from_env() -> Config {
        Config::from_values(
            std::env::var("DATABASE_URL").ok(),
            std::env::var("PORT").ok(),
            std::env::var("EDGE_ADDR").ok(),
        )
    }

    /// The pure core of [`Config::from_env`] — env values in, config out. Split out so
    /// the default/override logic is unit-testable without mutating process-global env.
    fn from_values(dsn: Option<String>, port: Option<String>, edge: Option<String>) -> Config {
        let database_url = match dsn {
            Some(v) if !v.trim().is_empty() => v,
            _ => DEFAULT_DSN.to_string(),
        };
        let edge_addr = match edge {
            Some(v) if !v.trim().is_empty() => v.trim().to_string(),
            _ => DEFAULT_EDGE_ADDR.to_string(),
        };
        Config {
            database_url,
            listen_addr: normalize_addr(port.as_deref().unwrap_or_default()),
            edge_addr,
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

/// Boots a service from a static list of modules. Opens the DB, wires the lifecycle
/// [`Context`], then, in EXACTLY this order (port of Go's `app.Run`):
///
/// 1. open a [`PgPool`] from `cfg.database_url`,
/// 2. build the [`Context`] backed by that pool,
/// 3. [`validate_requires`] — fail loud on an incomplete module set,
/// 4. [`App::build`] (two-phase register → init),
/// 5. [`App::migrate`],
/// 6. [`App::start`],
/// 7. if `edge_server` is `Some`, bind the QUIC listener AFTER build (so every
///    handler a module registered during init exists) using the shared dev CA,
/// 8. serve HTTP (the router the modules merged into the [`Context`], plus
///    `/healthz`/`/readyz`) on `cfg.listen_addr`,
/// 9. block until SIGINT (Ctrl-C — cross-platform),
/// 10. graceful shutdown in Go's order: stop accepting HTTP → close the edge
///     listener → drain the bus → [`App::stop`] (reverse registration order). The bus
///     drains BEFORE any module `stop`, so a stopping module never emits.
///
/// `modules` is the WHOLE topology of this process — real modules plus any remote
/// stubs standing in for peers. `edge_server` is `None` for an all-local process and
/// `Some` only when this process exposes edge-backed services.
pub async fn run(
    cfg: Config,
    modules: Vec<Box<dyn Module>>,
    edge_server: Option<edge::Server>,
) -> anyhow::Result<()> {
    // 1. Open the pool. `PgPool::connect` establishes an initial connection, so an
    //    unreachable DB fails here (Go's explicit ping equivalent).
    let pool = PgPool::connect(&cfg.database_url)
        .await
        .with_context(|| format!("open db {}", cfg.database_url))?;

    // 2. Wire the shared context; the same Arc is handed to App (which drives the
    //    modules) and kept here for the router + bus drain.
    let ctx = Arc::new(Context::with_db(pool.clone()));

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

    // 7. Bring up the shared edge server AFTER every module init has registered its
    //    handlers. One listener, all edge methods, mutual TLS via the shared dev CA.
    let running_edge = match edge_server {
        Some(server) => {
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

    // 8. Serve HTTP: the router the modules merged into the Context, plus liveness
    //    (`/healthz`, always 200 — a restart can't fix a down DB) and readiness
    //    (`/readyz`, DB-ping gated — controls whether a load balancer sends traffic).
    //    Modules must not themselves mount these two routes (axum `merge`/`route`
    //    panics on a duplicate, exactly like Go's ServeMux).
    let ready_pool = pool.clone();
    let router = ctx
        .take_router()
        .route("/healthz", get(|| async { "ok" }))
        .route(
            "/readyz",
            get(move || {
                let pool = ready_pool.clone();
                async move {
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
    //    drained — so the await below IS "stop accepting HTTP".
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("http serve")?;

    // 10. Ordered teardown: HTTP already stopped → close edge → drain bus → stop
    //     modules (reverse registration order, inside App::stop).
    tracing::info!("shutting down");
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
mod tests {
    use super::*;
    use lifecycle::Context;

    /// A minimal module for the topology tests: a name + a requires manifest. A
    /// "remote stub" is indistinguishable here — it too just reports a name.
    struct Fake {
        name: String,
        requires: Vec<String>,
    }

    impl Fake {
        fn boxed(name: &str, requires: &[&str]) -> Box<dyn Module> {
            Box::new(Fake {
                name: name.to_string(),
                requires: requires.iter().map(|s| s.to_string()).collect(),
            })
        }
    }

    #[async_trait::async_trait]
    impl Module for Fake {
        fn name(&self) -> &str {
            &self.name
        }
        fn requires(&self) -> Vec<String> {
            self.requires.clone()
        }
        fn init(&self, _ctx: &Context) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn validate_requires_passes_when_provider_present() {
        let mods = vec![
            Fake::boxed("characters", &[]),
            Fake::boxed("inventory", &["characters"]),
        ];
        validate_requires(&mods).unwrap();
    }

    #[test]
    fn validate_requires_fails_when_provider_absent() {
        let mods = vec![Fake::boxed("inventory", &["characters"])];
        let err = validate_requires(&mods).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("\"inventory\""), "{msg}");
        assert!(msg.contains("\"characters\""), "{msg}");
        assert!(msg.contains("no provider is present"), "{msg}");
    }

    #[test]
    fn validate_requires_satisfied_by_remote_stub() {
        // The provider is a name-only stand-in (as `remote::Stub` will be) reporting
        // the provider's name — the name-based check can't tell it from a real module.
        let mods = vec![
            Fake::boxed("characters", &[]), // stub for a peer's `characters`
            Fake::boxed("inventory", &["characters"]),
        ];
        validate_requires(&mods).unwrap();
    }

    #[test]
    fn config_defaults_when_env_absent() {
        let cfg = Config::from_values(None, None, None);
        assert_eq!(cfg.database_url, DEFAULT_DSN);
        assert_eq!(cfg.listen_addr, ":8080");
        assert_eq!(cfg.edge_addr, ":9000");
    }

    #[test]
    fn config_defaults_when_env_blank() {
        let cfg = Config::from_values(Some("  ".into()), Some("".into()), Some("   ".into()));
        assert_eq!(cfg.database_url, DEFAULT_DSN);
        assert_eq!(cfg.listen_addr, ":8080");
        assert_eq!(cfg.edge_addr, ":9000");
    }

    #[test]
    fn config_overrides_from_env() {
        let cfg = Config::from_values(
            Some("postgres://u:p@db:5432/x".into()),
            Some("9090".into()),
            Some(":9001".into()),
        );
        assert_eq!(cfg.database_url, "postgres://u:p@db:5432/x");
        // Bare port gets the leading colon (Go's normalizeAddr).
        assert_eq!(cfg.listen_addr, ":9090");
        assert_eq!(cfg.edge_addr, ":9001");
    }

    #[test]
    fn config_accepts_colon_port_form() {
        let cfg = Config::from_values(None, Some(":8081".into()), None);
        assert_eq!(cfg.listen_addr, ":8081");
    }

    #[test]
    fn to_bind_addr_expands_colon_port() {
        assert_eq!(to_bind_addr(":9000"), "0.0.0.0:9000");
        assert_eq!(to_bind_addr("127.0.0.1:9000"), "127.0.0.1:9000");
    }
}
