//! The "API Keys" admin page — the runtime configurator over the normalized role+key
//! store. The admin portal owns the route/auth/rendering; this module supplies the
//! declarative typed widgets and the submit dispatch.
//!
//! ONE [`adminapi::Content`] carries ONE [`adminapi::Form`], so the several distinct
//! mutations (create/edit/delete a role; create/re-role/revoke a key) are discriminated
//! by an explicit `_action` Select. The submit dispatch reads `_action`, validates the
//! fields that action requires, and REJECTS partial/ambiguous input explicitly — it never
//! silently no-ops a half-filled row (a real bug in the former flat-form design). CAS
//! evidence rides as `_expected_<role|key>_rev_<name>` hidden fields (one per row); the
//! dispatch pulls the matching one for the chosen target.
//!
//! Two rendering paths share [`build_form_data`]:
//!   - LOCAL ([`admin_content_local`] / [`admin_render`]): attaches the in-process submit
//!     closure (backed by the store); the `RenderFn` is sync, so it bridges via
//!     `block_in_place` (requires the multi-thread rt).
//!   - REMOTE ([`adminapi::AdminData::admin_data`], `form.submit == None`): the SAME typed
//!     structure + current values, but the mutation is driven by the admin process over
//!     the edge via [`adminapi::AdminSubmit::admin_submit`] — which runs [`apply_submit`]
//!     server-side here, so the closure never marshals across the wire.
//!
//! Secret secrecy authority: `generate_secret` (in `store`) and the show-once
//! [`adminapi::RevealItem`] on `create_key` are the ONLY places plaintext exists; no read
//! path (table, `list_keys`, KPIs) ever holds it.

use std::sync::Arc;

use crate::store::{KeySummary, RoleSummary, WriteError};
use crate::Service;

/// The admin item id — also the `/admin/<id>` page slug the portal routes to.
pub(crate) const ADMIN_ITEM_ID: &str = "apikeys";
/// Sidebar group.
pub(crate) const ADMIN_SECTION: &str = "Platform";
/// Menu entry + page title.
pub(crate) const ADMIN_LABEL: &str = "API Keys";

// ---- Form field names + action discriminants -------------------------------

const ACTION_FIELD: &str = "_action";
const ROLE_NAME_FIELD: &str = "role_name";
const ROLE_POLICY_FIELD: &str = "role_policy";
const ROLE_TARGET_FIELD: &str = "role_target";
const KEY_NAME_FIELD: &str = "key_name";
const KEY_ROLE_FIELD: &str = "key_role";
const KEY_TARGET_FIELD: &str = "key_target";

/// Hidden CAS-evidence field name prefixes (one field per row; the `_expected_` stem is
/// what the admin portal's allowlist retains even when a row vanishes before re-render).
const ROLE_REV_PREFIX: &str = "_expected_role_rev_";
const KEY_REV_PREFIX: &str = "_expected_key_rev_";

const ACTION_CREATE_ROLE: &str = "create_role";
const ACTION_SET_ROLE_POLICY: &str = "set_role_policy";
const ACTION_DELETE_ROLE: &str = "delete_role";
const ACTION_CREATE_KEY: &str = "create_key";
const ACTION_SET_KEY_ROLE: &str = "set_key_role";
const ACTION_REVOKE_KEY: &str = "revoke_key";

// ============================================================================
// Widget builders
// ============================================================================

fn text_field(name: &str, label: &str) -> adminapi::Field {
    adminapi::Field {
        name: name.into(),
        label: label.into(),
        value: String::new(),
        kind: adminapi::FieldKind::Text,
        options: Vec::new(),
    }
}

/// A single-choice dropdown with a leading blank option (so "nothing chosen" is
/// representable and explicitly rejected by [`required`], never a silent default).
fn select_field(name: &str, label: &str, mut options: Vec<adminapi::FieldOption>) -> adminapi::Field {
    options.insert(
        0,
        adminapi::FieldOption { value: String::new(), label: "— choose —".into(), checked: true },
    );
    adminapi::Field {
        name: name.into(),
        label: label.into(),
        value: String::new(),
        kind: adminapi::FieldKind::Select,
        options,
    }
}

fn action_options() -> Vec<adminapi::FieldOption> {
    let opt = |value: &str, label: &str| adminapi::FieldOption {
        value: value.into(),
        label: label.into(),
        checked: false,
    };
    vec![
        adminapi::FieldOption { value: String::new(), label: "— choose an action —".into(), checked: true },
        opt(ACTION_CREATE_ROLE, "Create role"),
        opt(ACTION_SET_ROLE_POLICY, "Edit role policy"),
        opt(ACTION_DELETE_ROLE, "Delete role"),
        opt(ACTION_CREATE_KEY, "Create key (reveals a one-time secret)"),
        opt(ACTION_SET_KEY_ROLE, "Set key role"),
        opt(ACTION_REVOKE_KEY, "Revoke key"),
    ]
}

