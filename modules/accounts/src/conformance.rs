//! This module's convention-conformance entry — the explicit stance `accounts`
//! declares for every [`conformance::Convention`], executed by the
//! `conformancecheck` harness (`tools/conformance`). Always compiled (the
//! `asyncevents::testing` precedent — no feature flags, so the harness can import
//! the probes as a plain dependency); nothing here runs on a production path.

use std::sync::{Arc, OnceLock};

use accountsapi::Auth as _;
use conformance::{
    ArgonParams, CapCase, Convention, Entry, Fixture, OutageCase, OutageClass, Stance,
};
use sqlx::PgPool;
use tokio::sync::Semaphore;

use crate::password::ArgonVerifier;
use crate::store::Store;
use crate::{email_within_cap, password_within_cap, Service};

/// Fallback DSN for the probe's LAZY pool. Never connected: `login_epic` with an
/// unconfigured provider answers before any store access (see [`probe_service`]).
const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

/// A [`Service`] with a lazy pool (no I/O at construction) and an EMPTY epic
/// `OnceLock` — the NO-network "unconfigured identity provider" outage fixture
/// (the `modules/accounts/src/tests.rs` `lazy_service` shape). Deliberately NOT an
/// `OidcVerifier` pointed at a dead URL: that would be real I/O in a probe.
fn probe_service() -> Service {
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    Service {
        store: Store {
            pool: PgPool::connect_lazy(&dsn).expect("lazy pool from a well-formed DSN"),
        },
        bus: Arc::new(bus::Bus::new()),
        dev_auth: false,
        epic: OnceLock::new(),
        argon_permits: Arc::new(Semaphore::new(2)),
        login_slots: Arc::new(Semaphore::new(32)),
        verifier: Arc::new(ArgonVerifier),
    }
}

/// The `accounts` conformance entry. Stances per the decided matrix
/// (`docs/plans/2026-07-12-0952-convention-conformance-harness-plan.md`).
pub fn entry() -> Entry {
    let (m_cost, t_cost, p_cost, output_len) = crate::argon2_params_for_parity_test();
    Entry {
        module: "accounts",
        stances: vec![
            (
                Convention::EnvValidation,
                Stance::NotApplicable {
                    why: "accounts env is presence-gates only (EPIC_CLIENT_ID enables the \
                          provider, ACCOUNTS_DEV_AUTH is a boolean opt-in): absence fails \
                          closed, and no numeric value is parsed at init that could be \
                          silently defaulted",
                },
            ),
            (
                Convention::InputByteCaps,
                Stance::Applies(Fixture::InputByteCaps(vec![
                    CapCase {
                        name: "accounts register/login email",
                        cap: crate::MAX_EMAIL_BYTES,
                        probe: Arc::new(|len| !email_within_cap(&"a".repeat(len))),
                    },
                    CapCase {
                        name: "accounts register/login password",
                        cap: crate::MAX_PASSWORD_BYTES,
                        probe: Arc::new(|len| !password_within_cap(&"a".repeat(len))),
                    },
                ])),
            ),
            (
                Convention::InfraOutage503,
                Stance::Applies(Fixture::InfraOutage503(vec![OutageCase {
                    name: "accounts loginEpic with an unconfigured epic provider",
                    probe: Arc::new(|| {
                        Box::pin(async {
                            let svc = probe_service();
                            match svc.login_epic("conformance.probe.jwt".into()).await {
                                Err(e) if e.status == opsapi::Status::Unavailable => {
                                    OutageClass::Unavailable
                                }
                                Err(e) if e.status == opsapi::Status::Unauthorized => {
                                    OutageClass::Rejected
                                }
                                Err(e) => OutageClass::Other(format!(
                                    "unexpected error status {:?}: {}",
                                    e.status, e.msg
                                )),
                                Ok(_) => OutageClass::Other(
                                    "login_epic succeeded with no provider configured".into(),
                                ),
                            }
                        })
                    }),
                }])),
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
