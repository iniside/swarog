//! Minimal factual probes consumed by `tools/conformance`.

#[doc(hidden)]
pub fn conformance_name_rejected(len: usize) -> bool {
    !crate::name_within_cap(&"a".repeat(len))
}

#[doc(hidden)]
pub fn conformance_class_rejected(len: usize) -> bool {
    !crate::class_within_cap(&"a".repeat(len))
}
