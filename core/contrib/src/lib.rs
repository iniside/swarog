//! The multi-value slot registry. Unlike [`registry`](../registry) (one service
//! per name), a slot collects MANY contributors — for cross-cutting collections
//! like admin items, health checks or nav entries. A consumer reads them all via
//! [`Slots::contributions`], so a new module lights up without the consumer being
//! edited. A leaf: depends on no module, importable by everyone.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::Mutex;

/// A canonical, typed contribution slot.
pub struct Slot<T> {
    name: &'static str,
    marker: PhantomData<fn() -> T>,
}

impl<T> Slot<T> {
    /// Defines a slot. Production slots are declared once by their contract owner.
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            marker: PhantomData,
        }
    }
}

impl<T> Copy for Slot<T> {}

impl<T> Clone for Slot<T> {
    fn clone(&self) -> Self {
        *self
    }
}

struct Bucket {
    type_id: TypeId,
    type_name: &'static str,
    values: Box<dyn Any + Send + Sync>,
}

/// Holds every slot's contributions, each in registration order.
///
/// Behind a `Mutex` for the same reason as [`registry::Registry`]: the shared
/// `&Slots` (through the lifecycle `Context`) is contributed to from many
/// modules' `init`, so `contribute` takes `&self`.
#[derive(Default)]
pub struct Slots {
    m: Mutex<HashMap<&'static str, Bucket>>,
}

impl Slots {
    pub fn new() -> Self {
        Slots::default()
    }

    /// Adds a value to a typed slot.
    ///
    /// A wrong value is rejected by the compiler:
    ///
    /// ```compile_fail
    /// use contrib::{Slot, Slots};
    /// const NAMES: Slot<String> = Slot::new("names");
    /// Slots::new().contribute(NAMES, 42_u32);
    /// ```
    pub fn contribute<T: Any + Send + Sync>(&self, slot: Slot<T>, value: T) {
        let mut buckets = self.m.lock().unwrap();
        if let Some(bucket) = buckets.get(slot.name) {
            if bucket.type_id != TypeId::of::<T>() {
                let actual = bucket.type_name;
                drop(buckets);
                panic!(
                    "contribution slot {:?} has type {}, not {}",
                    slot.name,
                    actual,
                    std::any::type_name::<T>()
                );
            }
        }
        let bucket = buckets.entry(slot.name).or_insert_with(|| Bucket {
            type_id: TypeId::of::<T>(),
            type_name: std::any::type_name::<T>(),
            values: Box::new(Vec::<T>::new()),
        });
        bucket
            .values
            .downcast_mut::<Vec<T>>()
            .expect("slot TypeId and Vec<T> storage must agree")
            .push(value);
    }

    /// Everything contributed to `slot`, in registration order. The slot's type
    /// selects `T`; a same-name slot forged with another type panics in every build.
    ///
    /// Returns a `Vec<T>` by cloning — the `Mutex` guard cannot outlive the
    /// call, so borrowed views are impossible; contributions are cheap handles
    /// (`Arc<dyn Trait>`, small structs) where cloning is fine.
    pub fn contributions<T: Clone + Send + Sync + 'static>(&self, slot: Slot<T>) -> Vec<T> {
        let mut buckets = self.m.lock().unwrap();
        if let Some(bucket) = buckets.get(slot.name) {
            if bucket.type_id != TypeId::of::<T>() {
                let actual = bucket.type_name;
                drop(buckets);
                panic!(
                    "contribution slot {:?} has type {}, not {}",
                    slot.name,
                    actual,
                    std::any::type_name::<T>()
                );
            }
        }
        let bucket = buckets.entry(slot.name).or_insert_with(|| Bucket {
            type_id: TypeId::of::<T>(),
            type_name: std::any::type_name::<T>(),
            values: Box::new(Vec::<T>::new()),
        });
        bucket
            .values
            .downcast_ref::<Vec<T>>()
            .expect("slot TypeId and Vec<T> storage must agree")
            .clone()
    }
}

#[cfg(test)]
mod tests;