fn role_name_options(roles: &[RoleSummary]) -> Vec<adminapi::FieldOption> {
    roles
        .iter()
        .map(|r| adminapi::FieldOption { value: r.name.clone(), label: r.name.clone(), checked: false })
        .collect()
}

fn key_name_options(keys: &[KeySummary]) -> Vec<adminapi::FieldOption> {
    keys
        .iter()
        .map(|k| adminapi::FieldOption { value: k.name.clone(), label: k.name.clone(), checked: false })
        .collect()
}

/// KPI row: total keys, the active (non-revoked) subset, and the role count.
fn build_kpis(roles: &[RoleSummary], keys: &[KeySummary]) -> Vec<adminapi::Kpi> {
    let active = keys.iter().filter(|k| !k.revoked).count();
    vec![
        adminapi::Kpi { label: "Keys".into(), value: keys.len().to_string(), sub: String::new() },
        adminapi::Kpi { label: "Active".into(), value: active.to_string(), sub: String::new() },
        adminapi::Kpi { label: "Roles".into(), value: roles.len().to_string(), sub: String::new() },
    ]
}

/// The read-only keys table: Name / Prefix (mono, NOT the secret) / Role / Status.
fn build_keys_table(keys: &[KeySummary]) -> adminapi::Table {
    let mut table = adminapi::Table {
        columns: vec!["Name".into(), "Prefix".into(), "Role".into(), "Created".into(), "Status".into()],
        rows: Vec::with_capacity(keys.len()),
    };
    for k in keys {
        let (text, badge) = if k.revoked { ("revoked", "red") } else { ("active", "green") };
        table.rows.push(vec![
            adminapi::Cell::text(&k.name),
            adminapi::Cell::mono(&k.prefix),
            adminapi::Cell::text(&k.role),
            adminapi::Cell::text(&k.created_at),
            adminapi::Cell { text: text.into(), badge: badge.into(), ..adminapi::Cell::default() },
        ]);
    }
    table
}

/// Builds the typed form (submit left `None` — the caller attaches it for the LOCAL path).
/// One `_action` Select drives dispatch; the create/edit/target fields coexist and are
/// read per-action. `_expected_*_rev_*` hidden fields carry per-row CAS evidence.
fn build_form(roles: &[RoleSummary], keys: &[KeySummary]) -> adminapi::Form {
    // The action selector doubles as the operator hint surface (finding #13 recovery +
    // the free-text-policy note) since the generic form has no free-text region.
    //
    // TODO(Step 7): swap the ROLE_POLICY_FIELD free-text field for a
    // FieldKind::CheckboxGroup whose options come from the generated ops-catalog artefact
    // (`opscatalog::OPERATIONS`); loose validation stays so an operator may still
    // pre-authorize an unserved method.
    let fields = vec![
        adminapi::Field {
            name: ACTION_FIELD.into(),
            label: "Action — a create secret is shown ONCE; if you lose it, revoke the key and \
                    recreate it under a new name (it cannot be recovered)"
                .into(),
            value: String::new(),
            kind: adminapi::FieldKind::Select,
            options: action_options(),
        },
        text_field(ROLE_NAME_FIELD, "New role name (Create role)"),
        text_field(
            ROLE_POLICY_FIELD,
            "Role policy — `full` or comma-separated methods (Create role / Edit role policy)",
        ),
        select_field(ROLE_TARGET_FIELD, "Target role (Edit role policy / Delete role)", role_name_options(roles)),
        text_field(KEY_NAME_FIELD, "New key name (Create key)"),
        select_field(KEY_ROLE_FIELD, "Key role (Create key / Set key role)", role_name_options(roles)),
        select_field(KEY_TARGET_FIELD, "Target key (Set key role / Revoke key)", key_name_options(keys)),
    ];

    let mut hidden = Vec::with_capacity(roles.len() + keys.len());
    for r in roles {
        hidden.push(adminapi::HiddenField { name: format!("{ROLE_REV_PREFIX}{}", r.name), value: r.revision.to_string() });
    }
    for k in keys {
        hidden.push(adminapi::HiddenField { name: format!("{KEY_REV_PREFIX}{}", k.name), value: k.revision.to_string() });
    }

    adminapi::Form { action: String::new(), fields, hidden, submit: None }
}

