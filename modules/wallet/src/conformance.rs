//! Minimal factual probes consumed by `tools/conformance` — each answers "does the
//! module's own validator reject a movement carrying this many bytes in this field?".
//!
//! Every probe routes through [`crate::validate_movement`], the ENFORCEMENT POINT every
//! `credit`/`debit` runs before ledger SQL, rather than the leaf `*_within_cap` predicate
//! underneath it: a probe on the leaf stays green when the `if !…` branch that consults it
//! is deleted from the validator, which is exactly the shape `e3ddfef` closed for apikeys.
//! Only the field under test is oversized, so the rejection can come from no other branch.
//!
//! The portal writer's probes run through `WalletAdmin::invoke` itself against a store that
//! cannot connect, the way mail's do: a value the writer admits reaches the pool and answers
//! `Internal`, so `Invalid` (or, for the create form, the field's own error) can only come
//! from the guard under test.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use bus::Bus;
use controlplane::{InvokeCall, InvokeOutcome, Provider};
use opsapi::Error;
use sqlx::PgPool;
use walletapi::Movement;

use crate::admin::{
    WalletAdmin, ADMIN_KEY_PREFIX, CURRENCY_CREATE, FIELD_AMOUNT, FIELD_CODE, FIELD_DECIMALS, FIELD_KIND, FIELD_NAME,
    FIELD_PLAYER, FIELD_REASON, LEDGER_GRANT,
};
use crate::Service;

/// The byte cap the provider applies to every `cp.invoke` input value.
pub const CP_INVOKE_INPUT_VALUE_BYTES: usize = controlplane::MAX_INPUT_VALUE_BYTES;

/// The create form's Name and Kind caps, module-private in `service` (no wire method carries
/// them) and named here for the conformance rows.
pub const CREATE_NAME_BYTES: usize = crate::service::MAX_CURRENCY_DISPLAY_NAME_BYTES;
pub const CREATE_KIND_BYTES: usize = crate::service::MAX_CURRENCY_KIND_BYTES;

/// A canonical uuid the stand-in directory answers as an existing player.
const PROBE_PLAYER: &str = "00000000-0000-4000-8000-000000000001";
const PROBE_AMOUNT: &str = r#"{"amount":"1","currency":"gold"}"#;
const PROBE_REASON: &str = "conformance";
const PROBE_REQUEST_ID: &str = "conformance-probe";

fn movement_with(field: fn(&mut Movement, String), len: usize) -> Movement {
    let mut m = Movement {
        idempotency_key: "conformance-key".into(),
        player_id: "conformance-player".into(),
        currency: "gold".into(),
        amount: 1,
        reason: "conformance".into(),
    };
    field(&mut m, "a".repeat(len));
    m
}

fn rejected(field: fn(&mut Movement, String), len: usize) -> bool {
    crate::validate_movement(&movement_with(field, len)).is_err()
}

#[doc(hidden)]
pub fn conformance_idempotency_key_rejected(len: usize) -> bool {
    rejected(|m, v| m.idempotency_key = v, len)
}

#[doc(hidden)]
pub fn conformance_currency_code_rejected(len: usize) -> bool {
    rejected(|m, v| m.currency = v, len)
}

#[doc(hidden)]
pub fn conformance_reason_rejected(len: usize) -> bool {
    rejected(|m, v| m.reason = v, len)
}

/// The provider's own first check on every `cp.invoke`, executed through
/// `WalletAdmin::invoke`. Every field of the grant is valid and the player exists, so at
/// the cap the call reaches `Service::credit` and answers `Internal` off the dead pool; one
/// byte over, the cap's `Invalid` is the only refusal it can meet. Deleting the guard sends
/// the long value to the pool too, and the case goes red.
#[doc(hidden)]
pub fn conformance_cp_invoke_input_rejected(len: usize) -> bool {
    let mut input = grant_input();
    input.insert("value".to_string(), "a".repeat(len));
    refused(call(input, PROBE_REQUEST_ID.to_string()))
}

/// The portal-minted movement key `admin:<request id>`, `len` bytes long in all, executed
/// through `WalletAdmin::invoke`. The bound is `validate_movement`'s key cap, which
/// `Service::credit` runs before it opens a transaction: a key within it reaches the dead
/// pool and answers `Internal`, a longer one is `Invalid` before it.
#[doc(hidden)]
pub fn conformance_admin_key_rejected(len: usize) -> bool {
    let request_id = "a".repeat(len.saturating_sub(ADMIN_KEY_PREFIX.len()));
    refused(call(grant_input(), request_id))
}

