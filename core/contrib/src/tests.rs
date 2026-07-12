use super::*;

#[test]
fn preserves_registration_order() {
    let slots = Slots::new();
    const NAV: Slot<String> = Slot::new("nav");
    slots.contribute(NAV, "home".to_string());
    slots.contribute(NAV, "players".to_string());
    slots.contribute(NAV, "matches".to_string());
    let got = slots.contributions(NAV);
    assert_eq!(got, vec!["home", "players", "matches"]);
}

#[test]
fn empty_slot_is_empty_vec() {
    let slots = Slots::new();
    const NOTHING: Slot<String> = Slot::new("nothing");
    assert!(slots.contributions(NOTHING).is_empty());
}

#[test]
fn forged_type_conflict_panics_without_corrupting_the_bucket() {
    let slots = Slots::new();
    const STRINGS: Slot<String> = Slot::new("mixed");
    const NUMBERS: Slot<u32> = Slot::new("mixed");
    slots.contribute(STRINGS, "a string".to_string());
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        slots.contribute(NUMBERS, 42);
    }));
    assert!(r.is_err(), "same-name/different-type slot must panic");

    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        slots.contributions(NUMBERS)
    }));
    assert!(r.is_err(), "same-name/different-type read must panic");
    assert_eq!(slots.contributions(STRINGS), vec!["a string"]);
}

#[test]
fn separate_slots_are_independent() {
    let slots = Slots::new();
    const A: Slot<u32> = Slot::new("a");
    const B: Slot<u32> = Slot::new("b");
    slots.contribute(A, 1);
    slots.contribute(B, 2);
    slots.contribute(A, 3);
    assert_eq!(slots.contributions(A), vec![1, 3]);
    assert_eq!(slots.contributions(B), vec![2]);
}