// ============================================================================
// Content assembly (shared data path + the two render paths)
// ============================================================================

/// The typed content WITHOUT a submit closure — the shared shape both paths render.
/// Reads the store (roles + keys) once.
async fn build_form_data(svc: &Service) -> anyhow::Result<adminapi::Content> {
    let roles = svc.list_roles().await?;
    let keys = svc.list_keys().await?;
    Ok(adminapi::Content {
        kpis: build_kpis(&roles, &keys),
        table: Some(build_keys_table(&keys)),
        form: Some(build_form(&roles, &keys)),
    })
}

/// The LOCAL editable content: [`build_form_data`] plus the in-process submit closure
/// (captures an `Arc<Service>` so the `'static` [`adminapi::SubmitFn`] can call
/// [`apply_submit`]).
pub(crate) async fn admin_content_local(svc: &Arc<Service>) -> anyhow::Result<adminapi::Content> {
    let mut content = build_form_data(svc).await?;
    if let Some(form) = content.form.as_mut() {
        let closure_svc = svc.clone();
        form.submit = Some(Arc::new(move |values: adminapi::Params| {
            let svc = closure_svc.clone();
            Box::pin(async move { apply_submit(&svc, values).await })
        }));
    }
    Ok(content)
}

/// The synchronous LOCAL render: bridges to the async [`admin_content_local`] via
/// `block_in_place` (the store reads are async; the `RenderFn` contract is sync).
pub(crate) fn admin_render(
    svc: &Arc<Service>,
    _params: &adminapi::Params,
) -> anyhow::Result<adminapi::Content> {
    let svc = svc.clone();
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(admin_content_local(&svc))
    })
}

// ============================================================================
// Submit dispatch — the single write authority (LOCAL closure + REMOTE admin_submit)
// ============================================================================

/// Reads a required non-empty field, else an explicit rejection (NOT a silent no-op).
fn required<'a>(
    values: &'a adminapi::Params,
    field: &str,
    action: &str,
) -> Result<&'a str, adminapi::SubmitError> {
    let v = adminapi::param(values, field).trim();
    if v.is_empty() {
        return Err(adminapi::SubmitError::Other(anyhow::anyhow!(
            "apikeys: action {action:?} requires a non-empty {field:?}"
        )));
    }
    Ok(v)
}

/// Reads the CAS `revision` evidence for `target` under `prefix`. A missing/unparseable
/// value means the target vanished (or the form is stale) since GET — a
/// [`adminapi::SubmitError::Conflict`], never a silent write.
fn expected_rev(
    values: &adminapi::Params,
    prefix: &str,
    target: &str,
) -> Result<i64, adminapi::SubmitError> {
    values
        .get(&format!("{prefix}{target}"))
        .and_then(|v| v.trim().parse::<i64>().ok())
        .ok_or(adminapi::SubmitError::Conflict)
}

/// Maps a store [`WriteError`] onto the admin submit error space. A domain conflict (CAS
/// miss, duplicate, FK) is [`adminapi::SubmitError::Conflict`]; validation/store trouble
/// is `Other` — NEVER anything that could read as a not-found (finding #2 authority).
fn to_submit_error(e: WriteError) -> adminapi::SubmitError {
    match e {
        WriteError::Conflict(_) => adminapi::SubmitError::Conflict,
        WriteError::Invalid(msg) => adminapi::SubmitError::Other(anyhow::anyhow!(msg)),
        WriteError::Db(err) => adminapi::SubmitError::Other(anyhow::anyhow!(err)),
    }
}

