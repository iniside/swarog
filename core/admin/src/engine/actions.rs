//! Actions: admission (role, `implemented`, the module's `writes`), the claimed form
//! instances and click claims of `admin.forms`, the posted-body decoding, the invoke call,
//! the outcome → HTTP mapping, the `admin.action` record and the show-once flash through
//! `admin.reveals`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use controlplane::{
    decode_field, encode_field, ActionDecl, ConfirmSpec, ControlPlane, EntityRef, Field, FieldKind, FormSpec,
    InvokeOutcome, Link, ReasonReq, SecretReveal, Then,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;

use super::manifests::Snapshot;
use super::roles::{self, Operator};
use crate::render::{encode, entity_url, link_url};
use crate::AdminState;

/// Bound on one invoke round trip; the edge has no response deadline of its own
/// (`core/edge/src/client.rs:113-154`).
pub(crate) const INVOKE_DEADLINE: Duration = Duration::from_secs(10);

/// A claim older than this is abandoned and may be taken over; it exceeds [`INVOKE_DEADLINE`].
pub(crate) const CLAIM_WINDOW: Duration = Duration::from_secs(30);
/// A click claim consumed less than this ago answers a repeated click.
pub(crate) const CLICK_REPEAT_WINDOW: Duration = Duration::from_secs(5);
/// Lifetime of a form instance or click row.
pub(crate) const FORM_TTL: Duration = Duration::from_secs(3600);

/// Portal-consumed body keys.
pub(crate) const FIELD_CSRF: &str = "_csrf";
pub(crate) const FIELD_FORM: &str = "_form";
pub(crate) const FIELD_ENTITY: &str = "_entity";
pub(crate) const FIELD_ITEM: &str = "_item";
pub(crate) const FIELD_REASON: &str = "_reason";
pub(crate) const FIELD_CONFIRM: &str = "_confirm";
pub(crate) const FIELD_RETURN: &str = "_return";

/// Upper bound, in bytes, of a bound element id.
pub(crate) const ITEM_MAX: usize = 256;

pub(crate) struct Found<'a> {
    pub(crate) decl: &'a ActionDecl,
    pub(crate) face: Arc<dyn ControlPlane>,
    /// The owning module's `Manifest.writes`.
    pub(crate) writes: bool,
}

pub(crate) fn find<'a>(snapshot: &'a Snapshot, module: &str, action: &str) -> Option<Found<'a>> {
    let (manifest, face) = snapshot.route(module)?;
    let decl = manifest.actions.iter().find(|a| a.id == action)?;
    Some(Found { decl, face, writes: manifest.writes })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    Denied,
    NotImplemented,
    ReadOnly,
}

impl Refusal {
    /// The disabled control's hint and the refused request's message.
    pub(crate) fn hint(self) -> &'static str {
        match self {
            Refusal::Denied => "Not permitted for your role",
            Refusal::NotImplemented => "Not implemented yet",
            Refusal::ReadOnly => "Read-only: this module does not accept edits from the portal",
        }
    }
}

/// Role first, then `implemented`, then the module's `writes`, which refuses every action
/// but a link; a refusal means no provider call.
pub(crate) fn admit(operator: &Operator, decl: &ActionDecl, writes: bool) -> Result<(), Refusal> {
    if !roles::allows(operator, decl.min_role) {
        return Err(Refusal::Denied);
    }
    if !decl.implemented {
        return Err(Refusal::NotImplemented);
    }
    if !writes && !matches!(decl.then, Then::Link(_)) {
        return Err(Refusal::ReadOnly);
    }
    Ok(())
}

pub(crate) fn parse_entity(raw: &str) -> Option<EntityRef> {
    let (kind, id) = raw.split_once(':')?;
    if kind.is_empty() || id.is_empty() {
        return None;
    }
    Some(EntityRef { kind: kind.to_string(), id: id.to_string() })
}

/// A bound element id the portal forwards: non-empty, at most [`ITEM_MAX`] bytes, no
/// control characters. Modules still check it against their own data.
pub(crate) fn validate_item(item: &str) -> bool {
    !item.is_empty() && item.len() <= ITEM_MAX && !item.chars().any(char::is_control)
}

/// The body key of a Money field's currency; modules never see it.
pub(crate) fn money_currency_key(key: &str) -> String {
    format!("{key}__currency")
}

/// The first value of `key` in a posted body.
pub(crate) fn first<'a>(body: &'a [(String, String)], key: &str) -> Option<&'a str> {
    body.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

