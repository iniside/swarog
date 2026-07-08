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
    /// A slot is homogeneous by convention (all admin items are `adminapi::Item`),
    /// so `T` is the slot's element type. Values that do not downcast to `T` are
    /// skipped. Returns a `Vec<T>` by cloning — the `Mutex` guard cannot outlive
    /// the call, so borrowed views are impossible; contributions are cheap handles
    /// (`Arc<dyn Trait>`, small structs) where cloning is fine.
    pub fn contributions<T: Clone + 'static>(&self, slot: &str) -> Vec<T> {
        let m = self.m.lock().unwrap();
        let Some(items) = m.get(slot) else {
            return Vec::new();
        };
        items
            .iter()
            .filter_map(|b| b.downcast_ref::<T>().cloned())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_registration_order() {
        let slots = Slots::new();
        slots.contribute("nav", "home".to_string());
        slots.contribute("nav", "players".to_string());
        slots.contribute("nav", "matches".to_string());
        let got = slots.contributions::<String>("nav");
        assert_eq!(got, vec!["home", "players", "matches"]);
    }

    #[test]
    fn empty_slot_is_empty_vec() {
        let slots = Slots::new();
        assert!(slots.contributions::<String>("nothing").is_empty());
    }

    #[test]
    fn separate_slots_are_independent() {
        let slots = Slots::new();
        slots.contribute("a", 1u32);
        slots.contribute("b", 2u32);
        slots.contribute("a", 3u32);
        assert_eq!(slots.contributions::<u32>("a"), vec![1, 3]);
        assert_eq!(slots.contributions::<u32>("b"), vec![2]);
    }
}
