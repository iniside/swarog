//! This module's convention-conformance entry — the explicit stance `match`
//! declares for every [`conformance::Convention`], executed by the
//! `conformancecheck` harness (`tools/conformance`). Always compiled (the
//! `asyncevents::testing` precedent — no feature flags); nothing here runs on a
//! production path.
//!
//! Naming trap: the crate is `match_module` (`match` is a Rust keyword), but
//! [`Entry::module`] MUST be the string `"match"` — it equals `Module::name()`
//! and the `modules/match` directory name, the drift-diff key the harness uses.

use conformance::{Convention, Entry, Stance};

/// The `match` conformance entry. Stances per the decided matrix
/// (`docs/plans/2026-07-12-0952-convention-conformance-harness-plan.md`) — all
/// four conventions are NotApplicable for this module. `InputByteCaps`
/// deliberately does NOT add validation in this rollout — see the `why` below.
pub fn entry() -> Entry {
    Entry {
        module: "match",
        stances: vec![
            (
                Convention::EnvValidation,
                Stance::NotApplicable {
                    why: "match parses no env vars at all — /match/report is driven \
                          entirely by request body and registry-resolved MmrReader \
                          state, never process env",
                },
            ),
            (
                Convention::InputByteCaps,
                Stance::NotApplicable {
                    why: "ReportId has no byte-cap today; candidate for T8 adoption",
                },
            ),
            (
                Convention::InfraOutage503,
                Stance::NotApplicable {
                    why: "match has no auth verifier — its only sync dependency \
                          (MmrReader) surfaces ordinary opsapi errors through the \
                          registry/edge like any capability call, with no bespoke \
                          rejection classification to get wrong",
                },
            ),
            (
                Convention::ArgonParity,
                Stance::NotApplicable {
                    why: "match performs no password hashing — it has no credential \
                          material at all",
                },
            ),
        ],
    }
}
