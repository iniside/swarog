//! The multi-value slot registry. Unlike [`registry`](../registry) (one service
//! per name), a slot collects MANY contributors — for cross-cutting collections
//! like admin items, health checks or nav entries. A consumer reads them all via
//! [`Slots::contributions`], so a new module lights up without the consumer being
//! edited. A leaf: depends on no module, importable by everyone.

use std::any::Any;
use std::collections::HashMap;
use std::sync::Mutex;

/// Holds every slot's contributions, each in registration order.
///
/// Behind a `Mutex` for the same reason as [`registry::Registry`]: the shared
/// `&Slots` (through the lifecycle `Context`) is contributed to from many
/// modules' `init`, so `contribute` takes `&self`.
#[derive(Default)]
pub struct Slots {
    m: Mutex<HashMap<String, Vec<Box<dyn Any + Send + Sync>>>>,
}

impl Slots {
    pub fn new() -> Self {
        Slots::default()
    }

    /// Adds a value to a named slot.
    pub fn contribute<T: Any + Send + Sync>(&self, slot: impl Into<String>, v: T) {
        self.m
            .lock()
            .unwrap()
            .entry(slot.into())
            .or_default()
            .push(Box::new(v));
    }

    /// Everything contributed to `slot`, downcast to `T`, in registration order.
    ///
    /// A slot is homogeneous by contract (all admin items are `adminapi::Item`),
    /// so `T` is the slot's element type. A value that does not downcast to `T`
    /// is a wiring bug (a typo'd type on a critical slot — edge regs, ops,
    /// readiness — would otherwise manifest as silently missing wiring): every
    /// miss is `tracing::error!`-logged, and in debug/test builds
    /// (`debug_assertions`) the call panics via `debug_assert!`. Release builds
    /// keep the skip semantics after logging. The loudness is deliberate:
    /// `contributions()` runs per-request on `/readyz` (the readiness slot is
    /// read lazily), so under `debug_assertions` a type mismatch panics every
    /// debug run — including the split-proof fleet.
    ///
    /// Returns a `Vec<T>` by cloning — the `Mutex` guard cannot outlive the
    /// call, so borrowed views are impossible; contributions are cheap handles
    /// (`Arc<dyn Trait>`, small structs) where cloning is fine.
    pub fn contributions<T: Clone + 'static>(&self, slot: &str) -> Vec<T> {
        let m = self.m.lock().unwrap();
        let Some(items) = m.get(slot) else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(items.len());
        let mut misses = 0usize;
        for b in items {
            match b.downcast_ref::<T>() {
                Some(v) => out.push(v.clone()),
                None => misses += 1,
            }
        }
        drop(m); // release before a potential debug_assert! panic — never poison the lock
        if misses > 0 {
            tracing::error!(
                slot,
                expected = std::any::type_name::<T>(),
                misses,
                "contributions: type mismatch — values silently skipped"
            );
            debug_assert!(
                misses == 0,
                "contributions::<{}>({slot:?}): {misses} contribution(s) failed to downcast — \
                 a slot is homogeneous by contract",
                std::any::type_name::<T>()
            );
        }
        out
    }
}

#[cfg(test)]
mod tests;
