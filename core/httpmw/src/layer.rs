//! [`HttpLayer`] — a module-contributed HTTP middleware layer, plus the [`LAYER_SLOT`]
//! it is contributed to. The `edge::EDGE_SLOT` analogue for the HTTP plane.
//!
//! The seam that lets a core-infra module wrap the WHOLE HTTP surface without the boot
//! layer hard-coding it: a module (e.g. `metrics`) contributes an `HttpLayer` during
//! `init`, and `app::run` drains the slot at a FIXED point in router assembly — AFTER the
//! merged module routes are rate-limited — so a contributed layer wraps the rate limiter
//! (a `429` it issues is still seen by, e.g., the metrics recorder). Multiple
//! contributions apply in CONTRIBUTION ORDER: the first contributed is the innermost, the
//! last contributed is the OUTERMOST layer (axum's `Router::layer` nests later-added
//! layers outside earlier ones, and this drain adds them in order).
//!
//! Lives HERE (not `core/app`): `app` is the top boot layer, so hosting the type there
//! and having `metrics` contribute it would force `metrics → app` (a leaf depending on the
//! top). Instead both the drainer (`app`) and the contributor (`metrics`) depend on this
//! leaf — the same shape as `edge` owning `EdgeReg`. The closure names only `axum::Router`,
//! so no heavier crate is needed.

use std::sync::{Arc, Mutex};

use axum::Router;

/// The contrib slot HTTP layers are contributed to (`ctx.contribute`) and `app::run`
/// drains (`ctx.contributions::<HttpLayer>`).
pub const LAYER_SLOT: &str = "app.http_layer";

/// The wrapping closure: consumes the assembled router and returns it wrapped (typically
/// one `Router::layer` call). `FnOnce` because axum layering consumes the router.
type WrapFn = Box<dyn FnOnce(Router) -> Router + Send>;

/// One module's contributed HTTP layer, waiting in [`LAYER_SLOT`] until `app::run` applies
/// it over the assembled router.
///
/// Mechanics mirror [`crate::ReadyCheck`]/`edge::EdgeReg`: the contrib registry hands
/// contributions back BY CLONE, but wrapping is a one-shot `FnOnce` (it consumes the
/// router). So the closure sits behind an `Arc<Mutex<Option<…>>>`: clones share the ONE
/// closure, and [`HttpLayer::apply`] `take`s it — applied at most once by construction, no
/// matter how many clones the slot handed out.
#[derive(Clone)]
pub struct HttpLayer(Arc<Mutex<Option<WrapFn>>>);

impl HttpLayer {
    /// Wraps a module's layering closure. The closure receives the assembled router and
    /// returns it wrapped (e.g. `|r| r.layer(middleware::from_fn(record))`).
    pub fn new(f: impl FnOnce(Router) -> Router + Send + 'static) -> HttpLayer {
        HttpLayer(Arc::new(Mutex::new(Some(Box::new(f)))))
    }

    /// Applies the wrap to `router`, returning the wrapped router. At most once across ALL
    /// clones of this contribution: a second call (or a call on another clone) returns
    /// `router` unchanged.
    pub fn apply(&self, router: Router) -> Router {
        match self.0.lock().unwrap().take() {
            Some(f) => f(router),
            None => router,
        }
    }
}

#[cfg(test)]
#[path = "layer_tests.rs"]
mod tests;
