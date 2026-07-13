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
const DEFAULT_MODULE_STOP_GRACE_MS: u64 = 5000;
/// Whole-request inbound HTTP timeout (`HTTP_REQUEST_TIMEOUT_MS`, round 4 finding 3):
/// bounds request-received → response-started for every process. Default ON at 30s
/// (aligned with the proxy's `PROXY_READ_TIMEOUT`); explicit `0` disables the layer.
/// A process/topology knob read HERE in `core/app`, never by a module.
const DEFAULT_HTTP_REQUEST_TIMEOUT_MS: u64 = 30000;
/// Global cap on concurrent player-QUIC connections (`PLAYER_MAX_CONNS`) — the public
/// front faces untrusted, certless peers, so the accept loop must bound its
/// task-per-connection fan-out. `core/app` owns the env surface; `core/edge` stays
/// env-blind and receives the value via `PlayerServer::with_conn_limits`.
const DEFAULT_PLAYER_MAX_CONNS: usize = 1024;
/// Per-source-IP cap on concurrent player-QUIC connections (`PLAYER_MAX_CONNS_PER_IP`)
/// — a tighter bound so one abusive peer cannot consume the whole global budget.
const DEFAULT_PLAYER_MAX_CONNS_PER_IP: usize = 32;
const DEFAULT_PLAYER_RATE_LIMIT_RPS: f64 = 20.0;
const DEFAULT_PLAYER_RATE_LIMIT_BURST: u32 = 40;
const DEFAULT_PLAYER_CONN_RATE_LIMIT_RPS: f64 = 10.0;
const DEFAULT_PLAYER_CONN_RATE_LIMIT_BURST: u32 = 20;

/// Whether an explicit zero rate disables a limiter or is invalid for an always-on
/// surface. Kept at the process configuration boundary so the limiter implementations
/// receive only finite, non-negative values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RateZeroPolicy {
    Allow,
    Reject,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RateParseError {
    Malformed,
    NonFinite,
    Negative,
    ZeroRejected,
}

impl std::fmt::Display for RateParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reason = match self {
            RateParseError::Malformed => "not a number",
            RateParseError::NonFinite => "not finite",
            RateParseError::Negative => "negative",
            RateParseError::ZeroRejected => "zero is not allowed",
        };
        f.write_str(reason)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ResolvedRate {
    value: f64,
    invalid: Option<RateParseError>,
}

/// Per-check budget for `/readyz` (the baseline DB ping AND each contributed
/// `httpmw::ReadyCheck`, individually) — no env knob, a readiness-probe budget is not a
/// tuning surface (config-as-code/anti-magic). LB probe timeouts are typically 5-10s, so
/// 2s per check keeps even DB-ping + a couple of checks comfortably under budget.
/// Deliberate trade-off: the bound on the DB ping INCLUDES pool-acquire wait, so a
/// pool-saturation spike now yields a fast 503 (instance pulled from rotation while busy)
/// instead of waiting out the contention — for a readiness probe, "busy to the point of a
/// 2s acquire wait" IS not-ready. Chosen, not accidental.
const READY_CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// `/readyz` flags the durable plane when no worker completed a healthy pass for
/// this long — ~30 ticks of the workers' 1s poll floor plus slack for a slow
/// pass, so a transient error never flaps readiness.
const DELIVERY_STALL_MAX: std::time::Duration = std::time::Duration::from_secs(30);

fn retention_stall_message(stall_after: std::time::Duration) -> String {
    format!("asyncevents retention sweep has not succeeded in >{stall_after:?}")
}

/// Freezes readiness membership after every wiring-time contributor has run. The
/// cloned checks still execute their live closures on every request.
fn snapshot_readiness_checks(ctx: &Context) -> Vec<httpmw::ReadyCheck> {
    ctx.contributions(httpmw::READINESS_SLOT)
}

