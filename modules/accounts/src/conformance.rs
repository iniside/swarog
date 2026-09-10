//! Minimal factual probes consumed by `tools/conformance`.
//!
//! Policy and expected outcomes live in the tool. These probes only exercise
//! the same production validators and service path used by real requests.

use std::sync::{Arc, OnceLock};

use accountsapi::Auth as _;
use opsapi::Identity;
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
pub use crate::{MAX_DELETE_TICKET_BYTES, MAX_PROVIDER_NAME_BYTES};

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
    service_on(PgPool::connect_lazy(&dsn).expect("lazy pool from a well-formed DSN"))
}

fn service_on(pool: PgPool) -> Service {
    Service {
        store: Store { pool },
        bus: Arc::new(bus::Bus::new()),
        dev_auth: false,
        providers: OnceLock::new(),
        argon_permits: Arc::new(Semaphore::new(2)),
        login_slots: Arc::new(Semaphore::new(32)),
        verifier: Arc::new(ArgonVerifier),
    }
}

/// [`service_without_providers`] with the registry the PRODUCTION `from_vars ->
/// providers` path yields, so `guest` resolves `Configured` and the `link` credential
/// probe reaches the resolved verifier's own cap instead of stopping at the
/// KnownButUnconfigured arm.
fn service_with_providers() -> Service {
    let svc = service_without_providers();
    let providers = crate::providers::ProviderConfig::from_vars(&fixture_vars())
        .expect("the conformance provider fixture must be a valid configuration")
        .providers(&svc.store.pool);
    assert!(
        svc.providers.set(Arc::new(providers)).is_ok(),
        "the conformance service's provider cell must be fresh"
    );
    svc
}

/// Whether the REAL `accountsapi::Auth::link` op answers `msg` (400-class) for
/// `(provider, credential)`. Drives the op itself, not the helpers under it: the caps
/// `link` states live in `verify_credential`, which `link_identity` — the entry point
/// every accounts test calls — sits BELOW, so a probe on the helper would stay green
/// with the op's guards deleted.
///
/// The service is built inside the runtime context (`PgPool::connect_lazy` registers an
/// idle reaper) but `block_on` runs outside it, and is dropped before the runtime.
/// No case here reaches a store call: an over-cap value is decided by the guard, and the
/// at-cap value is decided by an unknown provider name / a wrong-shaped guest ticket.
fn link_answers(svc: impl FnOnce() -> Service, provider: &str, credential: &str, msg: &str) -> bool {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime");
    let svc = {
        let _guard = rt.enter();
        svc()
    };
    let outcome = rt.block_on(svc.link(
        Identity::player("conformance-link-probe"),
        provider.to_string(),
        credential.to_string(),
    ));
    matches!(outcome, Err(error) if error.status.http() == 400 && error.msg == msg)
}

/// The provider-name cap of `accounts.link`, executed through the op.
#[doc(hidden)]
pub fn conformance_link_provider_rejected(len: usize) -> bool {
    link_answers(
        service_without_providers,
        &"a".repeat(len),
        "conformance.probe.ticket",
        "provider too long",
    )
}

/// The credential cap of `accounts.link`, executed through the op against the guest
/// verifier the production registry resolves — the cap is per-provider, so this states
/// guest's own bound. An at-cap value carries no `.` and is `Rejected` by shape, so the
/// case stays zero-I/O.
#[doc(hidden)]
pub fn conformance_link_credential_rejected(len: usize) -> bool {
    link_answers(
        service_with_providers,
        GUEST,
        &"a".repeat(len),
        "credential too long",
    )
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

/// Whether `accountsapi::Auth::refresh` answers 401 for a refresh token of `len` bytes,
/// driven through the op on a pool that CANNOT connect.
///
/// The dead pool is what makes the cap executable. `refresh` has no cheap second
/// rejection: an at-cap token is unknown, and an unknown token is also a 401 — so on a
/// live pool this probe would answer identically with the guard deleted. Against a pool
/// whose only connection attempt fails, the 401 can be produced ONLY before the store is
/// touched: at the cap the probe reaches the rotation and gets Internal (false, as
/// required), over the cap it is rejected by the guard (true). Delete the guard and the
/// over-cap case becomes Internal too, turning this case red.
#[doc(hidden)]
pub fn conformance_refresh_token_rejected(len: usize) -> bool {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime");
    let svc = {
        let _guard = rt.enter();
        service_on(dead_pool())
    };
    let outcome = rt.block_on(svc.refresh("a".repeat(len)));
    matches!(outcome, Err(error) if error.status == opsapi::Status::Unauthorized)
}

/// A pool pointed at a port nothing listens on, with the acquire wait bounded so the
/// at-cap case fails fast instead of sitting out sqlx's 30-second default.
fn dead_pool() -> PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_millis(250))
        .connect_lazy(DEAD_DSN)
        .expect("lazy pool from a well-formed DSN")
}

const DEAD_DSN: &str = "postgres://gamebackend:gamebackend@127.0.0.1:1/gamebackend?sslmode=disable";

/// Whether `accountsapi::Auth::delete_account` rejects a ticket of `len` bytes, driven
/// through the op on a pool that CANNOT connect — the `refresh` case's shape, and for the
/// same reason: an at-cap ticket is simply not a live one, and an unknown ticket is also a
/// 404, so on a live pool this probe would answer identically with the guard deleted.
/// Against a dead pool only the guard can produce a 400: at the cap the op reaches the
/// store and answers Internal (false), over the cap the guard rejects it (true).
#[doc(hidden)]
pub fn conformance_delete_ticket_rejected(len: usize) -> bool {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime");
    let svc = {
        let _guard = rt.enter();
        service_on(dead_pool())
    };
    let outcome = rt.block_on(svc.delete_account(
        Identity::player("00000000-0000-0000-0000-000000000000"),
        "a".repeat(len),
    ));
    matches!(outcome, Err(error) if error.status == opsapi::Status::Invalid)
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
