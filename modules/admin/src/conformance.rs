//! This module's convention-conformance entry — the explicit stance `admin`
//! declares for every [`conformance::Convention`], executed by the
//! `conformancecheck` harness (`tools/conformance`). Always compiled (the
//! `asyncevents::testing` precedent — no feature flags); nothing here runs on a
//! production path.

use std::sync::Arc;

use conformance::{ArgonParams, CapCase, Convention, Entry, Fixture, Stance};

use crate::{password_within_cap, username_within_cap};

/// The `admin` conformance entry. Stances per the decided matrix
/// (`docs/plans/2026-07-12-0952-convention-conformance-harness-plan.md`).
pub fn entry() -> Entry {
    let (m_cost, t_cost, p_cost, output_len) = crate::argon2_params_for_parity_test();
    Entry {
        module: "admin",
        stances: vec![
            (
                Convention::EnvValidation,
                Stance::NotApplicable {
                    why: "ADMIN_COOKIE_SECURE and ADMIN_OPEN are boolean behavior gates \
                          (unset stays secure/closed), not parsed values; no numeric env \
                          is parsed at init that could be silently defaulted",
                },
            ),
            (
                Convention::InputByteCaps,
                Stance::Applies(Fixture::InputByteCaps(vec![
                    CapCase {
                        name: "admin login username",
                        cap: crate::MAX_USERNAME_BYTES,
                        probe: Arc::new(|len| !username_within_cap(&"a".repeat(len))),
                    },
                    CapCase {
                        name: "admin login password",
                        cap: crate::MAX_PASSWORD_BYTES,
                        probe: Arc::new(|len| !password_within_cap(&"a".repeat(len))),
                    },
                ])),
            ),
            (
                Convention::InfraOutage503,
                Stance::NotApplicable {
                    why: "admin has no infrastructure-backed credential verifier of its \
                          own: login checks the local admin schema directly, and a remote \
                          adminData fan-out failure renders as an error card on the page, \
                          never as an auth classification",
                },
            ),
            (
                Convention::ArgonParity,
                Stance::Applies(Fixture::ArgonParity(ArgonParams {
                    m_cost,
                    t_cost,
                    p_cost,
                    output_len,
                })),
            ),
        ],
    }
}
