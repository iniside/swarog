//! `metrics` — the cross-cutting HTTP-metrics middleware + `/metrics` scrape, mounted
//! by `core/app` in every module-hosting process (port of Go's `metrics/metrics.go`).
//!
//! It owns a PRIVATE [`prometheus::Registry`] (never `prometheus::default_registry()`),
//! so scraping this process never picks up anything another library registered globally,
//! and a process that never mounts this — the pure-transport `gateway-svc` — exposes no
//! `/metrics` at all (Go parity: the gateway binary carries no prometheus footprint).
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
//! service (running AFTER the match), making the extension visible before `next` runs. A
//! request that matched no route has no `MatchedPath`; like Go's empty-pattern case it is
//! recorded under the fixed label `"unmatched"` so attacker-supplied paths can never
//! explode label cardinality.
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
struct Metrics {
    registry: Registry,
    requests_total: IntCounterVec,
    request_duration: HistogramVec,
}

fn metrics() -> &'static Metrics {
    static M: OnceLock<Metrics> = OnceLock::new();
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
        Metrics {
            registry,
            requests_total,
            request_duration,
        }
    })
}

/// Mounts the `/metrics` scrape route AND installs the recording middleware onto `router`,
/// returning the wrapped router. `core/app` calls this once, after every module has merged
/// its routes and `/healthz`/`/readyz` are added, so the layer wraps the WHOLE surface.
/// A process that never calls this (the gateway) exposes no `/metrics` and records nothing.
pub fn mount(router: Router) -> Router {
    router
        .route("/metrics", get(scrape))
        .layer(middleware::from_fn(record))
}

/// The recording middleware. Reads the matched route pattern (falling back to `"unmatched"`),
/// skips the infra endpoints, then times `next` and updates both collectors with the final
/// status. `MatchedPath` is read BEFORE `next.run` consumes the request (the extension is
/// already present because the layer wraps the post-match route service).
async fn record(req: Request, next: Next) -> Response {
    let method = req.method().as_str().to_owned();
    let path = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_owned())
        .unwrap_or_else(|| UNMATCHED.to_owned());

    // Infra endpoints (probes + the scrape) are never recorded — no cardinality, no
    // self-counting scrape. Still served, just not measured.
    if INFRA_PATHS.contains(&path.as_str()) {
        return next.run(req).await;
    }

    let start = Instant::now();
    let response = next.run(req).await;
    let status = response.status().as_u16().to_string();

    let m = metrics();
    let labels = [method.as_str(), path.as_str(), status.as_str()];
    m.requests_total.with_label_values(&labels).inc();
    m.request_duration
        .with_label_values(&labels)
        .observe(start.elapsed().as_secs_f64());

    response
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