/// How the HTTP front terminates TLS (admin hardening Step 4). The MECHANISM lives
/// here in `core/app` (the serve path owns the listener); the ENV PARSING lives in the
/// one composition root that fronts the public internet (`cmd/gateway-svc` reads
/// `TLS_MODE`/`TLS_CERT_PATH`/… and calls [`Config::with_tls`]) — modules see nothing,
/// and no other `cmd/*` main is TLS-aware today (single public front door).
#[derive(Debug, Clone, PartialEq)]
pub enum TlsFront {
    /// Serve HTTPS from an operator-provided PEM cert chain + private key.
    Files {
        cert: std::path::PathBuf,
        key: std::path::PathBuf,
    },
    /// Serve HTTPS with certificates obtained/renewed automatically from Let's
    /// Encrypt via rustls-acme (TLS-ALPN-01 — no port-80 listener needed). Certs and
    /// the account key persist in `cache_dir` across restarts.
    Acme {
        /// Domains on the certificate (SANs) — TLS-ALPN-01 is answered inline on the
        /// HTTPS listener itself, so each must resolve to this host.
        domains: Vec<String>,
        /// Directory for the ACME account + issued-cert cache (created if absent).
        cache_dir: std::path::PathBuf,
        /// Optional operator contact (an email address; rendered as `mailto:`).
        contact: Option<String>,
    },
}

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
    /// Deadline for any SINGLE module's `stop` during ordered teardown (and the
    /// start-unwind path) before it is abandoned and teardown continues to the next
    /// module (`MODULE_STOP_GRACE_MS`, default 5000ms). A process/topology knob read
    /// HERE, never by a module — applied via [`App::with_stop_grace`].
    pub module_stop_grace: std::time::Duration,
    /// Whole-request inbound HTTP timeout applied to the served router (round 4,
    /// finding 3). `Some(d)` wraps the surface in a `tower_http::timeout::TimeoutLayer`
    /// that emits **408** once `d` elapses before the response starts; `None` disables
    /// the layer. Default `Some(30s)` for every process (`HTTP_REQUEST_TIMEOUT_MS`,
    /// `0` → `None`), overridable by [`Config::with_request_timeout_default`]. A
    /// process/topology knob read HERE, never by a module.
    pub http_request_timeout: Option<std::time::Duration>,
    /// TLS termination for the HTTP front. `None` (the [`Config::from_env`] default)
    /// serves plain HTTP exactly as before; `Some` switches the serve path to
    /// axum-server over rustls ([`TlsFront::Files`]) or rustls-acme
    /// ([`TlsFront::Acme`]). Set ONLY via [`Config::with_tls`] by a composition root
    /// that parsed its own env — never read from env here.
    pub tls: Option<TlsFront>,
    /// Global cap on concurrent player-QUIC connections (`PLAYER_MAX_CONNS`, default
    /// [`DEFAULT_PLAYER_MAX_CONNS`]) — threaded to the player edge before it listens.
    /// `0` disables the cap (opt-out). Only used when a player server is passed to
    /// [`run`]. A process/topology knob read HERE, never by a module.
    pub player_max_conns: usize,
    /// Per-source-IP cap on concurrent player-QUIC connections (`PLAYER_MAX_CONNS_PER_IP`,
    /// default [`DEFAULT_PLAYER_MAX_CONNS_PER_IP`]). `0` disables it. Only used when a
    /// player server is passed to [`run`]. A process/topology knob read HERE, never by a
    /// module.
    pub player_max_conns_per_ip: usize,
    pub player_rate_limit_rps: f64,
    pub player_rate_limit_burst: u32,
    pub player_conn_rate_limit_rps: f64,
    pub player_conn_rate_limit_burst: u32,
}

