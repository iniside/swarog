//! `httpmw` — cross-cutting HTTP middleware mounted in the boot layer (`core/app`,
//! and, always-on, the gateway front door). Never in a domain module: a module
//! registers routes on the shared `Context` router; only the boot layer, after every
//! module has mounted, can wrap the WHOLE surface. Port of Go's `httpmw/httpmw.go`.
//!
//! Three concerns, each in its own submodule:
//!
//! - **Rate limiting** ([`IpLimiter`] + [`mount`]) — a per-IP token-bucket limiter
//!   (rate `r`, burst `b`) with a background idle-bucket eviction loop, mirroring Go's
//!   `golang.org/x/time/rate` per-IP buckets. Over the limit → `429` with a plain body
//!   and `Retry-After: 1`.
//! - **Trusted-proxy client IP** ([`client_ip`] + [`parse_cidrs`]) — resolves the
//!   trustworthy client address for the limiter's bucket key. **SECURITY-CRITICAL:**
//!   `X-Forwarded-For` is honored ONLY when the direct peer is itself a trusted proxy,
//!   and then the chain is walked RIGHT-TO-LEFT for the first hop NOT in the trusted
//!   set — never index 0 blindly (a reverse proxy APPENDS the real peer, so `XFF[0]` is
//!   fully attacker-controlled). This is deliberately NOT any framework's default XFF
//!   extractor — those trust the header unconditionally, the exact spoof Go guards.
//! - **Readiness slot** ([`ReadyCheck`] + [`READINESS_SLOT`]) — a contribution slot
//!   `/readyz` folds in: `core/app` runs the baseline DB ping plus every contributed
//!   check and answers `503` with a per-failed-check JSON body on any failure.
//! - **HTTP layer slot** ([`HttpLayer`] + [`LAYER_SLOT`]) — a contribution slot `core/app`
//!   drains to wrap the whole rate-limited surface: a core-infra module (`metrics`)
//!   contributes its recording layer here instead of `app` hard-coding it.
//! - **Route-pattern label** ([`RoutePattern`]) — a response-extension the gateway front
//!   door stamps with an op's route pattern so `metrics::record` labels fallback-dispatched
//!   op traffic by pattern instead of the fixed `"unmatched"`. The shared seam both the
//!   gateway and `metrics` import.
//!
//! Leaf rule: this crate imports only core infrastructure dependencies; it never reaches
//! a module or an `api/` contract crate — same tier as `bus`/`registry`/`metrics`.

mod client_ip;
mod layer;
mod limiter;
mod middleware;
mod readiness;
mod route_pattern;

pub use client_ip::{client_ip, parse_cidrs};
pub use layer::{HttpLayer, LAYER_SLOT};
pub use limiter::IpLimiter;
#[doc(hidden)]
pub use limiter::table_saturation_collector;
pub use middleware::mount;
pub use readiness::{ReadyCheck, READINESS_SLOT};
pub use route_pattern::RoutePattern;

/// The infra endpoints that must NEVER be rate limited: the k8s liveness/readiness
/// probes and the Prometheus scrape all arrive from one IP and a `429` there means a
/// restart loop or scrape gaps. The EXACT set `core/metrics` exempts from recording.
pub const INFRA_PATHS: [&str; 3] = ["/healthz", "/readyz", "/metrics"];

/// Reports whether `path` targets an infra endpoint exempt from rate limiting (Go's
/// `httpmw.SkipInfra`). Matched against the request's exact URL path.
pub fn skip_infra(path: &str) -> bool {
    INFRA_PATHS.contains(&path)
}

#[cfg(test)]
#[path = "skip_infra_tests.rs"]
mod skip_infra_tests;
