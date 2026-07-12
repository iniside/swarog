//! This module's convention-conformance entry — the explicit stance `scheduler`
//! declares for every [`conformance::Convention`], executed by the
//! `conformancecheck` harness (`tools/conformance`). Always compiled (the
//! `asyncevents::testing` precedent — no feature flags); nothing here runs on a
//! production path.

use conformance::{Convention, Entry, Stance};

/// The `scheduler` conformance entry. Stances per the decided matrix
/// (`docs/plans/2026-07-12-0952-convention-conformance-harness-plan.md`) — all
/// four conventions are NotApplicable for this module.
pub fn entry() -> Entry {
    Entry {
        module: "scheduler",
        stances: vec![
            (
                Convention::EnvValidation,
                Stance::NotApplicable {
                    why: "SCHEDULER_ENABLED is a boolean gate; the tick interval is DB \
                          data (`scheduler.schedules.interval_seconds`), not env — no \
                          numeric env value is parsed at init",
                },
            ),
            (
                Convention::InputByteCaps,
                Stance::NotApplicable {
                    why: "schedules are admin/operator-inserted rows into \
                          scheduler.schedules, not player-supplied free text — there is \
                          no player input path to cap",
                },
            ),
            (
                Convention::InfraOutage503,
                Stance::NotApplicable {
                    why: "scheduler has no auth verifier — it is a durable event SOURCE \
                          with a liveness probe folded into /readyz, not a rejection \
                          classification to get wrong",
                },
            ),
            (
                Convention::ArgonParity,
                Stance::NotApplicable {
                    why: "scheduler performs no password hashing — it has no credential \
                          material at all",
                },
            ),
        ],
    }
}
