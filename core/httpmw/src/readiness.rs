//! The readiness contribution slot (port of Go's `ReadinessSlot`). A module with its
//! own dependency (a cache, a downstream API, …) contributes a named [`ReadyCheck`];
//! `core/app`'s `/readyz` folds every contributed check in alongside the baseline DB
//! ping and answers `503` with a per-failed-check JSON body on any failure.
//!
//! Refinement over Go, which contributed a bare `func(context.Context) error` and
//! labeled failures by index (`readiness[0]`): a [`ReadyCheck`] carries a NAME, so the
//! `/readyz` failure body maps a meaningful `check_name → error string`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// The contrib slot `/readyz` reads (`ctx.contribute(READINESS_SLOT, ReadyCheck::new(…))`).
/// Defined here — not in a module — so it is a stable, dependency-free name any module
/// can target without an import cycle.
pub const READINESS_SLOT: contrib::Slot<ReadyCheck> = contrib::Slot::new("readiness.check");

/// A boxed, `Send` future producing a readiness verdict.
type CheckFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;

/// The stored check closure — `Arc` so [`ReadyCheck`] is `Clone` (the contrib registry
/// hands contributions back by clone).
type CheckFn = Arc<dyn Fn() -> CheckFuture + Send + Sync>;

/// A named readiness check a module contributes to [`READINESS_SLOT`]. On failure its
/// `name` becomes a key in `/readyz`'s `503` JSON body, its `Err(String)` the value.
#[derive(Clone)]
pub struct ReadyCheck {
    name: String,
    check: CheckFn,
}

impl ReadyCheck {
    /// Builds a check named `name` whose async closure `f` returns `Ok(())` when ready
    /// or `Err(reason)` when not. The closure is called fresh on every `/readyz` request
    /// (readiness is a live check, not a cached verdict).
    pub fn new<F, Fut>(name: impl Into<String>, f: F) -> ReadyCheck
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), String>> + Send + 'static,
    {
        ReadyCheck {
            name: name.into(),
            check: Arc::new(move || Box::pin(f())),
        }
    }

    /// This check's name — the key it contributes to the `/readyz` failure body.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Runs the check once.
    pub async fn run(&self) -> Result<(), String> {
        (self.check)().await
    }
}

#[cfg(test)]
#[path = "readiness_tests.rs"]
mod readiness_tests;
