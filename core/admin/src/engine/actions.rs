//! Actions: admission (role, `implemented`, the module's `writes`), the invoke call, the
//! outcome → HTTP mapping, the `admin.action` record and the show-once flash through
//! `admin.reveals`.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use controlplane::{
    ActionDecl, ConfirmSpec, ControlPlane, EntityRef, InvokeOutcome, InvokeRequest, Link, ReasonReq, SecretReveal, Then,
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use super::manifests::Snapshot;
use super::roles::{self, Operator};
use crate::render::{encode, entity_url, link_url};
use crate::AdminState;

/// Bound on one invoke round trip; the edge has no response deadline of its own
/// (`core/edge/src/client.rs:113-154`).
pub(crate) const INVOKE_DEADLINE: Duration = Duration::from_secs(10);

/// Form fields the portal consumes; every other field is the action's `input`.
pub(crate) const FIELD_CSRF: &str = "_csrf";
pub(crate) const FIELD_ENTITY: &str = "_entity";
pub(crate) const FIELD_REASON: &str = "_reason";
pub(crate) const FIELD_REVISION: &str = "_expected_revision";
pub(crate) const FIELD_RETURN: &str = "_return";

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

pub(crate) enum BuildError {
    /// 400: a portal field is malformed.
    Malformed(String),
    /// 422: the declaration's own requirements are unmet.
    Fields(BTreeMap<String, String>),
}

/// Splits the posted form into an [`InvokeRequest`]; a required confirm reason is
/// enforced here, before any call.
pub(crate) fn build_request(
    module: &str,
    decl: &ActionDecl,
    operator: &Operator,
    body: &HashMap<String, String>,
) -> Result<InvokeRequest, BuildError> {
    let entity = match body.get(FIELD_ENTITY).map(String::as_str).filter(|v| !v.is_empty()) {
        None => None,
        Some(raw) => Some(
            parse_entity(raw).ok_or_else(|| BuildError::Malformed(format!("{FIELD_ENTITY} must be kind:id")))?,
        ),
    };
    let expected_revision = match body.get(FIELD_REVISION).map(String::as_str).filter(|v| !v.is_empty()) {
        None => None,
        Some(raw) => Some(
            raw.parse::<u64>()
                .map_err(|_| BuildError::Malformed(format!("{FIELD_REVISION} must be an unsigned integer")))?,
        ),
    };
    let reason = body
        .get(FIELD_REASON)
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty());
    if let Then::Confirm(ConfirmSpec { reason: ReasonReq::Required, .. }) = &decl.then {
        if reason.is_none() {
            let mut errors = BTreeMap::new();
            errors.insert(FIELD_REASON.to_string(), "a reason is required".to_string());
            return Err(BuildError::Fields(errors));
        }
    }
    let input = body
        .iter()
        .filter(|(k, _)| ![FIELD_CSRF, FIELD_ENTITY, FIELD_REASON, FIELD_REVISION, FIELD_RETURN].contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    Ok(InvokeRequest {
        module: module.to_string(),
        action: decl.id.clone(),
        entity,
        input,
        expected_revision,
        reason,
        operator: operator.login.clone(),
    })
}

pub(crate) enum Invoked {
    Outcome(InvokeOutcome),
    Failed(opsapi::Error),
    TimedOut,
}

pub(crate) async fn invoke(face: &dyn ControlPlane, req: InvokeRequest) -> Invoked {
    match tokio::time::timeout(INVOKE_DEADLINE, face.invoke(req)).await {
        Ok(Ok(outcome)) => Invoked::Outcome(outcome),
        Ok(Err(error)) => Invoked::Failed(error),
        Err(_) => Invoked::TimedOut,
    }
}

/// The HTTP status of an invoke result, per the portal's outcome table.
pub(crate) fn http_status(invoked: &Invoked) -> StatusCode {
    match invoked {
        Invoked::Outcome(InvokeOutcome::Ok { .. }) => StatusCode::SEE_OTHER,
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
    req: &InvokeRequest,
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

/// The PRG flash: an `Ok` outcome's message and show-once reveal.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Flash {
    pub(crate) message: String,
    pub(crate) reveal: Option<SecretReveal>,
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

/// Where an `Ok` outcome redirects: its own link, else the posted `_return`, else the
/// entity page, else the dashboard.
pub(crate) fn ok_target(redirect: Option<&Link>, body: &HashMap<String, String>, entity: Option<&EntityRef>) -> String {
    if let Some(link) = redirect {
        return link_url(link);
    }
    if let Some(ret) = body.get(FIELD_RETURN).and_then(|r| safe_return(r)) {
        return ret.to_string();
    }
    entity.map(entity_url).unwrap_or_else(|| "/admin".to_string())
}

pub(crate) fn with_flash(target: &str, token: &str) -> String {
    let sep = if target.contains('?') { '&' } else { '?' };
    format!("{target}{sep}reveal={}", encode(token))
}
