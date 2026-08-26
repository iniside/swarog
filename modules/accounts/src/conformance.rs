//! Minimal factual probes consumed by `tools/conformance`.
//!
//! Policy and expected outcomes live in the tool. These probes only exercise
//! the same production validators and service path used by real requests.

use std::sync::{Arc, OnceLock};

use accountsapi::Auth as _;
use sqlx::PgPool;
use tokio::sync::Semaphore;

use crate::password::ArgonVerifier;
use crate::providers::{Providers, Resolution, EPIC, GOOGLE, GUEST};
use crate::store::Store;
use crate::{
    credential_within_cap, display_name_within_cap, email_within_cap,
    password_within_cap, provider_name_within_cap, session_token_within_cap, Service,
};

/// Re-exported so `tools/conformance` states these caps by reference instead of a
/// second literal — the definition sites (`providers.rs`, `guest.rs`, `lib.rs`) stay
/// the only ones.
pub use crate::guest::MAX_GUEST_CREDENTIAL_BYTES;
pub use crate::providers::MAX_OIDC_CREDENTIAL_BYTES;
pub use crate::MAX_PROVIDER_NAME_BYTES;

/// Every provider name this build can verify — the naming authority itself, so the
/// tool's per-provider cap list is diffed against the real list rather than a copy.
pub const KNOWN_PROVIDERS: &[&str] = crate::providers::KNOWN_PROVIDERS;

/// The credential cap of every verifier the PRODUCTION registry construction yields,
/// keyed by provider name.
///
/// `login_federated`'s credential cap is per-provider (`CredentialVerifier::
/// max_credential_bytes`), but one wire field carries every provider's credential, so
/// the input-policy row can only state one number. This is the factual list that keeps
/// that row honest: the tool diffs it against [`KNOWN_PROVIDERS`] (a provider whose
/// verifier the fixture cannot build is a finding) and against its own reviewed
/// per-provider table (a changed or new bound is a finding), so a second verifier
/// cannot get a different bound while the OIDC-shaped policy row stays green.
///
/// The registry is built through the real `from_vars -> providers` path with a LAZY
/// pool: `PgPool::connect_lazy` performs no I/O, and no verifier is invoked here.
pub fn credential_caps() -> std::collections::BTreeMap<String, usize> {
    with_registry(|providers| providers.credential_caps())
}

/// The environment the conformance registry is built from — one variable per provider
/// family, the rest left to the parse's own defaults.
///
/// Hand-written, and therefore self-checked below: every key
/// [`crate::providers::provider_env_keys`] names belongs to some provider's family
/// (the segment before the first `_`), and a family with no fixture value here is a
/// provider `ProviderConfig::providers` never registers — which would leave its
/// verifier's cap unmeasured while every conformance assertion stayed green.
fn fixture_vars() -> std::collections::BTreeMap<String, String> {
    let vars: std::collections::BTreeMap<String, String> = [
        ("EPIC_CLIENT_ID", "conformance-epic-client"),
        ("GOOGLE_CLIENT_IDS", "conformance-google-client"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    let families: std::collections::BTreeSet<&str> =
        vars.keys().map(|key| env_family(key)).collect();
    for key in crate::providers::provider_env_keys() {
        assert!(
            families.contains(env_family(key)),
            "accounts::conformance::fixture_vars configures no value for the {} provider \
             environment family (from {key}), so the conformance registry would never build \
             its verifier and its credential cap would go unmeasured",
            env_family(key)
        );
    }
    vars
}

/// A provider environment variable's family: the segment before the first `_`
/// (`EPIC_CLIENT_ID` -> `EPIC`), the grouping `EPIC_VARS`/`GOOGLE_VARS` already use.
fn env_family(key: &str) -> &str {
    key.split_once('_').map_or(key, |(family, _)| family)
}

/// Runs `f` with the registry the PRODUCTION `from_vars -> providers` path yields from
/// [`fixture_vars`], on a lazy pool. No I/O: `PgPool::connect_lazy` opens no connection
/// and no verifier is invoked here.
fn with_registry<T>(f: impl FnOnce(&Providers) -> T) -> T {
    let vars = fixture_vars();
    with_lazy_pool(|pool| {
        let providers = crate::providers::ProviderConfig::from_vars(&vars)
            .expect("the conformance provider fixture must be a valid configuration")
            .providers(pool);
        f(&providers)
    })
}

/// Whether `credential` of `len` bytes is rejected by the cap of the provider the real
/// registry resolves under `provider` — the same `credential_within_cap` call
/// `login_federated` makes on the verifier it resolved, never a hand-built one, so each
/// CapCase executes exactly one provider's own bound.
fn registry_credential_rejected(provider: &str, len: usize) -> bool {
    with_registry(|providers| match providers.resolve(provider) {
        Resolution::Configured(verifier) => {
            !credential_within_cap(verifier.as_ref(), &"a".repeat(len))
        }
        _ => panic!(
            "the conformance registry configured no {provider} verifier — its credential cap \
             cannot be executed"
        ),
    })
}

/// Runs `f` with a lazy pool. `PgPool::connect_lazy` opens no connection but does
/// register its idle reaper on the current Tokio runtime, so these SYNCHRONOUS probes
/// supply one; the pool is dropped inside `f`'s frame, before the runtime.
fn with_lazy_pool<T>(f: impl FnOnce(&PgPool) -> T) -> T {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime");
    let _guard = rt.enter();
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let pool = PgPool::connect_lazy(&dsn).expect("lazy pool from a well-formed DSN");
    f(&pool)
}

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

/// Epic's credential cap, executed through the verifier the production registry
/// resolves for `epic`.
#[doc(hidden)]
pub fn conformance_epic_credential_rejected(len: usize) -> bool {
    registry_credential_rejected(EPIC, len)
}

/// Google's credential cap. Its own CapCase rather than epic's: the two happen to share
/// a bound today, so a single shared case would leave one of them never executed.
#[doc(hidden)]
pub fn conformance_google_credential_rejected(len: usize) -> bool {
    registry_credential_rejected(GOOGLE, len)
}

/// The guest credential cap — a different bound from the OIDC one, checked by the same
/// `credential_within_cap` the handler calls.
#[doc(hidden)]
pub fn conformance_guest_credential_rejected(len: usize) -> bool {
    registry_credential_rejected(GUEST, len)
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
