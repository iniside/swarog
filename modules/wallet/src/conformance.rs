//! Minimal factual probes consumed by `tools/conformance` — each answers "does the
//! module's own validator reject an input of this many bytes?", executed against the
//! real cap functions rather than a restated number.

#[doc(hidden)]
pub fn conformance_idempotency_key_rejected(len: usize) -> bool {
    !crate::idempotency_key_within_cap(&"a".repeat(len))
}

#[doc(hidden)]
pub fn conformance_currency_code_rejected(len: usize) -> bool {
    !crate::currency_code_within_cap(&"a".repeat(len))
}

#[doc(hidden)]
pub fn conformance_reason_rejected(len: usize) -> bool {
    !crate::reason_within_cap(&"a".repeat(len))
}
