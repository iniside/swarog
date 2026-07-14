//! Minimal factual probes consumed by `tools/conformance`.

/// Re-exported so `tools/conformance` can state the cap by reference instead of a
/// second literal — the store's `validate_policy` remains the sole definition site.
pub use crate::store::MAX_POLICY_BYTES;

/// A presented key longer than the shared byte cap is definitively rejected — the
/// invariant [`apikeysapi::MAX_KEY_BYTES`] still guarantees. Secrets are now
/// SERVER-GENERATED (there is no caller-supplied creation path to cap), so this probe
/// reflects the surviving authority: the gateway's presented-key length guard
/// (`modules/gateway/src/keys.rs`) and the store's generated-secret guard
/// (`store::generate_secret`) both hold the digest input to `<= MAX_KEY_BYTES`. Stays
/// TRUE for any over-cap length. (Step 8 repoints the conformance policy basis string to
/// the gateway lookup side accordingly.)
#[doc(hidden)]
pub fn conformance_key_rejected(len: usize) -> bool {
    len > apikeysapi::MAX_KEY_BYTES
}

/// A role's `policy` string longer than the shared byte cap is definitively rejected —
/// the invariant [`crate::store::MAX_POLICY_BYTES`] guarantees. `roles.policy` is
/// admin-writable via `admin.adminSubmit` (`create_role`/`set_role_policy`) and rides
/// every gateway key-lookup response plus its 5s cache, so `store::validate_policy`
/// caps it at admission — this probe mirrors that same authority, referencing the one
/// constant rather than a second literal. Stays TRUE for any over-cap length.
#[doc(hidden)]
pub fn conformance_policy_rejected(len: usize) -> bool {
    len > crate::store::MAX_POLICY_BYTES
}
