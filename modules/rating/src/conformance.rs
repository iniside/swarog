//! This module's convention-conformance entry — the explicit stance `rating`
//! declares for every [`conformance::Convention`], executed by the
//! `conformancecheck` harness (`tools/conformance`). Always compiled (the
//! `asyncevents::testing` precedent — no feature flags); nothing here runs on a
//! production path.

use conformance::{Convention, Entry, Stance};

/// The `rating` conformance entry. Stances per the decided matrix
/// (`docs/plans/2026-07-12-0952-convention-conformance-harness-plan.md`) — all
/// four conventions are NotApplicable for this module.
pub fn entry() -> Entry {
    Entry {
        module: "rating",
        stances: vec![
            (
                Convention::EnvValidation,
                Stance::NotApplicable {
                    why: "rating parses no env vars at all — its MMR projection is \
                          driven entirely by the durable match.finished subscription \
                          and DB state, never process env",
                },
            ),
            (
                Convention::InputByteCaps,
                Stance::NotApplicable {
                    why: "rating's MmrReader capability takes a player id, not \
                          free-text — there is no player-supplied string field to cap",
                },
            ),
            (
                Convention::InfraOutage503,
                Stance::NotApplicable {
                    why: "rating's MmrReader is a direct DB read with no external \
                          verifier or infra dependency to misclassify on outage",
                },
            ),
            (
                Convention::ArgonParity,
                Stance::NotApplicable {
                    why: "rating performs no password hashing — it has no credential \
                          material at all",
                },
            ),
        ],
    }
}