impl Config {
    /// Reads the standard process env (`DATABASE_URL`, `PORT`, `EDGE_ADDR`,
    /// `PLAYER_EDGE_ADDR`) into a [`Config`], applying the same defaults the Go
    /// monolith used. Both `:8080` and `8080` forms of `PORT` are accepted. The DSN
    /// is always `Some` here — a process that wants no DB calls [`Config::without_db`].
    pub fn from_env() -> Config {
        let mut cfg = Config::from_values(
            std::env::var("DATABASE_URL").ok(),
            std::env::var("PORT").ok(),
            std::env::var("EDGE_ADDR").ok(),
            std::env::var("PLAYER_EDGE_ADDR").ok(),
            std::env::var("EDGE_DRAIN_GRACE_MS").ok(),
            std::env::var("HTTP_DRAIN_GRACE_MS").ok(),
            std::env::var("MODULE_STOP_GRACE_MS").ok(),
            std::env::var("HTTP_REQUEST_TIMEOUT_MS").ok(),
            std::env::var("PLAYER_MAX_CONNS").ok(),
            std::env::var("PLAYER_MAX_CONNS_PER_IP").ok(),
        );
        cfg.player_rate_limit_rps = env_rate(
            "PLAYER_RATE_LIMIT_RPS",
            DEFAULT_PLAYER_RATE_LIMIT_RPS,
            RateZeroPolicy::Allow,
        );
        cfg.player_rate_limit_burst =
            env_number("PLAYER_RATE_LIMIT_BURST", DEFAULT_PLAYER_RATE_LIMIT_BURST);
        cfg.player_conn_rate_limit_rps = env_rate(
            "PLAYER_CONN_RATE_LIMIT_RPS",
            DEFAULT_PLAYER_CONN_RATE_LIMIT_RPS,
            RateZeroPolicy::Allow,
        );
        cfg.player_conn_rate_limit_burst = env_number(
            "PLAYER_CONN_RATE_LIMIT_BURST",
            DEFAULT_PLAYER_CONN_RATE_LIMIT_BURST,
        );
        cfg
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

    /// Overrides the whole-request inbound HTTP timeout default (round 4, finding 3).
    /// [`Config::from_env`] already defaults it ON at 30s for every process and lets
    /// `HTTP_REQUEST_TIMEOUT_MS` tune/disable it; this builder lets a composition root
    /// pick a different baseline (mirrors [`Config::with_rate_limit_default`]). Pass a
    /// zero `Duration` to disable the layer.
    pub fn with_request_timeout_default(self, timeout: std::time::Duration) -> Config {
        Config {
            http_request_timeout: (!timeout.is_zero()).then_some(timeout),
            ..self
        }
    }

    /// Sets how the HTTP front terminates TLS — `None` leaves the plain-HTTP path
    /// untouched. Called by the composition root that parsed the TLS env
    /// (`cmd/gateway-svc` today); `core/app` itself never reads TLS env vars.
    pub fn with_tls(self, tls: Option<TlsFront>) -> Config {
        Config { tls, ..self }
    }

    /// The pure core of [`Config::from_env`] — env values in, config out. Split out so
    /// the default/override logic is unit-testable without mutating process-global env.
    /// One positional param per env var (all `Option<String>`, same shape) — a mirror of
    /// `from_env`, not a public builder, so the long arg list is deliberate.
    #[allow(clippy::too_many_arguments)]
    fn from_values(
        dsn: Option<String>,
        port: Option<String>,
        edge: Option<String>,
        player_edge: Option<String>,
        drain_grace_ms: Option<String>,
        http_drain_grace_ms: Option<String>,
        module_stop_grace_ms: Option<String>,
        http_request_timeout_ms: Option<String>,
        player_max_conns: Option<String>,
        player_max_conns_per_ip: Option<String>,
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
        let module_stop_grace_ms = module_stop_grace_ms
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_MODULE_STOP_GRACE_MS);
        // Same parse shape as the graces above (unset/blank/unparseable → default);
        // the ONE difference is the disable semantics: an explicit `0` yields `None`
        // (layer off), any positive value yields `Some(Duration)`. Default 30s.
        let http_request_timeout_ms = http_request_timeout_ms
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_HTTP_REQUEST_TIMEOUT_MS);
        let http_request_timeout =
            (http_request_timeout_ms > 0).then(|| std::time::Duration::from_millis(http_request_timeout_ms));
        let player_max_conns = player_max_conns
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(DEFAULT_PLAYER_MAX_CONNS);
        let player_max_conns_per_ip = player_max_conns_per_ip
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(DEFAULT_PLAYER_MAX_CONNS_PER_IP);
        Config {
            database_url: Some(database_url),
            listen_addr: normalize_addr(port.as_deref().unwrap_or_default()),
            edge_addr,
            player_edge_addr,
            rate_limit_default: None,
            edge_drain_grace: std::time::Duration::from_millis(edge_drain_grace_ms),
            http_drain_grace: std::time::Duration::from_millis(http_drain_grace_ms),
            module_stop_grace: std::time::Duration::from_millis(module_stop_grace_ms),
            http_request_timeout,
            tls: None,
            player_max_conns,
            player_max_conns_per_ip,
            player_rate_limit_rps: DEFAULT_PLAYER_RATE_LIMIT_RPS,
            player_rate_limit_burst: DEFAULT_PLAYER_RATE_LIMIT_BURST,
            player_conn_rate_limit_rps: DEFAULT_PLAYER_CONN_RATE_LIMIT_RPS,
            player_conn_rate_limit_burst: DEFAULT_PLAYER_CONN_RATE_LIMIT_BURST,
        }
    }
}

