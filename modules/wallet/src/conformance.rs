//! Minimal factual probes consumed by `tools/conformance` — each answers "does the
//! module's own validator reject a movement carrying this many bytes in this field?".
//!
//! Every probe routes through [`crate::validate_movement`], the ENFORCEMENT POINT every
//! `credit`/`debit` runs before ledger SQL, rather than the leaf `*_within_cap` predicate
//! underneath it: a probe on the leaf stays green when the `if !…` branch that consults it
//! is deleted from the validator, which is exactly the shape `e3ddfef` closed for apikeys.
//! Only the field under test is oversized, so the rejection can come from no other branch.

use walletapi::Movement;

fn movement_with(field: fn(&mut Movement, String), len: usize) -> Movement {
    let mut m = Movement {
        idempotency_key: "conformance-key".into(),
        player_id: "conformance-player".into(),
        currency: "gold".into(),
        amount: 1,
        reason: "conformance".into(),
    };
    field(&mut m, "a".repeat(len));
    m
}

fn rejected(field: fn(&mut Movement, String), len: usize) -> bool {
    crate::validate_movement(&movement_with(field, len)).is_err()
}

#[doc(hidden)]
pub fn conformance_idempotency_key_rejected(len: usize) -> bool {
    rejected(|m, v| m.idempotency_key = v, len)
}

#[doc(hidden)]
pub fn conformance_currency_code_rejected(len: usize) -> bool {
    rejected(|m, v| m.currency = v, len)
}

#[doc(hidden)]
pub fn conformance_reason_rejected(len: usize) -> bool {
    rejected(|m, v| m.reason = v, len)
}