// ---------------------------------------------------------------------------
// admin.forms rows
// ---------------------------------------------------------------------------

/// The `spec` of an `admin.forms` row: what the form instance rendered, or a click claim.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub(crate) enum StoredSpec {
    Form(FormSpec),
    Confirm(ConfirmSpec),
    Click,
}

/// One `admin.forms` row; timestamps are Postgres microseconds since the Unix epoch.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FormRow {
    pub(crate) id: String,
    pub(crate) login: String,
    pub(crate) module: String,
    pub(crate) action: String,
    /// Set for a panel form: the contribution whose panel renders it.
    pub(crate) contribution: Option<String>,
    pub(crate) entity: Option<EntityRef>,
    pub(crate) item: Option<String>,
    pub(crate) spec: StoredSpec,
    pub(crate) generation: i64,
    pub(crate) incarnation: String,
    pub(crate) claimed_at: Option<i64>,
    pub(crate) consumed_at: Option<i64>,
    pub(crate) expires_at: i64,
}

/// Who presents a form id or click: the session login and the URL's module and action.
#[derive(Clone, Debug)]
pub(crate) struct Claimant {
    pub(crate) login: String,
    pub(crate) module: String,
    pub(crate) action: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Claim {
    /// 400 with zero calls.
    Reject,
    /// The repeated-click answer with zero calls.
    Repeated,
    /// Claim once more.
    Retry,
}

fn micros(d: Duration) -> i64 {
    d.as_micros() as i64
}

fn matches_claimant(row: &FormRow, who: &Claimant) -> bool {
    row.login == who.login && row.module == who.module && row.action == who.action
}

/// Held: claimed within [`CLAIM_WINDOW`] of `now`, the inverse of the claims' takeover test.
fn held(row: &FormRow, now: i64) -> bool {
    row.claimed_at.is_some_and(|at| at >= now - micros(CLAIM_WINDOW))
}

/// Why a form claim returned no row, given that row (as read with Postgres `now`).
pub(crate) fn classify_form(row: Option<&FormRow>, who: &Claimant, now: i64) -> Claim {
    let Some(row) = row else {
        return Claim::Reject;
    };
    if row.spec == StoredSpec::Click || !matches_claimant(row, who) {
        return Claim::Reject;
    }
    if row.consumed_at.is_some() || held(row, now) {
        return Claim::Repeated;
    }
    if row.expires_at <= now {
        return Claim::Reject;
    }
    Claim::Retry
}

/// Why a click claim returned no row, given that row (as read with Postgres `now`).
pub(crate) fn classify_click(row: Option<&FormRow>, who: &Claimant, now: i64) -> Claim {
    let Some(row) = row else {
        return Claim::Retry;
    };
    if !matches_claimant(row, who) {
        return Claim::Reject;
    }
    let recently_consumed = row.consumed_at.is_some_and(|at| at >= now - micros(CLICK_REPEAT_WINDOW));
    if (row.consumed_at.is_none() && held(row, now)) || recently_consumed {
        return Claim::Repeated;
    }
    Claim::Retry
}

fn sha256_hex(parts: &[&str]) -> String {
    let digest = Sha256::digest(parts.join("\0").as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// The deterministic click-claim row id of one button for one operator.
pub(crate) fn click_row_id(who: &Claimant, entity: Option<&EntityRef>, item: Option<&str>) -> String {
    let entity = entity.map(|e| format!("{}:{}", e.kind, e.id)).unwrap_or_default();
    let hash = sha256_hex(&[&who.login, &who.module, &who.action, &entity, item.unwrap_or("")]);
    format!("click:{hash}")
}

/// A click's `request_id`: stable across a released claim's retry, new after a deliberate
/// repeat (`generation`) or a re-inserted row (`incarnation`).
pub(crate) fn click_request_id(row: &FormRow) -> String {
    format!("click:{}", sha256_hex(&[&row.id, &row.incarnation, &row.generation.to_string()]))
}

// ---------------------------------------------------------------------------
// Posted bodies
// ---------------------------------------------------------------------------

/// A token-less click posts only these keys, each at most once.
pub(crate) fn check_click_keys(body: &[(String, String)]) -> Result<(), String> {
    let allowed = [FIELD_CSRF, FIELD_ENTITY, FIELD_RETURN, FIELD_ITEM];
    check_keys(body, |key| allowed.contains(&key), |_| false)
}

/// A form instance posts the portal keys, its spec's field keys and each Money field's
/// currency key; only a Multi field's key repeats.
pub(crate) fn check_form_keys(spec: &StoredSpec, body: &[(String, String)]) -> Result<(), String> {
    let fields: &[Field] = match spec {
        StoredSpec::Form(form) => &form.fields,
        StoredSpec::Confirm(_) | StoredSpec::Click => &[],
    };
    let portal = [FIELD_CSRF, FIELD_FORM, FIELD_REASON, FIELD_CONFIRM, FIELD_RETURN];
    let allowed = |key: &str| {
        portal.contains(&key)
            || fields.iter().any(|f| {
                f.key == key || (matches!(f.kind, FieldKind::Money(_)) && money_currency_key(&f.key) == key)
            })
    };
    let repeats = |key: &str| fields.iter().any(|f| f.key == key && matches!(f.kind, FieldKind::Multi(_)));
    check_keys(body, allowed, repeats)
}

fn check_keys(
    body: &[(String, String)],
    allowed: impl Fn(&str) -> bool,
    repeats: impl Fn(&str) -> bool,
) -> Result<(), String> {
    for (i, (key, _)) in body.iter().enumerate() {
        if !allowed(key) {
            return Err(format!("unexpected field {key:?}"));
        }
        if !repeats(key) && body[..i].iter().any(|(k, _)| k == key) {
            return Err(format!("field {key:?} is posted twice"));
        }
    }
    Ok(())
}

/// The raw value of `field` in a posted body, in the field codec's form; `None` when blank.
/// Multi joins its repeated values into a JSON array; Money pairs the amount with
/// [`money_currency_key`]; an optional String/Text posted as `""` is not blank.
pub(crate) fn posted_raw(field: &Field, body: &[(String, String)]) -> Option<String> {
    let key = field.key.as_str();
    match &field.kind {
        FieldKind::Multi(_) => {
            let picked: Vec<&str> = body.iter().filter(|(k, _)| k == key).map(|(_, v)| v.as_str()).collect();
            (!picked.is_empty()).then(|| serde_json::to_string(&picked).unwrap_or_default())
        }
        FieldKind::Money(_) => {
            let amount = first(body, key).filter(|a| !a.is_empty())?;
            let currency = first(body, &money_currency_key(key)).unwrap_or("");
            Some(serde_json::json!({ "amount": amount, "currency": currency }).to_string())
        }
        FieldKind::String | FieldKind::Text => match first(body, key) {
            Some("") if field.required => None,
            other => other.map(str::to_string),
        },
        _ => first(body, key).filter(|v| !v.is_empty()).map(str::to_string),
    }
}

/// What a form instance submits once its body passed [`check_form_keys`].
pub(crate) struct Submission {
    pub(crate) input: BTreeMap<String, String>,
    pub(crate) expected_revision: Option<u64>,
    pub(crate) reason: Option<String>,
}

/// Decodes the body against the stored spec. A readonly field takes its stored default; a
/// blank optional field is left out; a blank required field, an undecodable value, a
/// missing required reason and a wrong type-to-confirm are field errors.
pub(crate) fn build_submission(
    spec: &StoredSpec,
    body: &[(String, String)],
) -> Result<Submission, BTreeMap<String, String>> {
    let mut errors = BTreeMap::new();
    let mut input = BTreeMap::new();
    let reason = first(body, FIELD_REASON).map(|r| r.trim().to_string()).filter(|r| !r.is_empty());
    let mut expected_revision = None;
    match spec {
        StoredSpec::Form(form) => {
            for field in &form.fields {
                let raw = if field.readonly { field.default.clone() } else { posted_raw(field, body) };
                let Some(raw) = raw else {
                    if field.required && !field.readonly {
                        errors.insert(field.key.clone(), "required".to_string());
                    }
                    continue;
                };
                match decode_field(&field.kind, &raw) {
                    Ok(value) => {
                        input.insert(field.key.clone(), encode_field(&field.kind, &value));
                    }
                    Err(e) => {
                        errors.insert(field.key.clone(), e.reason);
                    }
                }
            }
            input.extend(form.hidden.iter().map(|(k, v)| (k.clone(), v.clone())));
            expected_revision = form.revision;
        }
        StoredSpec::Confirm(confirm) => {
            if confirm.reason == ReasonReq::Required && reason.is_none() {
                errors.insert(FIELD_REASON.to_string(), "a reason is required".to_string());
            }
            if let Some(token) = &confirm.type_to_confirm {
                if first(body, FIELD_CONFIRM) != Some(token.as_str()) {
                    errors.insert(FIELD_CONFIRM.to_string(), format!("type {token} to confirm"));
                }
            }
        }
        StoredSpec::Click => {}
    }
    if errors.is_empty() {
        Ok(Submission { input, expected_revision, reason })
    } else {
        Err(errors)
    }
}

// ---------------------------------------------------------------------------
// Invoke
// ---------------------------------------------------------------------------

pub(crate) enum Invoked {
    Outcome(InvokeOutcome),
    Failed(opsapi::Error),
    TimedOut,
}

pub(crate) async fn invoke(face: &dyn ControlPlane, req: controlplane::InvokeCall) -> Invoked {
    match tokio::time::timeout(INVOKE_DEADLINE, face.invoke(req)).await {
        Ok(Ok(outcome)) => Invoked::Outcome(outcome),
        Ok(Err(error)) => Invoked::Failed(error),
        Err(_) => Invoked::TimedOut,
    }
}

/// Whether an invoke result settles its row consumed (`true`) or releases its claim.
pub(crate) fn consumes(invoked: &Invoked) -> bool {
    match invoked {
        Invoked::Outcome(InvokeOutcome::Ok { .. } | InvokeOutcome::NoOp { .. } | InvokeOutcome::Conflict { .. }) => {
            true
        }
        Invoked::Failed(e) => e.status == opsapi::Status::Conflict,
        Invoked::Outcome(InvokeOutcome::FieldErrors(_)) | Invoked::TimedOut => false,
    }
}

/// The HTTP status of an invoke result, per the portal's outcome table.
pub(crate) fn http_status(invoked: &Invoked) -> StatusCode {
    match invoked {
        Invoked::Outcome(InvokeOutcome::Ok { .. } | InvokeOutcome::NoOp { .. }) => StatusCode::SEE_OTHER,
        Invoked::Outcome(InvokeOutcome::FieldErrors(_)) => StatusCode::UNPROCESSABLE_ENTITY,
        Invoked::Outcome(InvokeOutcome::Conflict { .. }) => StatusCode::CONFLICT,
        Invoked::Failed(e) if e.status == opsapi::Status::Conflict => StatusCode::CONFLICT,
        Invoked::Failed(e) if e.status == opsapi::Status::NotFound => StatusCode::NOT_FOUND,
        Invoked::Failed(e) => {
            StatusCode::from_u16(e.status.http()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
        }
        Invoked::TimedOut => StatusCode::GATEWAY_TIMEOUT,
    }
}

/// Upper bound, in bytes, of an `admin.action` `detail`.
pub(crate) const DETAIL_MAX: usize = 1024;

/// Appends the `admin.action` record of one invoke attempt, whatever its outcome.
pub(crate) async fn record(
    st: &AdminState,
    operator: &Operator,
    req: &controlplane::InvokeCall,
    invoked: &Invoked,
) -> anyhow::Result<()> {
    let evt = controlplane::events::AdminAction {
        actor: operator.login.clone(),
        action: format!("{}.{}", req.module, req.action),
        target: req.entity.as_ref().map(|e| format!("{}:{}", e.kind, e.id)).unwrap_or_default(),
        detail: audit_detail(invoked, req.reason.as_deref()),
    };
    st.emit_action(&evt).await
}

/// The `admin.action` `detail` of one invoke attempt: the outcome, then ` — reason: <reason>`
/// when the operator gave one, cut to [`DETAIL_MAX`] bytes on a char boundary (`…` marks a cut).
pub(crate) fn audit_detail(invoked: &Invoked, reason: Option<&str>) -> String {
    let mut detail = match invoked {
        Invoked::Outcome(InvokeOutcome::Ok { .. }) => "ok".to_string(),
        Invoked::Outcome(InvokeOutcome::NoOp { message }) => format!("noop: {message}"),
        Invoked::TimedOut => format!("unknown: timeout after {}s", INVOKE_DEADLINE.as_secs()),
        Invoked::Outcome(InvokeOutcome::FieldErrors(errors)) => {
            let keys: Vec<&str> = errors.keys().map(String::as_str).collect();
            format!("rejected: field errors {}", keys.join(","))
        }
        Invoked::Outcome(InvokeOutcome::Conflict { current_revision }) => {
            format!("conflict: current revision {current_revision}")
        }
        Invoked::Failed(error) if error.status == opsapi::Status::Conflict => format!("conflict: {}", error.msg),
        Invoked::Failed(error) if error.msg.is_empty() => format!("failed: {}", status_name(error.status)),
        Invoked::Failed(error) => format!("failed: {} {}", status_name(error.status), error.msg),
    };
    if let Some(reason) = reason {
        detail.push_str(" — reason: ");
        detail.push_str(reason);
    }
    if detail.len() > DETAIL_MAX {
        const MARK: &str = "…";
        let mut cut = DETAIL_MAX - MARK.len();
        while !detail.is_char_boundary(cut) {
            cut -= 1;
        }
        detail.truncate(cut);
        detail.push_str(MARK);
    }
    detail
}

fn status_name(status: opsapi::Status) -> &'static str {
    match status {
        opsapi::Status::Ok => "ok",
        opsapi::Status::NotFound => "not_found",
        opsapi::Status::Forbidden => "forbidden",
        opsapi::Status::Invalid => "invalid",
        opsapi::Status::Unavailable => "unavailable",
        opsapi::Status::Internal => "internal",
        opsapi::Status::Unauthorized => "unauthorized",
        opsapi::Status::Conflict => "conflict",
    }
}

// ---------------------------------------------------------------------------
// Flash
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum FlashTone {
    Success,
    Info,
}

impl FlashTone {
    /// The toast tone the templates render.
    pub(crate) fn name(self) -> &'static str {
        match self {
            FlashTone::Success => "ok",
            FlashTone::Info => "info",
        }
    }
}

/// The PRG flash: an `Ok` outcome's message and show-once reveal, or a `NoOp`'s message.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Flash {
    pub(crate) message: String,
    pub(crate) reveal: Option<SecretReveal>,
    pub(crate) tone: FlashTone,
}

/// Stores `flash` under a fresh one-shot token in `admin.reveals`.
pub(crate) async fn stash_flash(pool: &PgPool, flash: &Flash) -> anyhow::Result<String> {
    crate::prune_expired_reveals(pool).await;
    let token = crate::new_token();
    let payload = serde_json::to_string(flash)?;
    sqlx::query("INSERT INTO admin.reveals (token, reveal) VALUES ($1, $2)")
        .bind(&token)
        .bind(&payload)
        .execute(pool)
        .await?;
    Ok(token)
}

/// Consumes the flash for `token` exactly once (`DELETE ... RETURNING`). A store or
/// decode error shows nothing: a show-once secret is never served twice.
pub(crate) async fn take_flash(pool: &PgPool, token: &str) -> Option<Flash> {
    let redeemed = sqlx::query_scalar::<_, String>(&format!(
        "DELETE FROM admin.reveals \
         WHERE token = $1 AND created_at > now() - interval '{} seconds' \
         RETURNING reveal",
        crate::REVEAL_TTL.as_secs()
    ))
    .bind(token)
    .fetch_optional(pool)
    .await;
    match redeemed {
        Ok(Some(json)) => match serde_json::from_str(&json) {
            Ok(flash) => Some(flash),
            Err(err) => {
                tracing::error!(%err, "admin flash: stored payload decode failed");
                None
            }
        },
        Ok(None) => None,
        Err(err) => {
            tracing::error!(%err, "admin flash: redemption query failed");
            None
        }
    }
}

/// A posted `_return` is honoured only as a same-portal path of visible ASCII, never an
/// open redirect, so it is always a valid `Location` value.
pub(crate) fn safe_return(raw: &str) -> Option<&str> {
    let portal = raw == "/admin" || raw.starts_with("/admin/") || raw.starts_with("/admin?");
    let clean = raw.bytes().all(|b| b.is_ascii_graphic()) && !raw.contains("//") && !raw.contains('\\');
    (portal && clean).then_some(raw)
}

/// Where a settled submit redirects: the outcome's own link, else the posted `_return`,
/// else the entity page, else the dashboard.
pub(crate) fn ok_target(redirect: Option<&Link>, body: &[(String, String)], entity: Option<&EntityRef>) -> String {
    if let Some(link) = redirect {
        return link_url(link);
    }
    if let Some(ret) = first(body, FIELD_RETURN).and_then(safe_return) {
        return ret.to_string();
    }
    entity.map(entity_url).unwrap_or_else(|| "/admin".to_string())
}

pub(crate) fn with_flash(target: &str, token: &str) -> String {
    let sep = if target.contains('?') { '&' } else { '?' };
    format!("{target}{sep}reveal={}", encode(token))
}
