//! Minimal factual probes consumed by `tools/conformance`.
//!
//! Policy and expected outcomes live in the tool. These probes only exercise the
//! same private validators used by the production report path.

#[doc(hidden)]
pub fn conformance_report_id_rejected(len: usize) -> bool {
    crate::validate_report_id(&"a".repeat(len)).is_err()
}

#[doc(hidden)]
pub fn conformance_winner_rejected(len: usize) -> bool {
    crate::validate_participant("Winner", &"a".repeat(len)).is_err()
}

#[doc(hidden)]
pub fn conformance_loser_rejected(len: usize) -> bool {
    crate::validate_participant("Loser", &"a".repeat(len)).is_err()
}
