//! The "API Keys" admin page — the runtime CRUD surface over the key store, mirroring
//! `config`'s admin item shape (KPIs + a read-only table + a flat text [`adminapi::Form`]).
//! The admin portal owns the route/auth/rendering; this module only supplies the
//! declarative widgets and the [`apply_edit`] submit closure.
//!
//! Two rendering paths share the table/KPI builders:
//!   - LOCAL (the monolith / apikeys-svc itself): [`admin_render`] returns an editable
//!     [`adminapi::Content`] whose form submit diffs the posted policies and applies
//!     `set_policy`/`insert`/`revoke`.
//!   - REMOTE (admin-svc fanning out over the edge): the [`adminapi::AdminData`] impl
//!     returns a READ-ONLY [`adminapi::ItemData`] (`form: None` — a submit closure can't
//!     marshal), fetched via `admin.adminData`.
//!
//! The `RenderFn` is synchronous but the store reads are async, so [`admin_render`]
//! bridges via `block_in_place` (like accounts/characters — requires the multi-thread rt).

use std::sync::Arc;

use crate::store::KeyRow;
use crate::Service;

/// The admin item id — also the `/admin/<id>` page slug the portal routes to.
pub(crate) const ADMIN_ITEM_ID: &str = "apikeys";
/// Sidebar group.
pub(crate) const ADMIN_SECTION: &str = "Platform";
/// Menu entry + page title.
pub(crate) const ADMIN_LABEL: &str = "API Keys";

/// Loose policy validation (Decision 4 of the plan): non-empty, and either the literal
/// `full` or a comma-separated list whose every entry is non-blank. Deliberately NOT a
/// strict method-name check — ops evolve, and an operator may pre-authorize a method that
/// no process serves yet.
fn valid_policy(policy: &str) -> bool {
    !policy.trim().is_empty() && policy.split(',').all(|m| !m.trim().is_empty())
}

/// Rejects an invalid policy with a descriptive error (surfaced first-error-wins).
fn check_policy(policy: &str) -> anyhow::Result<()> {
    if valid_policy(policy) {
        Ok(())
    } else {
        anyhow::bail!("apikeys: invalid policy {policy:?} (must be `full` or a comma-separated method list)")
    }
}

/// Rejects a key name that would collide with the admin form's `_new_*`/`_revoke_name`
/// control fields — a key literally named e.g. `_new_name` overwrites those widgets'
/// own posted values on the next render/submit round-trip. Per-key policy fields use
/// the key's own `name` as the field name, so any `_`-prefixed name is unsafe.
fn check_name(name: &str) -> anyhow::Result<()> {
    if name.starts_with('_') {
        anyhow::bail!(
            "apikeys: key name {name:?} must not start with '_' (reserved for admin form control fields)"
        )
    } else {
        Ok(())
    }
}

/// KPI row: total keys and the active (non-revoked) subset.
fn build_kpis(rows: &[KeyRow]) -> Vec<adminapi::Kpi> {
    let active = rows.iter().filter(|r| !r.revoked).count();
    vec![
        adminapi::Kpi {
            label: "Keys".into(),
            value: rows.len().to_string(),
            sub: String::new(),
        },
        adminapi::Kpi {
            label: "Active".into(),
            value: active.to_string(),
            sub: String::new(),
        },
    ]
}

/// The read-only table: Name / Key (mono) / Policy / Created / Status (badge).
fn build_table(rows: &[KeyRow]) -> adminapi::Table {
    let mut table = adminapi::Table {
        columns: vec![
            "Name".into(),
            "Key".into(),
            "Policy".into(),
            "Created".into(),
            "Status".into(),
        ],
        rows: Vec::with_capacity(rows.len()),
    };
    for r in rows {
        let (text, badge) = if r.revoked {
            ("revoked", "red")
        } else {
            ("active", "green")
        };
        table.rows.push(vec![
            adminapi::Cell::text(&r.name),
            adminapi::Cell::mono(&r.key),
            adminapi::Cell::text(&r.policy),
            adminapi::Cell::text(&r.created_at),
            adminapi::Cell {
                text: text.into(),
                badge: badge.into(),
                ..adminapi::Cell::default()
            },
        ]);
    }
    table
}

/// The FULL editable content: KPIs, table, and a form with one policy [`adminapi::Field`]
/// per key plus the `_new_*` add-row triple and a `_revoke_name` field. Async because it
/// reads the store; the local `RenderFn` bridges to it via [`admin_render`].
pub(crate) async fn admin_content_full(svc: &Arc<Service>) -> anyhow::Result<adminapi::Content> {
    let rows = svc.store.list().await?;

    let mut fields: Vec<adminapi::Field> = Vec::with_capacity(rows.len() + 4);
    for r in &rows {
        // One flat text field per key; editing its value re-writes the policy. Revoked
        // keys are shown too (an operator can still see/adjust their policy string).
        fields.push(adminapi::Field {
            name: r.name.clone(),
            label: r.name.clone(),
            value: r.policy.clone(),
        });
    }
    // Add-row triple: apikeys owns the "all three non-empty -> insert" semantics; the
    // adminapi::Form contract stays a generic name/value list (no richer widget).
    fields.push(adminapi::Field {
        name: "_new_name".into(),
        label: "New key name".into(),
        value: String::new(),
    });
    fields.push(adminapi::Field {
        name: "_new_key".into(),
        label: "New key secret".into(),
        value: String::new(),
    });
    fields.push(adminapi::Field {
        name: "_new_policy".into(),
        label: "New key policy".into(),
        value: String::new(),
    });
    // Revoke: type an existing key name to revoke it.
    fields.push(adminapi::Field {
        name: "_revoke_name".into(),
        label: "Revoke key (name)".into(),
        value: String::new(),
    });

    let submit_svc = svc.clone();
    let form = adminapi::Form {
        action: String::new(),
        fields,
        submit: Some(Arc::new(move |values: adminapi::Params| {
            let svc = submit_svc.clone();
            Box::pin(async move { apply_edit(&svc, values).await })
        })),
    };

    Ok(adminapi::Content {
        kpis: build_kpis(&rows),
        table: Some(build_table(&rows)),
        form: Some(form),
    })
}