fn env_number<T>(name: &str, default: T) -> T
where
    T: std::str::FromStr + Copy,
{
    parse_number(std::env::var(name).ok().as_deref(), default)
}

/// Parses one optional rate without applying a surface-specific fallback. Unset and
/// blank values are absent; malformed, non-finite, negative, and policy-rejected zero
/// values are invalid.
fn parse_rate(
    value: Option<&str>,
    zero_policy: RateZeroPolicy,
) -> Result<Option<f64>, RateParseError> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let rate = value
        .parse::<f64>()
        .map_err(|_| RateParseError::Malformed)?;
    if !rate.is_finite() {
        return Err(RateParseError::NonFinite);
    }
    if rate < 0.0 {
        return Err(RateParseError::Negative);
    }
    if rate == 0.0 && zero_policy == RateZeroPolicy::Reject {
        return Err(RateParseError::ZeroRejected);
    }
    Ok(Some(rate))
}

/// Resolves one parsed rate against the owning surface's fallback while preserving the
/// invalid reason for a value-less warning at the env boundary.
fn resolve_rate(value: Option<&str>, default: f64, zero_policy: RateZeroPolicy) -> ResolvedRate {
    match parse_rate(value, zero_policy) {
        Ok(Some(value)) => ResolvedRate {
            value,
            invalid: None,
        },
        Ok(None) => ResolvedRate {
            value: default,
            invalid: None,
        },
        Err(error) => ResolvedRate {
            value: default,
            invalid: Some(error),
        },
    }
}

/// Reads and resolves one rate env knob. Warnings deliberately include the variable,
/// fallback, and reason but never the operator-provided raw value.
fn env_rate(name: &str, default: f64, zero_policy: RateZeroPolicy) -> f64 {
    let raw = std::env::var(name).ok();
    let resolved = resolve_rate(raw.as_deref(), default, zero_policy);
    if let Some(reason) = resolved.invalid {
        tracing::warn!(
            variable = name,
            fallback_rps = default,
            %reason,
            "invalid rate-limit configuration; using fallback"
        );
    }
    resolved.value
}

