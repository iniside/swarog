//! This module's convention-conformance entry — the explicit stance `config`
//! declares for every [`conformance::Convention`], executed by the
//! `conformancecheck` harness (`tools/conformance`). Always compiled (the
//! `asyncevents::testing` precedent — no feature flags); nothing here runs on a
//! production path.

use conformance::{Convention, Entry, Stance};

/// The `config` conformance entry. Stances per the decided matrix
/// (`docs/plans/2026-07-12-0952-convention-conformance-harness-plan.md`) — all
/// four conventions are NotApplicable for this module.
pub fn entry() -> Entry {
    Entry {
        module: "config",
        stances: vec![
            (
                Convention::EnvValidation,
                Stance::NotApplicable {
                    why: "config parses no env vars at all — its namespaced settings are \
                          admin/operator-supplied via the admin portal or raw SQL against \
                          `config.settings`, not process env",
                },
            ),
            (
                Convention::InputByteCaps,
                Stance::NotApplicable {
                    why: "config values are admin/operator-supplied via the admin portal \
                          or SQL, not player input — there is no player-facing free-text \
                          field to cap",
                },
            ),
            (
                Convention::InfraOutage503,
                Stance::NotApplicable {
                    why: "config's reader capability is a local cache swap driven by the \
                          invalidation plane, not an auth verifier — there is no \
                          rejection classification to get wrong on outage",
                },
            ),
            (
                Convention::ArgonParity,
                Stance::NotApplicable {
                    why: "config performs no password hashing — it has no credential \
                          material at all",
                },
            ),
        ],
    }
}