/// The read-only content (KPIs + table, no editable form) — what the REMOTE admin fan-out
/// returns, since a remote form cannot marshal its `submit` closure.
async fn admin_content_ro(svc: &Service) -> anyhow::Result<adminapi::Content> {
    let rows = svc.store.list().await?;
    Ok(adminapi::Content {
        kpis: build_kpis(&rows),
        table: Some(build_table(&rows)),
        form: None,
    })
}

/// The synchronous LOCAL render: bridges to the async [`admin_content_full`] via
/// `block_in_place` (the store read is async; the `RenderFn` contract is sync).
pub(crate) fn admin_render(
    svc: &Arc<Service>,
    _params: &adminapi::Params,
) -> anyhow::Result<adminapi::Content> {
    let svc = svc.clone();
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(admin_content_full(&svc))
    })
}

/// One write the posted form plans against a single snapshot — executed together in
/// phase 2's transaction so the whole form lands atomically.
enum PlannedWrite {
    SetPolicy { name: String, policy: String },
    Insert { name: String, key: String, policy: String },
    Revoke { name: String },
}

/// Applies a posted edit in two phases over ONE snapshot (anti-TOCTOU, all-or-nothing):
///   - **Phase 1** reads the store once and validates the WHOLE form — every changed
///     policy plus the `_new_*` triple — building the planned-writes list. The first
///     validation error returns before any write, so an invalid field can never leave an
///     earlier valid one committed.
///   - **Phase 2** opens one transaction, executes exactly the planned writes, commits.
///     A store error rolls the whole batch back, leaving the store untouched.
///
/// The plan is: (1) `set_policy` for each posted policy differing from the snapshot,
/// (2) `insert` the add-row triple when `_new_name`/`_new_key`/`_new_policy` are all set,
/// (3) `revoke` the key named in `_revoke_name` when non-empty. Policies are validated
/// loosely ([`valid_policy`]) in phase 1.
pub(crate) async fn apply_edit(svc: &Service, values: adminapi::Params) -> anyhow::Result<()> {
    // Phase 1 — validate the whole form against one snapshot, building the plan.
    let rows = svc.store.list().await?;
    let mut planned: Vec<PlannedWrite> = Vec::new();

    // (1) policy edits — only keys whose posted value differs from the snapshot.
    for r in &rows {
        if let Some(v) = values.get(&r.name) {
            if *v != r.policy {
                check_policy(v)?;
                planned.push(PlannedWrite::SetPolicy {
                    name: r.name.clone(),
                    policy: v.clone(),
                });
            }
        }
    }

    // (2) add-row: insert only when the whole triple is filled.
    let new_name = adminapi::param(&values, "_new_name");
    let new_key = adminapi::param(&values, "_new_key");
    let new_policy = adminapi::param(&values, "_new_policy");
    if !new_name.is_empty() && !new_key.is_empty() && !new_policy.is_empty() {
        check_name(new_name)?;
        check_policy(new_policy)?;
        planned.push(PlannedWrite::Insert {
            name: new_name.to_string(),
            key: new_key.to_string(),
            policy: new_policy.to_string(),
        });
    }

    // (3) revoke by name (a missing name is a no-op at the store — no validation surface).
    let revoke_name = adminapi::param(&values, "_revoke_name");
    if !revoke_name.is_empty() {
        planned.push(PlannedWrite::Revoke {
            name: revoke_name.to_string(),
        });
    }

    // Phase 2 — one transaction: execute exactly the planned writes and commit.
    let mut tx = svc.store.pool.begin().await?;
    for write in planned {
        match write {
            PlannedWrite::SetPolicy { name, policy } => {
                svc.store.set_policy_tx(&mut tx, &name, &policy).await?;
            }
            PlannedWrite::Insert { name, key, policy } => {
                svc.store.insert_tx(&mut tx, &name, &key, &policy).await?;
            }
            PlannedWrite::Revoke { name } => {
                svc.store.revoke_tx(&mut tx, &name).await?;
            }
        }
    }
    tx.commit().await?;
    Ok(())
}

#[async_trait::async_trait]
impl adminapi::AdminData for Service {
    /// The admin fan-out (`admin.adminData` on the edge): the API Keys page as
    /// [`adminapi::ItemData`]. Read-only over the wire (the editable form is LOCAL-only),
    /// carrying the same section/label the local `Item` does.
    async fn admin_data(&self) -> Result<adminapi::ItemData, opsapi::Error> {
        let content = admin_content_ro(self)
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
