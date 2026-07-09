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

#[test]
fn require_observer_records_kind_and_key_sequence() {
    let reg = Registry::new();
    let log: Arc<Mutex<Vec<(RequireKind, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&log);
    reg.set_require_observer(Arc::new(move |kind, name: &str| {
        sink.lock().unwrap().push((kind, name.to_string()));
    }));

    reg.provide::<dyn Greeter>(key("i18n", "greeter"), Arc::new(En));
    // Mandatory require of a present service.
    let _ = reg.require::<dyn Greeter>(&key("i18n", "greeter"));
    // Optional require, present.
    let _ = reg.try_require::<dyn Greeter>(&key("i18n", "greeter"));
    // Optional require, absent.
    assert!(reg.try_require::<dyn Greeter>("nope").is_none());

    let recorded = log.lock().unwrap().clone();
    assert_eq!(
        recorded,
        vec![
            (RequireKind::Mandatory, key("i18n", "greeter")),
            (RequireKind::Optional, key("i18n", "greeter")),
            (RequireKind::Optional, "nope".to_string()),
        ]
    );
}

#[test]
fn no_observer_is_a_silent_no_op() {
    // The un-observed path still resolves and records nothing (no observer to record).
    let reg = Registry::new();
    reg.provide::<dyn Greeter>("x", Arc::new(En));
    let g = reg.require::<dyn Greeter>("x");
    assert_eq!(g.greet(), "hello");
    assert!(reg.try_require::<dyn Greeter>("absent").is_none());
}

#[test]
fn require_observer_records_before_missing_key_panic() {
    // The observer fires at the TOP of require, so a missing-mandatory panic still
    // leaves the attempted require recorded.
    let reg = Registry::new();
    let log: Arc<Mutex<Vec<(RequireKind, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&log);
    reg.set_require_observer(Arc::new(move |kind, name: &str| {
        sink.lock().unwrap().push((kind, name.to_string()));
    }));

    let result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| reg.require::<dyn Greeter>("gone")));
    assert!(result.is_err(), "require of a missing service must panic");
    assert_eq!(
        *log.lock().unwrap(),
        vec![(RequireKind::Mandatory, "gone".to_string())]
    );
}

#[test]
fn set_require_observer_is_last_write_wins() {
    let reg = Registry::new();
    let first: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let second: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let f1 = Arc::clone(&first);
    let f2 = Arc::clone(&second);
    reg.set_require_observer(Arc::new(move |_, name: &str| {
        f1.lock().unwrap().push(name.to_string());
    }));
    reg.set_require_observer(Arc::new(move |_, name: &str| {
        f2.lock().unwrap().push(name.to_string());
    }));

    let _ = reg.try_require::<dyn Greeter>("k");
    assert!(first.lock().unwrap().is_empty(), "first observer must be replaced");
    assert_eq!(*second.lock().unwrap(), vec!["k".to_string()]);
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
