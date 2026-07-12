//! Minimal factual probes consumed by `tools/conformance`.
//!
//! Policy and expected outcomes live in the tool. These probes only exercise
//! the same production validators and service path used by real requests.

use std::sync::{Arc, OnceLock};

use accountsapi::Auth as _;
use sqlx::PgPool;
use tokio::sync::Semaphore;

use crate::password::ArgonVerifier;
use crate::store::Store;
use crate::{email_within_cap, password_within_cap, Service};

const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

fn service_without_epic_provider() -> Service {
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

#[doc(hidden)]
pub fn conformance_email_rejected(len: usize) -> bool {
    !email_within_cap(&"a".repeat(len))
}

#[doc(hidden)]
pub fn conformance_password_rejected(len: usize) -> bool {
    !password_within_cap(&"a".repeat(len))
}

#[doc(hidden)]
pub async fn conformance_login_epic_without_provider(
) -> Result<accountsapi::Session, opsapi::Error> {
    service_without_epic_provider()
        .login_epic("conformance.probe.jwt".into())
        .await
}
