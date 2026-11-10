//! Minimal factual probes consumed by `tools/conformance`.

#[doc(hidden)]
pub fn conformance_username_rejected(len: usize) -> bool {
    !crate::username_within_cap(&"a".repeat(len))
}

#[doc(hidden)]
pub fn conformance_password_rejected(len: usize) -> bool {
    !crate::password_within_cap(&"a".repeat(len))
}
