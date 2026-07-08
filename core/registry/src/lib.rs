//! The synchronous service lookup: a module `provide`s a named service, another
//! `require`s it and downcasts it to its OWN local trait. A leaf: depends on no
//! module, importable by everyone.
//!
//! # Why capability-scoped keys (the one real semantic gap vs. Go)
//!
//! Go registers one service under `"characters"`; consumers downcast the erased
//! `any` to whichever *structural* interface they need — one stored value, many
//! views. Rust's `Any` downcasts an erased value to exactly ONE concrete
//! `Sized + 'static` type; you cannot recover three different `Arc<dyn Trait>`
//! from one erased slot. So each capability gets its OWN key, derived mechanically
//! as `"<module>.<capability>"` (see [`key`]):
//!
//! ```ignore
//! reg.provide::<dyn Ownership + Send + Sync>(key("characters", "ownership"), arc);
//! let own = reg.require::<dyn Ownership + Send + Sync>(&key("characters", "ownership"));
//! ```
//!
//! The registry stores `Arc<T>` (which is itself a concrete `Sized + 'static`
//! value even when `T` is `dyn Trait`) boxed as `Box<dyn Any>`, and downcasts it
//! back to the same `Arc<T>` on the way out. The registry swap still holds: a
//! local impl and a remote stub both `provide` an `Arc<dyn Ownership>` under the
//! same key, so a consumer is unaware which it got.

use std::any::{type_name, Any};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Maps a service name to its implementation — one service per name. (Contrast
/// [`contrib::Slots`], which collects MANY values under one name.)
///
/// The map lives behind a `Mutex` for interior mutability: the same `&Registry`
/// is shared (through the lifecycle `Context`) across every module's `register`
/// and `init`, so `provide`/`require` take `&self`, not `&mut self`.
#[derive(Default)]
pub struct Registry {
    services: Mutex<HashMap<String, Box<dyn Any + Send + Sync>>>,
}

impl Registry {
    pub fn new() -> Self {
        Registry::default()
    }

    /// Registers a named service so other modules can `require` it. Panics on a
    /// duplicate — a wiring bug, better loud at startup than silent later.
    ///
    /// `T` is typically `dyn SomeTrait + Send + Sync`; pass the matching
    /// `Arc<T>`. The concrete `Arc<T>` handle is boxed as `dyn Any` for lookup.
    pub fn provide<T: ?Sized + Send + Sync + 'static>(&self, name: impl Into<String>, svc: Arc<T>) {
        let name = name.into();
        let mut services = self.services.lock().unwrap();
        if services.contains_key(&name) {
            panic!("service {name:?} already provided");
        }
        services.insert(name, Box::new(svc));
    }

    /// Looks up a named service and downcasts it to `Arc<T>`. The presence check
    /// comes FIRST so a missing service keeps its distinct "not found" message; a
    /// present-but-wrong-type service then fails downcast with a separate message.
    /// Both are wiring bugs — loud at startup rather than a surprise later.
    pub fn require<T: ?Sized + Send + Sync + 'static>(&self, name: &str) -> Arc<T> {
        let services = self.services.lock().unwrap();
        let Some(svc) = services.get(name) else {
            panic!("required service {name:?} not found");
        };
        match svc.downcast_ref::<Arc<T>>() {
            Some(arc) => Arc::clone(arc),
            None => panic!("service {name:?} does not implement {}", type_name::<T>()),
        }
    }

    /// The comma-ok variant of [`require`](Registry::require): `Some(arc)` when a
    /// service is registered under `name` AND downcasts to `Arc<T>`, else `None`
    /// (name absent, or present but not a `T`). Never panics — for an OPTIONAL
    /// dependency a consumer can run without.
    pub fn try_require<T: ?Sized + Send + Sync + 'static>(&self, name: &str) -> Option<Arc<T>> {
        let services = self.services.lock().unwrap();
        let arc = services.get(name)?.downcast_ref::<Arc<T>>()?;
        Some(Arc::clone(arc))
    }
}

/// Derives the capability key `"<module>.<capability>"`. Keeping derivation in one
/// place makes the provider side and every consumer agree without a shared const.
pub fn key(module: &str, capability: &str) -> String {
    format!("{module}.{capability}")
}

#[cfg(test)]
mod tests;
