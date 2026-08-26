//! Minimal factual probes consumed by `tools/conformance`.
//!
//! Policy and expected outcomes live in the tool. These probes only exercise
//! the same production validators and service path used by real requests.

use std::sync::{Arc, OnceLock};

use accountsapi::Auth as _;
use sqlx::PgPool;
use tokio::sync::Semaphore;

use crate::oidc::{IssuerMatch, OidcVerifier};
use crate::password::ArgonVerifier;
use crate::providers::{oidc_credentials, EPIC};
use crate::store::Store;
use crate::{
    credential_within_cap, display_name_within_cap, email_within_cap,
    password_within_cap, provider_name_within_cap, session_token_within_cap, Service,
};

/// Re-exported so `tools/conformance` states these caps by reference instead of a
/// second literal — the definition sites (`providers.rs`, `lib.rs`) stay the only ones.
pub use crate::providers::MAX_OIDC_CREDENTIAL_BYTES;
pub use crate::MAX_PROVIDER_NAME_BYTES;

const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

fn service_without_providers() -> Service {
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    Service {
        store: Store {
            pool: PgPool::connect_lazy(&dsn).expect("lazy pool from a well-formed DSN"),
        },
        bus: Arc::new(bus::Bus::new()),
        dev_auth: false,
        providers: OnceLock::new(),
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
pub fn conformance_display_name_rejected(len: usize) -> bool {
    !display_name_within_cap(&"a".repeat(len))
}

/// The OIDC credential cap traversed through a provider's own `max_credential_bytes`
/// rather than a constant restated here.
#[doc(hidden)]
pub fn conformance_federated_credential_rejected(len: usize) -> bool {
    let verifier = oidc_credentials(
        EPIC,
        Arc::new(
            OidcVerifier::new(
                "https://conformance.invalid/jwks",
                IssuerMatch::prefix("issuer", "https://conformance.invalid").expect("valid issuer"),
                vec!["conformance-client".to_string()],
            )
            .expect("valid verifier configuration"),
        ),
    );
    !credential_within_cap(verifier.as_ref(), &"a".repeat(len))
}

#[doc(hidden)]
pub fn conformance_provider_name_rejected(len: usize) -> bool {
    !provider_name_within_cap(&"a".repeat(len))
}

#[doc(hidden)]
pub fn conformance_session_token_rejected(len: usize) -> bool {
    !session_token_within_cap(&"a".repeat(len))
}

/// `epic` is in `KNOWN_PROVIDERS`, so a service that configured nothing answers the
/// KnownButUnconfigured arm — the 503 this case pins, distinct from the 400 an
/// unknown name gets.
#[doc(hidden)]
pub async fn conformance_login_federated_unconfigured_provider(
) -> Result<accountsapi::Session, opsapi::Error> {
    service_without_providers()
        .login_federated(EPIC.to_string(), "conformance.probe.jwt".into())
        .await
}
