//! This module's convention-conformance entry — the explicit stance `apikeys`
//! declares for every [`conformance::Convention`], executed by the
//! `conformancecheck` harness (`tools/conformance`). Always compiled (the
//! `asyncevents::testing` precedent — no feature flags); nothing here runs on a
//! production path.

use std::sync::Arc;

use conformance::{CapCase, Convention, Entry, Fixture, Stance};

use crate::admin::check_key_length;

/// The `apikeys` conformance entry. Stances per the decided matrix
/// (`docs/plans/2026-07-12-0952-convention-conformance-harness-plan.md`). The T8
/// probe reuses the EXISTING validation fn (`admin::check_key_length`, the same
/// `apikeysapi::MAX_KEY_BYTES` contract `store::insert_tx` and the DDL CHECK
/// enforce) — no refactor was needed here.
pub fn entry() -> Entry {
    Entry {
        module: "apikeys",
        stances: vec![
            (
                Convention::EnvValidation,
                Stance::NotApplicable {
                    why: "APIKEYS_DEV_SEED is a boolean opt-in gate that fails closed by \
                          absence; no env value is parsed at init that could be silently \
                          defaulted",
                },
            ),
            (
                Convention::InputByteCaps,
                Stance::Applies(Fixture::InputByteCaps(vec![CapCase {
                    name: "apikeys key secret",
                    cap: apikeysapi::MAX_KEY_BYTES,
                    probe: Arc::new(|len| check_key_length(&"a".repeat(len)).is_err()),
                }])),
            ),
            (
                Convention::InfraOutage503,
                Stance::NotApplicable {
                    why: "the API-key verifier and its outage classification live in the \
                          gateway module; apikeys only serves authoritative lookups over \
                          its own local store",
                },
            ),
            (
                Convention::ArgonParity,
                Stance::NotApplicable {
                    why: "apikeys stores plaintext key secrets under the sessions-token \
                          trust model — it performs no password hashing",
                },
            ),
        ],
    }
}
