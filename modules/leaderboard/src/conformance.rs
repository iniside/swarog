//! This module's convention-conformance entry — the explicit stance `leaderboard`
//! declares for every [`conformance::Convention`], executed by the
//! `conformancecheck` harness (`tools/conformance`). Always compiled (the
//! `asyncevents::testing` precedent — no feature flags); nothing here runs on a
//! production path.

use conformance::{Convention, Entry, Stance};

/// The `leaderboard` conformance entry. Stances per the decided matrix
/// (`docs/plans/2026-07-12-0952-convention-conformance-harness-plan.md`) — all
/// four conventions are NotApplicable for this module.
pub fn entry() -> Entry {
    Entry {
        module: "leaderboard",
        stances: vec![
            (
                Convention::EnvValidation,
                Stance::NotApplicable {
                    why: "leaderboard parses no env vars at all — its win tally is \
                          driven entirely by the durable match.finished subscription \
                          and the GET /leaderboard read, never process env",
                },
            ),
            (
                Convention::InputByteCaps,
                Stance::NotApplicable {
                    why: "leaderboard takes no player-supplied free-text field — the \
                          player id it tallies against arrives inside the \
                          match.finished event payload, not a client request",
                },
            ),
            (
                Convention::InfraOutage503,
                Stance::NotApplicable {
                    why: "leaderboard has no auth verifier or sync capability that \
                          classifies outages — GET /leaderboard is a plain DB-backed \
                          read with no rejection classification to get wrong",
                },
            ),
            (
                Convention::ArgonParity,
                Stance::NotApplicable {
                    why: "leaderboard performs no password hashing — it has no \
                          credential material at all",
                },
            ),
        ],
    }
}
