//! This module's convention-conformance entry — the explicit stance `characters`
//! declares for every [`conformance::Convention`], executed by the
//! `conformancecheck` harness (`tools/conformance`). Always compiled (the
//! `asyncevents::testing` precedent — no feature flags); nothing here runs on a
//! production path.

use conformance::{Convention, Entry, Stance};

/// The `characters` conformance entry. Stances per the decided matrix
/// (`docs/plans/2026-07-12-0952-convention-conformance-harness-plan.md`) — all
/// four conventions are NotApplicable for this module.
pub fn entry() -> Entry {
    Entry {
        module: "characters",
        stances: vec![
            (
                Convention::EnvValidation,
                Stance::NotApplicable {
                    why: "characters parses no env vars at all — its player-facing \
                          operations, schema DDL, and durable event emission are driven \
                          entirely by request input and DB state, never process env",
                },
            ),
            (
                Convention::InputByteCaps,
                Stance::NotApplicable {
                    why: "character creation takes no free-text field from the player \
                          today (identity is the verified caller, not client-supplied \
                          text) — there is no string input to cap",
                },
            ),
            (
                Convention::InfraOutage503,
                Stance::NotApplicable {
                    why: "characters provides the Ownership capability as a direct DB \
                          read with no external verifier or infra dependency to \
                          misclassify on outage",
                },
            ),
            (
                Convention::ArgonParity,
                Stance::NotApplicable {
                    why: "characters performs no password hashing — it has no credential \
                          material at all",
                },
            ),
        ],
    }
}
