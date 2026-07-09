//! `metrics` — the cross-cutting HTTP-metrics middleware + `/metrics` scrape, packaged as
//! a core-infra [`lifecycle::Module`] listed in EVERY process main (a `core/` crate that
//! stands on its own as a `Module`). Its `init` mounts `GET /metrics`
//! (`ctx.mount`) and contributes the recording layer to [`httpmw::LAYER_SLOT`], which
//! `app::run` drains over the whole rate-limited surface. (Port of Go's `metrics/metrics.go`,
//! but self-registering instead of wired by a `Config` flag.)
//!
//! It owns a PRIVATE [`prometheus::Registry`] (never `prometheus::default_registry()`),
//! so scraping this process never picks up anything another library registered globally.
//! A process lists this module iff it should serve `/metrics`: every module host does, and
//! — since the single front door now measures its op traffic — so does `gateway-svc` (the
//! earlier gateway exemption was Go parity that lost its rationale once peers stopped
//! fronting HTTP).
//!
//! Two collectors, both labeled `{method, path, status}`:
//! - `http_requests_total` — request count,
//! - `http_request_duration_seconds` — latency histogram, on prometheus's default
//!   buckets, which are byte-identical to Go's `prometheus.DefBuckets`
//!   (`.005,.01,.025,.05,.1,.25,.5,1,2.5,5,10`).
//!
//! **The `path` label is the MATCHED ROUTE PATTERN, never the raw URL** — the cardinality
//! guard Go implemented by reading `r.Pattern` after the mux matched. The axum analogue is
//! the [`axum::extract::MatchedPath`] request extension, which the router inserts during
//! routing; [`layer`] is applied with [`axum::Router::layer`] so it wraps each route's
//! service (running AFTER the match), making the extension visible before `next` runs.
//!
//! The gateway front door is the exception: it dispatches its operations from an axum
//! FALLBACK, which carries no `MatchedPath`, so its whole op surface would otherwise
//! record under `"unmatched"`. The front door therefore stamps the op's route pattern into
//! the response extensions as a [`httpmw::RoutePattern`], and [`record`] reads it in
//! preference to `MatchedPath`. A request that matched neither is recorded under the fixed
//! label `"unmatched"` so attacker-supplied paths can never explode label cardinality.
//!
//! Infra endpoints (`/healthz`, `/readyz`, `/metrics`) are EXEMPT from recording — the
//! same set Go's `httpmw.SkipInfra` names — so liveness/readiness probes and the scrape
//! itself never pollute the series (a deliberate, documented tightening of Go's metrics
//! middleware, which counted its own scrape as "benign").
//!
//! Leaf rule: this crate imports only `axum` + `prometheus`; it never reaches a module or
//! an `api/` contract crate.

use std::sync::OnceLock;
use std::time::Instant;

use axum::extract::{MatchedPath, Request};
use axum::http::header::CONTENT_TYPE;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use lifecycle::{Context, Module};
use prometheus::{
    Encoder, HistogramOpts, HistogramVec, IntCounterVec, Opts, Registry, TextEncoder, TEXT_FORMAT,
};

/// The label placed on a request that matched no route (Go's empty-`r.Pattern` case).
const UNMATCHED: &str = "unmatched";

/// Infra endpoints exempt from recording — the exact set Go's `httpmw.SkipInfra` lists.
const INFRA_PATHS: [&str; 3] = ["/healthz", "/readyz", "/metrics"];

/// This process's own metrics: a PRIVATE registry plus the two labeled collectors, built
/// once on first use. Deliberately NOT the global default registry, so nothing another
/// crate registers globally leaks into our scrape and vice versa.
struct Collectors {
    registry: Registry,
    requests_total: IntCounterVec,
    request_duration: HistogramVec,
}

fn metrics() -> &'static Collectors {
    static M: OnceLock<Collectors> = OnceLock::new();
    M.get_or_init(|| {
        let registry = Registry::new();
        let requests_total = IntCounterVec::new(
            Opts::new(
                "http_requests_total",
                "Total HTTP requests processed, labeled by method, route pattern, and status.",
            ),
            &["method", "path", "status"],
        )
        .expect("valid http_requests_total metric");
        // Default buckets == Go's prometheus.DefBuckets, so no explicit `.buckets(...)`.
        let request_duration = HistogramVec::new(
            HistogramOpts::new(
                "http_request_duration_seconds",
                "HTTP request latency in seconds, labeled by method, route pattern, and status.",
            ),
            &["method", "path", "status"],
        )
        .expect("valid http_request_duration_seconds metric");
        registry
            .register(Box::new(requests_total.clone()))
            .expect("register http_requests_total");
        registry
            .register(Box::new(request_duration.clone()))
            .expect("register http_request_duration_seconds");
        Collectors {
            registry,
            requests_total,
            request_duration,
        }
    })
}

