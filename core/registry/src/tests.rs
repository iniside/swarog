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
