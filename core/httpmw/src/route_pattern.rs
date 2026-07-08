//! [`RoutePattern`] — a response-extension label bridge between the gateway front door
//! and the metrics recording layer.
//!
//! The front door dispatches its operations from an axum FALLBACK handler, which carries
//! no [`axum::extract::MatchedPath`] (the fallback is not a routed path). Without a
//! matched path, `metrics::record` would record every op under the fixed `"unmatched"`
//! label — the whole op surface collapsed into one series. To restore per-op cardinality
//! WITHOUT ever labelling a raw URL, the front door inserts a `RoutePattern` (the op's
//! route PATTERN, e.g. `"/characters"` or `"/characters/{id}"`, or a proxy prefix like
//! `"/admin/*"`) into the RESPONSE extensions after it matches; `metrics::record` reads it
//! in preference to `MatchedPath`, then falls back to `"unmatched"`.
//!
//! It lives here (not in `gateway` or `metrics`) because it is the shared seam BOTH sides
//! import: `metrics` already depends on `httpmw`, and `gateway` depends on it for this.
//! `Arc<str>` keeps cloning the label into the metrics layer cheap.

use std::sync::Arc;

/// The matched route pattern for a request the gateway front door dispatched, carried in
/// the response extensions for `metrics::record` to read (see the module docs).
#[derive(Clone, Debug)]
pub struct RoutePattern(Arc<str>);

impl RoutePattern {
    /// Wraps a route pattern (an op's `path`, or a proxy prefix pattern).
    pub fn new(pattern: impl Into<Arc<str>>) -> RoutePattern {
        RoutePattern(pattern.into())
    }

    /// The pattern string used as the metrics `path` label.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