/// The create form's Code, `len` bytes, executed through `WalletAdmin::invoke`.
#[doc(hidden)]
pub fn conformance_create_code_rejected(len: usize) -> bool {
    create_refused(FIELD_CODE, len)
}

/// The create form's Name, `len` bytes, executed through `WalletAdmin::invoke`.
#[doc(hidden)]
pub fn conformance_create_name_rejected(len: usize) -> bool {
    create_refused(FIELD_NAME, len)
}

/// The create form's Kind, `len` bytes, executed through `WalletAdmin::invoke`.
#[doc(hidden)]
pub fn conformance_create_kind_rejected(len: usize) -> bool {
    create_refused(FIELD_KIND, len)
}

/// A create every rule of the form accepts but `key`, which is `len` bytes. Within the cap
/// the insert reaches the dead pool and answers `Internal`; over it, the field's own error is
/// the only refusal it can meet. Deleting the field's cap sends the long value to the pool too.
fn create_refused(key: &str, len: usize) -> bool {
    let mut input = BTreeMap::from([
        (FIELD_CODE.to_string(), "conformance".to_string()),
        (FIELD_NAME.to_string(), "Conformance".to_string()),
        (FIELD_KIND.to_string(), "soft".to_string()),
        (FIELD_DECIMALS.to_string(), "0".to_string()),
    ]);
    input.insert(key.to_string(), "a".repeat(len));
    let call = InvokeCall { action: CURRENCY_CREATE.to_string(), request_id: None, ..call(input, String::new()) };
    matches!(run(call), Ok(InvokeOutcome::FieldErrors(errors)) if errors.contains_key(key))
}

/// A Grant-or-revoke submit every rule of the form accepts.
fn grant_input() -> BTreeMap<String, String> {
    BTreeMap::from([
        (FIELD_PLAYER.to_string(), PROBE_PLAYER.to_string()),
        (FIELD_AMOUNT.to_string(), PROBE_AMOUNT.to_string()),
        (FIELD_REASON.to_string(), PROBE_REASON.to_string()),
    ])
}

fn call(input: BTreeMap<String, String>, request_id: String) -> InvokeCall {
    InvokeCall {
        module: "wallet".to_string(),
        action: LEDGER_GRANT.to_string(),
        entity: None,
        input,
        expected_revision: None,
        reason: None,
        operator: "conformance".to_string(),
        item: None,
        request_id: Some(request_id),
    }
}

/// The accounts `Directory` stand-in: every id names a player, so the directory check never
/// decides a probe.
struct EveryPlayer;

#[async_trait]
impl accountsapi::Directory for EveryPlayer {
    async fn players_by_id(&self, ids: Vec<String>) -> Result<Vec<accountsapi::PlayerSummary>, Error> {
        Ok(ids
            .into_iter()
            .map(|player_id| accountsapi::PlayerSummary { player_id, ..Default::default() })
            .collect())
    }

    async fn find_by_handle(&self, _handle: String) -> Result<Option<accountsapi::PlayerSummary>, Error> {
        Ok(None)
    }
}

fn refused(call: InvokeCall) -> bool {
    matches!(run(call), Err(e) if e.status == opsapi::Status::Invalid)
}

fn run(call: InvokeCall) -> Result<InvokeOutcome, Error> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime");
    let provider = {
        let _guard = rt.enter();
        let svc = Service::new(dead_pool(), Arc::new(Bus::new()), false);
        svc.directory
            .set(Arc::new(EveryPlayer) as Arc<dyn accountsapi::Directory>)
            .unwrap_or_else(|_| panic!("a fresh service has no directory"));
        WalletAdmin::new(Arc::new(svc), "gold".to_string())
    };
    rt.block_on(provider.invoke(call))
}

/// A pool pointed at a port nothing listens on, with the acquire wait bounded so a probe
/// that reaches the store fails fast instead of sitting out sqlx's 30-second default.
fn dead_pool() -> PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_millis(250))
        .connect_lazy(DEAD_DSN)
        .expect("lazy pool from a well-formed DSN")
}

const DEAD_DSN: &str = "postgres://gamebackend:gamebackend@127.0.0.1:1/gamebackend?sslmode=disable";
