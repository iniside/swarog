//! This module's convention-conformance entry — the explicit stance `audit`
//! declares for every [`conformance::Convention`], executed by the
//! `conformancecheck` harness (`tools/conformance`). Always compiled (the
//! `asyncevents::testing` precedent — no feature flags); nothing here runs on a
//! production path.

use conformance::{Convention, Entry, EnvCase, Fixture, Stance};

/// The `audit` conformance entry. Stances per the decided matrix
/// (`docs/plans/2026-07-12-0952-convention-conformance-harness-plan.md`). Only
/// `EnvValidation` applies: `AUDIT_RETENTION_DAYS` must fail startup on a
/// parseable non-positive value (`modules/audit/src/lib.rs` — `env_int` already
/// falls back to the default when unset or unparseable, so the bad values below
/// must be values it actually parses and rejects, not ones it silently defaults
/// away).
pub fn entry() -> Entry {
    Entry {
        module: "audit",
        stances: vec![
            (
                Convention::EnvValidation,
                Stance::Applies(Fixture::EnvValidation(vec![
                    EnvCase {
                        var: "AUDIT_RETENTION_DAYS",
                        bad_value: "0",
                    },
                    EnvCase {
                        var: "AUDIT_RETENTION_DAYS",
                        bad_value: "-3",
                    },
                ])),
            ),
            (
                Convention::InputByteCaps,
                Stance::NotApplicable {
                    why: "audit is a zero-coupling raw sink — it records whatever raw \
                          event JSON the bus hands it via on_tx_raw and takes no \
                          player-supplied free-text field of its own to cap",
                },
            ),
            (
                Convention::InfraOutage503,
                Stance::NotApplicable {
                    why: "audit has no auth verifier or sync capability that classifies \
                          outages — it is a durable-subscription sink with no request \
                          path to misclassify",
                },
            ),
            (
                Convention::ArgonParity,
                Stance::NotApplicable {
                    why: "audit performs no password hashing — it has no credential \
                          material at all",
                },
            ),
        ],
    }
}