/// The metrics module: a core-infra [`Module`] listed in every process main. It requires
/// nothing, holds no state (the collectors are a process-global built on first use), and
/// runs only `init`.
///
/// `init` (a) mounts `GET /metrics` on the shared router (`ctx.mount`) and (b) contributes
/// the recording layer to [`httpmw::LAYER_SLOT`]; `app::run` applies that layer over the
/// whole rate-limited surface (so a `429` the limiter issues is still recorded) and skips
/// the infra endpoints from recording. A process serves `/metrics` iff it lists this
/// module — every module host does, and so does the `gateway-svc` front door (its op
/// traffic is now measured).
pub struct Metrics;

impl Metrics {
    pub fn new() -> Metrics {
        Metrics
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Metrics::new()
    }
}

#[async_trait::async_trait]
impl Module for Metrics {
    fn name(&self) -> &str {
        "metrics"
    }

    /// Wire-only (default `Caps::NONE`): mount the scrape route and contribute the
    /// recording layer. No `register`/`migrate`/`start`/`stop` — nothing to persist or run.
    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        // (a) The scrape route lands in the merged router; `app::run` later adds
        // `/healthz`/`/readyz`, rate-limits, then applies the layer below — which exempts
        // `/metrics` from recording (see `record`), so the scrape never self-counts.
        ctx.mount(Router::new().route("/metrics", get(scrape)));

        // (b) The recording layer. `app::run` drains `LAYER_SLOT` AFTER rate limiting, so
        // this wraps the limiter and a `429` is still recorded. `Router::layer` runs the
        // middleware AFTER routing, so `MatchedPath` is visible to `record`.
        ctx.contribute(
            httpmw::LAYER_SLOT,
            httpmw::HttpLayer::new(|router: Router| router.layer(middleware::from_fn(record))),
        );
        Ok(())
    }
}

/// The recording middleware. Resolves the `path` label — the gateway front door's
/// response-stamped [`httpmw::RoutePattern`] first (its ops dispatch from an axum FALLBACK,
/// which has no `MatchedPath`), then the request's [`MatchedPath`] (a real routed path),
/// then the fixed `"unmatched"` — skips the infra endpoints, then times `next` and updates
/// both collectors with the final status. `MatchedPath` is read BEFORE `next.run` consumes
/// the request; `RoutePattern` is read AFTER, from the response the front door produced.
async fn record(req: Request, next: Next) -> Response {
    let method = req.method().as_str().to_owned();
    // The routed path (present for a real axum route; ABSENT for the gateway's fallback,
    // which instead stamps a `RoutePattern` onto the response — preferred below).
    let matched = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_owned());

    // Infra endpoints (probes + the scrape) are real routes with a `MatchedPath`; never
    // recorded — no cardinality, no self-counting scrape. Still served, just not measured.
    if matches!(&matched, Some(p) if INFRA_PATHS.contains(&p.as_str())) {
        return next.run(req).await;
    }

    let start = Instant::now();
    let response = next.run(req).await;

    // Prefer the front door's response-stamped route pattern (the fallback carries no
    // `MatchedPath`), then the request's `MatchedPath`, then the fixed `"unmatched"`.
    let path = response
        .extensions()
        .get::<httpmw::RoutePattern>()
        .map(|r| r.as_str().to_owned())
        .or(matched)
        .unwrap_or_else(|| UNMATCHED.to_owned());

    let status = response.status().as_u16().to_string();

    let m = metrics();
    let labels = [method.as_str(), path.as_str(), status.as_str()];
    m.requests_total.with_label_values(&labels).inc();
    m.request_duration
        .with_label_values(&labels)
        .observe(start.elapsed().as_secs_f64());

    response
}

/// Registers an additional collector into this process's PRIVATE registry so it is
/// served by the same `GET /metrics` scrape — the seam other core infrastructure
/// (the asyncevents plane's subscription-lag gauges) uses to publish its series
/// without owning a second registry. Returns the prometheus error on a duplicate
/// registration; callers guard with a `OnceLock` per process.
pub fn register(c: Box<dyn prometheus::core::Collector>) -> prometheus::Result<()> {
    metrics().registry.register(c)
}

/// The `GET /metrics` handler: renders the private registry in the Prometheus text
/// exposition format. Never recorded in its own counters (see [`record`]).
async fn scrape() -> impl IntoResponse {
    let encoder = TextEncoder::new();
    let mut buf = Vec::new();
    // Encoding a `Vec<u8>` sink is infallible in practice; on the impossible error we
    // still answer with whatever was written rather than 500 the scrape.
    let _ = encoder.encode(&metrics().registry.gather(), &mut buf);
    // `TEXT_FORMAT` is the `'static` exposition content type (== `encoder.format_type()`),
    // used directly so the response borrows nothing from the local encoder.
    ([(CONTENT_TYPE, TEXT_FORMAT)], buf)
}

#[cfg(test)]
mod tests;
