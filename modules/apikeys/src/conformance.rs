//! Minimal factual probes consumed by `tools/conformance`.

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