fn parse_number<T>(value: Option<&str>, default: T) -> T
where
    T: std::str::FromStr + Copy,
{
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
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
///    pool exists, else answers a plain 200; when [`Config::with_tls`] set a
///    [`TlsFront`], the same router is served over rustls instead ([`serve_https`]),
///
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

    // 4. Two-phase Build (all registers before any init). The per-module stop
    //    deadline (`MODULE_STOP_GRACE_MS`, read HERE not by any module) bounds each
    //    module's `stop` so one hung module can't stall teardown.
    let mut app = App::new(ctx.clone()).with_stop_grace(cfg.module_stop_grace);
    for m in modules {
        app.add(m);
    }
    app.build().context("startup failed")?;

    // The plane's worker-health probe joins `/readyz`: a process whose pull
    // workers died (panic) OR whose workers are alive but persistently failing
    // (reconnect/error loop — `dead` never flips there) must stop taking traffic
    // that expects their effects. 30s ≈ 30 ticks of the 1s poll floor plus slack
    // for a slow pass, so a transient error never flaps readiness.
    if let Some(p) = &plane {
        let liveness = p.liveness();
        ctx.contribute(
            httpmw::READINESS_SLOT,
            httpmw::ReadyCheck::new("asyncevents-worker", move || {
                let liveness = liveness.clone();
                async move {
                    if liveness.dead() {
                        Err("asyncevents worker task died".to_string())
                    } else if liveness.delivery_stalled(DELIVERY_STALL_MAX) {
                        Err(format!(
                            "asyncevents workers have not completed a healthy pass in >{}s",
                            DELIVERY_STALL_MAX.as_secs()
                        ))
                    } else {
                        Ok(())
                    }
                }
            }),
        );
        // The retention GC task gets its OWN named check, never the delivery
        // `dead` flag: a GC outage is storage growth, not a serving outage, and
        // per-task isolation keeps the failing surface visible by name.
        let liveness = p.liveness();
        let retention_stall_after = p.retention_stall_after();
        ctx.contribute(
            httpmw::READINESS_SLOT,
            httpmw::ReadyCheck::new("asyncevents-retention", move || {
                let liveness = liveness.clone();
                async move {
                    if liveness.retention_dead() {
                        Err("asyncevents retention task died".to_string())
                    } else if liveness.retention_stalled(retention_stall_after) {
                        Err(retention_stall_message(retention_stall_after))
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

    // Module register/init is complete and the app-owned plane checks above are the
    // final readiness contributors. Snapshot the typed slot now so a forged-key type
    // conflict fails during boot wiring, never on a `/readyz` request. ReadyCheck is a
    // cloneable Arc-backed handle; its closure remains live when invoked per request.
    let readiness_checks = snapshot_readiness_checks(&ctx);

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
        // After module start (so the snapshot sees every wiring-time `register`): run
        // each callback's FIRST refresh synchronously — a failure fails startup loudly —
        // then launch the NOTIFY listener + poll fallback. This runs BEFORE durable
        // delivery starts: a durable handler reading a replica-local cache must never
        // run against a cold cache. (Teardown deliberately does NOT mirror this order —
        // `ordered_teardown` halts the durable plane FIRST, before modules stop.)
        if let Some(p) = &mut invalidation {
            p.start().await.context("invalidation start failed")?;
        }
        if let Some(p) = &mut plane {
            p.start().await.context("asyncevents start failed")?;
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
                let player = std::mem::take(&mut *shared.lock().unwrap())
                    .with_conn_limits(cfg.player_max_conns, cfg.player_max_conns_per_ip)
                    .with_request_limits(
                        cfg.player_rate_limit_rps,
                        cfg.player_rate_limit_burst,
                        cfg.player_conn_rate_limit_rps,
                        cfg.player_conn_rate_limit_burst,
                    );
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
        // `/readyz` folds in the baseline DB ping (when a pool exists) PLUS the startup
        // snapshot of every `httpmw::ReadyCheck` contributed during wiring. Membership
        // is fixed before migrate/start; each Arc-backed check closure still runs live on
        // every request. Any failure → 503 with a per-failed-check JSON body.
        let ready_pool = pool.clone();
        let ready_checks = readiness_checks.clone();
        let router = ctx
            .take_router()
            .route("/healthz", get(|| async { "ok" }))
            .route(
                "/readyz",
                get(move || {
                    let pool = ready_pool.clone();
                    let checks = ready_checks.clone();
                    async move {
                        readyz_response(pool.as_ref(), checks, READY_CHECK_TIMEOUT).await
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
        let gateway_always_on = cfg.rate_limit_default.is_some();
        let (default_rps, default_burst) = cfg.rate_limit_default.unwrap_or((0.0, 40));
        let zero_policy = if gateway_always_on {
            RateZeroPolicy::Reject
        } else {
            RateZeroPolicy::Allow
        };
        let rps = env_rate("RATE_LIMIT_RPS", default_rps, zero_policy);
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

        // Whole-request inbound timeout (round 4, finding 3): bound request-received →
        // response-STARTED across the whole surface, layered here — the same point as the
        // rate limiter, i.e. UNDER the metrics layer applied below — so a timeout is
        // COUNTED (Go's `metrics(...)` is the outermost wrap; the last `.layer` added is
        // outermost). This bounds BOTH the typed-op body-decode await AND the proxy
        // passthrough's inbound-upload leg, because both resolve inside the handler future.
        //
        // The emitted status is **408 Request Timeout** and that is DELIBERATE — do NOT
        // "fix" it to 504. tower-http's `TimeoutLayer` returns 408; the request never
        // reached an upstream, so 408 (client didn't produce a request in time) is the
        // honest code. The proxy passthrough's origin→client RESPONSE streaming resolves
        // the handler future FIRST (`forward()` returns a streaming body), so that leg is
        // NOT bounded by this layer — the proxy's own per-chunk `read_timeout` covers it.
        // Default 30s (`HTTP_REQUEST_TIMEOUT_MS`, read in core/app); explicit `0` disables.
        let router = match cfg.http_request_timeout {
            Some(d) => {
                tracing::info!(timeout_ms = %d.as_millis(), "http request timeout enabled (408 on elapse)");
                // 408 spelled out explicitly (the deprecated `::new` defaulted to it) so
                // the deliberate status is visible at the call site, not just in the comment.
                router.layer(tower_http::timeout::TimeoutLayer::with_status_code(
                    StatusCode::REQUEST_TIMEOUT,
                    d,
                ))
            }
            None => {
                tracing::info!("http request timeout disabled (HTTP_REQUEST_TIMEOUT_MS=0)");
                router
            }
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

        // TLS branch (admin hardening Step 4): when the composition root configured a
        // [`TlsFront`], the SAME router is served over rustls by [`serve_https`] —
        // wired to the same watch-signal + drain-grace shutdown contract as the plain
        // branch below, so the teardown choreography after this block is identical.
        // `None` (every process except the public front door) falls through to the
        // plain-HTTP path, unchanged.
        if let Some(front) = cfg.tls.clone() {
            let (sig_tx, sig_rx) = tokio::sync::watch::channel(false);
            tokio::spawn(async move {
                shutdown_signal().await;
                let _ = sig_tx.send(true);
            });
            return serve_https(front, &bind, router, cfg.http_drain_grace, sig_rx).await;
        }

        // 9. Serve the plain-HTTP plane through [`serve_http`] — the exact TLS-branch shape
        //    above, with the SAME watch-signal graceful-shutdown contract (see the comment
        //    at that dispatch on why a level-triggered `watch`, not a `Notify`). The plain
        //    path serves through axum-server's `Handle` for the SAME reason `serve_https`
        //    does: each connection task owns its hyper future and a grace-expiry abort DROPS
        //    it in place (true cancellation) rather than the detached-task leak `axum::serve`
        //    would produce (see `serve_http`).
        let (sig_tx, sig_rx) = tokio::sync::watch::channel(false);
        tokio::spawn(async move {
            shutdown_signal().await;
            let _ = sig_tx.send(true);
        });
        serve_http(&bind, router, cfg.http_drain_grace, sig_rx).await
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

/// Normalizes an `ACME_CONTACT` entry into the `mailto:` URI ACME expects, tolerating
/// an operator who already included the `mailto:` prefix (the documented contract is
/// a bare email — see `docs/reference/hetzner-deploy-checklist.md`) so we never emit
/// a double-prefixed `mailto:mailto:...`.
fn normalize_mailto(c: &str) -> String {
    let email = c.strip_prefix("mailto:").unwrap_or(c);
    format!("mailto:{email}")
}

/// Serves `router` over plain HTTP on `bind` until `sig_rx` flips — the twin of
/// [`serve_https`], served through axum-server's [`Handle`](axum_server::Handle)
/// rather than [`axum::serve`].
///
/// Why not `axum::serve`: axum 0.7 `tokio::spawn`s one DETACHED task per connection
/// and never stores the `JoinHandle`s, so dropping its serve future when the drain
/// grace expires ABANDONS the in-flight handlers — a hung handler keeps running (and
/// keeps touching modules/DB) while [`ordered_teardown`] stops the durable plane and
/// the modules underneath it. axum-server instead gives each connection task
/// ownership of its hyper future and `select!`s it against the `Handle`'s shutdown, so
/// a grace-expiry abort DROPS that future in place — true cancellation, no leak. This
/// is the exact mechanism [`serve_https`] already relied on; the plain path now shares
/// it.
///
/// Shutdown contract: once `sig_rx` flips, the drain task calls
/// `graceful_shutdown(None)` (stop accepting, let in-flight connections finish on
/// their own with NO internal deadline), waits `drain_grace`, samples the still-open
/// `connection_count` at that single instant, warns if any remain, then `shutdown()`
/// force-drops them. Using `None` + an explicit deadline here — rather than
/// `graceful_shutdown(Some(drain_grace))` as [`serve_https`] does — deliberately
/// trades the exact mirror for a race-free count: with `Some(grace)` axum-server's own
/// timer fires `shutdown()` at the same instant this task wakes and can zero the count
/// before we read it, turning the operator warn into a silent false-negative. The
/// drain OUTCOME (graceful for `drain_grace`, then hard abort) is identical, and the
/// serve future returns within `drain_grace` of the signal either way, so the ordered
/// teardown that follows in [`run`] starts on time.
///
/// Listener handoff: axum-server's `from_tcp` wraps the std listener via tokio's
/// `TcpListener::from_std`, which REQUIRES non-blocking mode already set; tokio's
/// `into_std` returns a listener with non-blocking mode left ON (verified against
/// tokio 1.52 `into_std` docs + axum-server 0.8 `from_tcp` source), so the handoff
/// needs no `set_nonblocking` call.
async fn serve_http(
    bind: &str,
    router: axum::Router,
    drain_grace: std::time::Duration,
    sig_rx: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind http {bind}"))?;
    tracing::info!(addr = %bind, "listening");

    let handle = axum_server::Handle::new();
    let drain_handle = handle.clone();
    let mut rx = sig_rx;
    tokio::spawn(async move {
        // Level-triggered watch (see the `run` comment): a flip is observed even if it
        // raced ahead of this `wait_for`.
        let _ = rx.wait_for(|v| *v).await;
        drain_handle.graceful_shutdown(None);
        tokio::time::sleep(drain_grace).await;
        let abandoned = drain_handle.connection_count();
        if abandoned > 0 {
            // Preserves the operator-visible signal the old select-timer emitted — now
            // carrying the count of connections force-dropped.
            tracing::warn!(abandoned, "http drain grace expired; abandoning in-flight connections");
        }
        drain_handle.shutdown();
    });

    // Serve WITH connection info so the gateway's passthrough can set `X-Forwarded-For`
    // (and the rate limiter can key per client IP) — identical `MakeService` to
    // `serve_https`. The await returns once the accept loop has stopped (graceful
    // signal) AND every connection task has ended (drained or aborted by the task
    // above).
    axum_server::from_tcp(
        listener
            .into_std()
            .context("convert http listener to std")?,
    )?
    .handle(handle)
    .serve(router.into_make_service_with_connect_info::<SocketAddr>())
    .await
    .context("http serve")
}

/// Serves `router` over HTTPS on `bind` until `sig_rx` flips — the TLS twin of the
/// plain [`serve_http`] branch in [`run`] (admin hardening Step 4). Same shutdown
/// contract: once the watch signal fires, `axum_server::Handle::graceful_shutdown`
/// gets `drain_grace` to finish in-flight connections, then abandons stragglers — so
/// the drain is time-bounded exactly like the plain branch's select-timer, and the
/// ordered teardown that follows in [`run`] starts on time either way.
///
/// Crypto-provider note: both TLS arms build rustls configs through the process
/// default provider. The workspace compiles rustls with ONLY the `ring` provider
/// feature (single-provider auto-detection covers us), and the front-door main
/// (`cmd/gateway-svc`) additionally pins it via
/// `rustls::crypto::ring::default_provider().install_default()` — never aws-lc-rs.
async fn serve_https(
    front: TlsFront,
    bind: &str,
    router: axum::Router,
    drain_grace: std::time::Duration,
    sig_rx: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let addr: SocketAddr = bind
        .parse()
        .with_context(|| format!("parse https bind addr {bind:?}"))?;

    // The graceful-shutdown bridge: the same level-triggered watch signal the plain
    // branch uses (see the `run` comment on why a watch, not a Notify) triggers
    // axum-server's handle-based drain, bounded by `drain_grace`.
    let handle = axum_server::Handle::new();
    let drain_handle = handle.clone();
    let mut rx = sig_rx;
    tokio::spawn(async move {
        let _ = rx.wait_for(|v| *v).await;
        drain_handle.graceful_shutdown(Some(drain_grace));
    });

    let make = router.into_make_service_with_connect_info::<SocketAddr>();
    match front {
        TlsFront::Files { cert, key } => {
            let config = axum_server::tls_rustls::RustlsConfig::from_pem_file(&cert, &key)
                .await
                .with_context(|| format!("load TLS cert {cert:?} / key {key:?}"))?;
            tracing::info!(addr = %bind, cert = ?cert, "listening (https, cert/key files)");
            axum_server::bind_rustls(addr, config)
                .handle(handle)
                .serve(make)
                .await
                .context("https serve")?;
        }
        TlsFront::Acme {
            domains,
            cache_dir,
            contact,
        } => {
            // TLS-ALPN-01 against Let's Encrypt production: the challenge is answered
            // inline on this listener via the acceptor's cert resolver — no :80
            // listener, no extra route. Account key + issued certs persist in
            // `cache_dir` so restarts don't re-issue (rate limits).
            let mut state = rustls_acme::AcmeConfig::new(domains.clone())
                .contact(contact.iter().map(|c| normalize_mailto(c)))
                .cache(rustls_acme::caches::DirCache::new(cache_dir.clone()))
                .directory_lets_encrypt(true)
                .state();
            let acceptor = state.axum_acceptor(state.default_rustls_config());
            // The state driver: polling this stream IS the ACME machinery (order,
            // validate, renew). Logged via tracing; aborted once serving ends.
            let driver = tokio::spawn(async move {
                use futures::StreamExt as _;
                loop {
                    match state.next().await {
                        Some(Ok(event)) => tracing::info!(?event, "acme"),
                        Some(Err(err)) => tracing::warn!(%err, "acme"),
                        None => break,
                    }
                }
            });
            tracing::info!(
                addr = %bind, domains = ?domains, cache = ?cache_dir,
                "listening (https, ACME/Let's Encrypt via TLS-ALPN-01)"
            );
            let served = axum_server::bind(addr)
                .acceptor(acceptor)
                .handle(handle)
                .serve(make)
                .await
                .context("https serve");
            driver.abort();
            served?;
        }
    }
    Ok(())
}

/// Builds the `/readyz` response: the baseline DB ping (when a pool exists) plus every
/// contributed [`httpmw::ReadyCheck`]. All green → `200 ok`; any failure → `503` with a
/// JSON body mapping each FAILED check's name to its error string (Go's `readyzHandler`
/// shape, refined to named checks instead of `readiness[i]` indices). Kept as a free
/// function so it is unit-testable without a live DB (pass `None` + failing checks). Each
/// check (the DB ping AND every contributed check) is individually bounded by `bound` —
/// the route closure passes [`READY_CHECK_TIMEOUT`]; tests pass a short bound so a hung
/// check fails fast instead of stalling the test suite for real wall-clock seconds.
async fn readyz_response(
    pool: Option<&PgPool>,
    checks: Vec<httpmw::ReadyCheck>,
    bound: std::time::Duration,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let mut failures: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    if let Some(pool) = pool {
        match tokio::time::timeout(bound, sqlx::query("SELECT 1").execute(pool)).await {
            Ok(Ok(_)) => {}
            Ok(Err(err)) => {
                tracing::warn!(%err, "readyz db check failed");
                failures.insert("db".to_string(), err.to_string());
            }
            Err(_elapsed) => {
                tracing::warn!(?bound, "readyz db check timed out");
                failures.insert("db".to_string(), format!("timed out after {bound:?}"));
            }
        }
    }
    for check in checks {
        let name = check.name().to_string();
        match tokio::time::timeout(bound, check.run()).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                failures.insert(name, err);
            }
            Err(_elapsed) => {
                failures.insert(name, format!("timed out after {bound:?}"));
            }
        }
    }
    if failures.is_empty() {
        (StatusCode::OK, "ok").into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, axum::Json(failures)).into_response()
    }
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
/// - windows: Ctrl-C, Ctrl-Break, `CTRL_CLOSE_EVENT` (console window closed), or
///   `CTRL_SHUTDOWN_EVENT` (system shutdown).
///
/// A failure to install a handler is logged and treated as "shut down".
///
/// **Windows limitation:** `Stop-Process -Force` / `taskkill /F` map to
/// `TerminateProcess` — no console control event ever reaches the process, so no
/// graceful path can run for a forced kill. A graceful stop on Windows needs a
/// *non-forced* console event. The `splitproof` harness ([W2]) spawns the process in
/// its own group and sends `CTRL_BREAK_EVENT`; forced kill is only its cleanup fallback.
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
    use tokio::signal::windows::{ctrl_break, ctrl_close, ctrl_shutdown};

    let mut break_signal = match ctrl_break() {
        Ok(break_signal) => break_signal,
        Err(err) => {
            tracing::error!(%err, "failed to listen for CTRL_BREAK_EVENT; shutting down");
            return;
        }
    };

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
        _ = break_signal.recv() => {}
        _ = close.recv() => {}
        _ = shutdown.recv() => {}
    }
}

#[cfg(test)]
mod tests;
