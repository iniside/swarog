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

/// The homogeneous-slot contract: a wrong-typed contribution under a slot makes
/// `contributions::<T>()` panic in debug/test builds (`debug_assert!`); release
/// builds log + skip instead.
#[test]
#[cfg(debug_assertions)]
fn mismatched_contribution_panics_in_debug() {
    let slots = Slots::new();
    slots.contribute("mixed", "a string".to_string());
    slots.contribute("mixed", 42u32);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        slots.contributions::<u32>("mixed")
    }));
    assert!(r.is_err(), "downcast miss must debug_assert!-panic in debug builds");
    // The lock is released before the panic, so the slots stay usable.
    slots.contribute("clean", 7u32);
    assert_eq!(slots.contributions::<u32>("clean"), vec![7]);
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
