//! Minimal factual probes consumed by `tools/conformance`.

#[doc(hidden)]
pub fn conformance_username_rejected(len: usize) -> bool {
    !crate::username_within_cap(&"a".repeat(len))
}

#[doc(hidden)]
pub fn conformance_password_rejected(len: usize) -> bool {
    !crate::password_within_cap(&"a".repeat(len))
}

/// The byte cap `core.events` applies to every `cp.invoke` input value.
pub const CORE_EVENTS_INPUT_VALUE_BYTES: usize = crate::events_provider::MAX_INPUT_VALUE_BYTES;

#[doc(hidden)]
pub fn conformance_core_events_input_rejected(len: usize) -> bool {
    let input = std::collections::BTreeMap::from([("value".to_string(), "a".repeat(len))]);
    !crate::events_provider::input_within_cap(&input)
}
