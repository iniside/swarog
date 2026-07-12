//! This module's convention-conformance entry — the explicit stance `inventory`
//! declares for every [`conformance::Convention`], executed by the
//! `conformancecheck` harness (`tools/conformance`). Always compiled (the
//! `asyncevents::testing` precedent — no feature flags); nothing here runs on a
//! production path.

use conformance::{Convention, Entry, Stance};

/// The `inventory` conformance entry. Stances per the decided matrix
/// (`docs/plans/2026-07-12-0952-convention-conformance-harness-plan.md`) — all
/// four conventions are NotApplicable for this module.
pub fn entry() -> Entry {
    Entry {
        module: "inventory",
        stances: vec![
            (
                Convention::EnvValidation,
                Stance::NotApplicable {
                    why: "INVENTORY_DEV_GRANT is a boolean presence-gate that fails \
                          closed by absence (`env_bool`, mirroring Go's envBool); no \
                          numeric or otherwise-parseable env value is read at init that \
                          could be silently defaulted",
                },
            ),
            (
                Convention::InputByteCaps,
                Stance::NotApplicable {
                    why: "inventory's player-facing operations (list_mine/list_character/ \
                          grant) take ids and item references, not free-text fields — \
                          there is no player-supplied string to cap",
                },
            ),
            (
                Convention::InfraOutage503,
                Stance::NotApplicable {
                    why: "inventory's sync dependency on characters' Ownership resolves \
                          over the registry/edge like any capability call and surfaces \
                          ordinary opsapi errors — it has no bespoke auth verifier that \
                          could misclassify an outage as a rejection",
                },
            ),
            (
                Convention::ArgonParity,
                Stance::NotApplicable {
                    why: "inventory performs no password hashing — it has no credential \
                          material at all",
                },
            ),
        ],
    }
}
