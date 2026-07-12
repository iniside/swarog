//! Minimal factual probes consumed by `tools/conformance`.

#[doc(hidden)]
pub fn conformance_key_rejected(len: usize) -> bool {
    crate::admin::check_key_length(&"a".repeat(len)).is_err()
}
