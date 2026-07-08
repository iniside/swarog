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
