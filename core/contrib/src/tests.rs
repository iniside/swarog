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
fn canonical_empty_read_reserves_type_before_forged_contribution() {
    let slots = Slots::new();
    const CANONICAL: Slot<String> = Slot::new("reserved");
    const FORGED: Slot<u32> = Slot::new("reserved");

    assert!(slots.contributions(CANONICAL).is_empty());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        slots.contribute(FORGED, 42);
    }));
    assert!(
        result.is_err(),
        "canonical empty read must reserve its type"
    );
    assert!(slots.contributions(CANONICAL).is_empty());
    slots.contribute(CANONICAL, "valid".to_string());
    assert_eq!(slots.contributions(CANONICAL), vec!["valid"]);
}

#[test]
fn forged_first_bucket_rejects_canonical_read_and_contribution() {
    let slots = Slots::new();
    const CANONICAL: Slot<String> = Slot::new("forged-first");
    const FORGED: Slot<u32> = Slot::new("forged-first");

    slots.contribute(FORGED, 7);
    let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        slots.contributions(CANONICAL)
    }));
    assert!(
        read.is_err(),
        "canonical read must reject a forged-first bucket"
    );
    let contribute = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        slots.contribute(CANONICAL, "wrong bucket".to_string());
    }));
    assert!(
        contribute.is_err(),
        "canonical contribution must reject a forged-first bucket"
    );

    assert_eq!(slots.contributions(FORGED), vec![7]);
    slots.contribute(FORGED, 8);
    assert_eq!(slots.contributions(FORGED), vec![7, 8]);
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
