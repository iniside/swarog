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
    /// An opt-in tool-only hook — `None` until a tool installs it via
    /// [`Registry::set_require_observer`]. Fired once per `require`/`try_require`
    /// so a tool (e.g. `requirecheck`) can observe every attempted lookup. Kept in
    /// its own lock so installing/reading it never contends with `services`, and
    /// mirrors `Bus`'s `Mutex<Option<Arc<dyn Transport>>>` seam.
    require_observer: Mutex<Option<RequireObserver>>,
}

/// The require-observer closure: invoked with the lookup kind and the `&str` key on
/// every [`require`](Registry::require) / [`try_require`](Registry::try_require).
/// A shared alias so the field and [`Registry::set_require_observer`] name one type.
pub type RequireObserver = Arc<dyn Fn(RequireKind, &str) + Send + Sync>;

/// Whether a lookup was a mandatory [`require`](Registry::require) or an optional
/// [`try_require`](Registry::try_require) — the flag the require-observer receives.
/// Only mandatory requires must be declared in a module's `requires()`, so a tool
/// distinguishes the two at the hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequireKind {
    /// A mandatory `require::<T>` (panics if absent).
    Mandatory,
    /// An optional `try_require::<T>` (returns `None` if absent).
    Optional,
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
        self.notify_observer(RequireKind::Mandatory, name);
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
        self.notify_observer(RequireKind::Optional, name);
        let services = self.services.lock().unwrap();
        let arc = services.get(name)?.downcast_ref::<Arc<T>>()?;
        Some(Arc::clone(arc))
    }

    /// Installs an opt-in observer fired once per [`require`](Registry::require)
    /// (as [`RequireKind::Mandatory`]) and [`try_require`](Registry::try_require)
    /// (as [`RequireKind::Optional`]), before the map lookup — so even a missing-key
    /// panic still records the attempted require. Takes `&self` (interior
    /// mutability), so a tool installs it via `ctx.registry().set_require_observer(…)`
    /// without any `Context` change. This is a **tool-only** introspection hook
    /// (e.g. `requirecheck`), the honest analogue of [`Bus::set_transport`]; the
    /// closure sees only a `&str` key, keeping this leaf foundation-pure.
    ///
    /// Last-write-wins: unlike `Bus::set_transport` there is no double-install
    /// invariant here (a tool may re-install freely), so a re-set silently replaces
    /// the previous observer rather than panicking.
    ///
    /// **INVARIANT — the observer closure MUST NOT call back into the registry**
    /// (`require`/`try_require`/`provide`): [`std::sync::Mutex`] is non-reentrant, so
    /// re-entry from inside the observer would deadlock. The observer is invoked
    /// while holding NEITHER the `services` lock NOR the observer lock (the `Arc` is
    /// cloned out and both locks dropped first), but calling back in would still
    /// re-lock `services` recursively via the outer `require` frame — so keep the
    /// closure self-contained (touch only its own state).
    ///
    /// [`Bus::set_transport`]: ../bus/struct.Bus.html#method.set_transport
    pub fn set_require_observer(&self, f: RequireObserver) {
        *self.require_observer.lock().unwrap() = Some(f);
    }

    /// Fires the require-observer if one is installed. Clones the `Arc` out of its
    /// lock and drops that lock BEFORE invoking the closure, so the observer never
    /// runs while holding the observer lock (nor the `services` lock — callers fire
    /// this before taking it). The no-observer path is a cheap `if let Some` no-op.
    fn notify_observer(&self, kind: RequireKind, name: &str) {
        let obs = self.require_observer.lock().unwrap().clone();
        if let Some(obs) = obs {
            obs(kind, name);
        }
    }
}

/// Derives the capability key `"<module>.<capability>"`. Keeping derivation in one
/// place makes the provider side and every consumer agree without a shared const.
pub fn key(module: &str, capability: &str) -> String {
    format!("{module}.{capability}")
}

#[cfg(test)]
mod tests;