/// Dispatches one posted form on its explicit `_action`. Only the fields that action needs
/// are read; a blank required field or an unknown/empty action is rejected explicitly.
/// `create_key` returns the freshly minted secret as a SHOW-ONCE reveal (the only place a
/// caller ever sees the plaintext).
pub(crate) async fn apply_submit(
    svc: &Service,
    values: adminapi::Params,
) -> Result<adminapi::SubmitOutcome, adminapi::SubmitError> {
    let action = adminapi::param(&values, ACTION_FIELD).trim().to_string();
    match action.as_str() {
        ACTION_CREATE_ROLE => {
            let name = required(&values, ROLE_NAME_FIELD, ACTION_CREATE_ROLE)?;
            let policy = required(&values, ROLE_POLICY_FIELD, ACTION_CREATE_ROLE)?;
            svc.create_role(name, policy).await.map_err(to_submit_error)?;
            Ok(adminapi::SubmitOutcome::default())
        }
        ACTION_SET_ROLE_POLICY => {
            let role = required(&values, ROLE_TARGET_FIELD, ACTION_SET_ROLE_POLICY)?;
            let policy = required(&values, ROLE_POLICY_FIELD, ACTION_SET_ROLE_POLICY)?;
            let rev = expected_rev(&values, ROLE_REV_PREFIX, role)?;
            svc.set_role_policy(role, rev, policy).await.map_err(to_submit_error)?;
            Ok(adminapi::SubmitOutcome::default())
        }
        ACTION_DELETE_ROLE => {
            let role = required(&values, ROLE_TARGET_FIELD, ACTION_DELETE_ROLE)?;
            let rev = expected_rev(&values, ROLE_REV_PREFIX, role)?;
            svc.delete_role(role, rev).await.map_err(to_submit_error)?;
            Ok(adminapi::SubmitOutcome::default())
        }
        ACTION_CREATE_KEY => {
            let name = required(&values, KEY_NAME_FIELD, ACTION_CREATE_KEY)?;
            let role = required(&values, KEY_ROLE_FIELD, ACTION_CREATE_KEY)?;
            let (secret, _prefix) = svc.create_key(name, role).await.map_err(to_submit_error)?;
            Ok(adminapi::SubmitOutcome {
                reveal: vec![adminapi::RevealItem { label: "secret".into(), value: secret }],
            })
        }
        ACTION_SET_KEY_ROLE => {
            let key = required(&values, KEY_TARGET_FIELD, ACTION_SET_KEY_ROLE)?;
            let role = required(&values, KEY_ROLE_FIELD, ACTION_SET_KEY_ROLE)?;
            let rev = expected_rev(&values, KEY_REV_PREFIX, key)?;
            svc.set_key_role(key, rev, role).await.map_err(to_submit_error)?;
            Ok(adminapi::SubmitOutcome::default())
        }
        ACTION_REVOKE_KEY => {
            let key = required(&values, KEY_TARGET_FIELD, ACTION_REVOKE_KEY)?;
            let rev = expected_rev(&values, KEY_REV_PREFIX, key)?;
            svc.revoke_key(key, rev).await.map_err(to_submit_error)?;
            Ok(adminapi::SubmitOutcome::default())
        }
        "" => Err(adminapi::SubmitError::Other(anyhow::anyhow!(
            "apikeys: no action selected — pick an action from the dropdown"
        ))),
        other => Err(adminapi::SubmitError::Other(anyhow::anyhow!(
            "apikeys: unknown action {other:?}"
        ))),
    }
}

/// Maps the admin submit error onto an [`opsapi::Error`] for the REMOTE
/// [`adminapi::AdminSubmit`] path. A conflict → [`opsapi::Status::Conflict`] (409);
/// everything else → [`opsapi::Status::Internal`] (error card). NEVER
/// [`opsapi::Status::NotFound`]: the edge makes NotFound indistinguishable from
/// UnknownMethod, so the admin would degrade the item to read-only (405) and mask a real
/// domain error or a wiring typo (finding #2).
fn submit_error_to_ops(e: adminapi::SubmitError) -> opsapi::Error {
    match e {
        adminapi::SubmitError::Conflict => opsapi::Error::conflict(
            "apikeys: the submitted form is stale or conflicts with existing state",
        ),
        adminapi::SubmitError::Other(err) => opsapi::Error::internal(err.to_string()),
    }
}

#[async_trait::async_trait]
impl adminapi::AdminData for Service {
    /// The admin fan-out READ (`admin.adminData` on the edge): the API Keys page as
    /// [`adminapi::ItemData`], typed fields + current values, `form.submit == None` (the
    /// write is driven remotely via `admin.adminSubmit`).
    async fn admin_data(&self) -> Result<adminapi::ItemData, opsapi::Error> {
        let content = build_form_data(self)
            .await
            .map_err(|e| opsapi::Error::internal(e.to_string()))?;
        Ok(adminapi::ItemData {
            id: ADMIN_ITEM_ID.into(),
            section: ADMIN_SECTION.into(),
            label: ADMIN_LABEL.into(),
            content,
        })
    }
}

#[async_trait::async_trait]
impl adminapi::AdminSubmit for Service {
    /// The opt-in remote WRITE (`admin.adminSubmit` on the edge): runs the SAME
    /// [`apply_submit`] dispatch server-side (where the store is local), so a remote admin
    /// process edits roles/keys over the mTLS edge without marshalling the closure. `id` is
    /// the page slug (ignored — this Service serves exactly the apikeys page).
    async fn admin_submit(
        &self,
        _id: String,
        params: adminapi::Params,
    ) -> Result<adminapi::SubmitOutcome, opsapi::Error> {
        apply_submit(self, params).await.map_err(submit_error_to_ops)
    }
}
