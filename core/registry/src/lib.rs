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
mod tests {
    use super::*;

    trait Greeter: Send + Sync {
        fn greet(&self) -> String;
    }

    struct En;
    impl Greeter for En {
        fn greet(&self) -> String {
            "hello".into()
        }
    }

    trait Counter: Send + Sync {
        #[allow(dead_code)] // named only as a downcast target, never invoked
        fn count(&self) -> u32;
    }

    #[test]
    fn provide_then_require_trait_object() {
        let reg = Registry::new();
        reg.provide::<dyn Greeter>(key("i18n", "greeter"), Arc::new(En));
        let g = reg.require::<dyn Greeter>(&key("i18n", "greeter"));
        assert_eq!(g.greet(), "hello");
    }

    #[test]
    fn try_require_absent_is_none() {
        let reg = Registry::new();
        assert!(reg.try_require::<dyn Greeter>("nope").is_none());
    }

    #[test]
    fn try_require_wrong_type_is_none() {
        let reg = Registry::new();
        reg.provide::<dyn Greeter>("x", Arc::new(En));
        // Present under "x" but not a Counter.
        assert!(reg.try_require::<dyn Counter>("x").is_none());
    }

    #[test]
    #[should_panic(expected = "already provided")]
    fn duplicate_provide_panics() {
        let reg = Registry::new();
        reg.provide::<dyn Greeter>("x", Arc::new(En));
        reg.provide::<dyn Greeter>("x", Arc::new(En));
    }

    #[test]
    #[should_panic(expected = "not found")]
    fn require_missing_panics() {
        let reg = Registry::new();
        reg.require::<dyn Greeter>("missing");
    }

    #[test]
    #[should_panic(expected = "does not implement")]
    fn require_wrong_type_panics() {
        let reg = Registry::new();
        reg.provide::<dyn Greeter>("x", Arc::new(En));
        reg.require::<dyn Counter>("x");
    }

    // A concrete (Sized) service round-trips too, not only trait objects.
    #[test]
    fn provide_require_concrete() {
        let reg = Registry::new();
        reg.provide::<String>("greeting", Arc::new("hi".to_string()));
        assert_eq!(*reg.require::<String>("greeting"), "hi");
    }

    proptest::proptest! {
        // Property: N services provided under distinct keys each require back to
        // the exact value stored, and a never-provided key is absent.
        #[test]
        fn provide_require_roundtrip(entries in proptest::collection::hash_map("[a-z]{1,8}\\.[a-z]{1,8}", 0u64..1000, 0..24)) {
            let reg = Registry::new();
            for (k, v) in &entries {
                reg.provide::<u64>(k.clone(), Arc::new(*v));
            }
            for (k, v) in &entries {
                proptest::prop_assert_eq!(*reg.require::<u64>(k), *v);
            }
            proptest::prop_assert!(reg.try_require::<u64>("__absent__.__key__").is_none());
        }
    }
}
