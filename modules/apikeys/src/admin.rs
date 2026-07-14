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
pub(crate) const EXPECTED_STATE_FIELD: &str = "_expected_state";

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
struct ExpectedRow {
    name: String,
    policy: String,
    revoked: bool,
}

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

/// Rejects a key secret longer than the shared [`apikeysapi::MAX_KEY_BYTES`] contract
/// (BYTES, not chars) — the same limit `RealKeyVerifier::lookup`
/// (`modules/gateway/src/keys.rs`) treats an over-length string as definitively unknown.
/// Checked here, in phase 1, so an over-length add-row is rejected before ANY write,
/// matching the store's own `insert_tx` guard (defense-in-depth, not the sole gate).
pub(crate) fn check_key_length(key: &str) -> anyhow::Result<()> {
    if key.len() > apikeysapi::MAX_KEY_BYTES {
        anyhow::bail!(
            "apikeys: key is {} bytes, exceeding apikeysapi::MAX_KEY_BYTES ({} bytes) — it would \
             always be rejected by the gateway's key verifier",
            key.len(),
            apikeysapi::MAX_KEY_BYTES,
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

    let mut expected_rows: Vec<ExpectedRow> = rows
        .iter()
        .map(|row| ExpectedRow {
            name: row.name.clone(),
            policy: row.policy.clone(),
            revoked: row.revoked,
        })
        .collect();
    expected_rows.sort_by(|a, b| a.name.cmp(&b.name));
    let expected_state = serde_json::to_string(&expected_rows)?;

    let mut fields: Vec<adminapi::Field> = Vec::with_capacity(rows.len() + 4);
    for r in &rows {
        // One flat text field per key; editing its value re-writes the policy. Revoked
        // keys are shown too (an operator can still see/adjust their policy string).
        fields.push(adminapi::Field {
            name: r.name.clone(),
            label: r.name.clone(),
            value: r.policy.clone(),
            ..Default::default()
        });
    }
    // Add-row triple: apikeys owns the "all three non-empty -> insert" semantics; the
    // adminapi::Form contract stays a generic name/value list (no richer widget).
    fields.push(adminapi::Field {
        name: "_new_name".into(),
        label: "New key name".into(),
        value: String::new(),
        ..Default::default()
    });
    fields.push(adminapi::Field {
        name: "_new_key".into(),
        label: "New key secret".into(),
        value: String::new(),
        ..Default::default()
    });
    fields.push(adminapi::Field {
        name: "_new_policy".into(),
        label: "New key policy".into(),
        value: String::new(),
        ..Default::default()
    });
    // Revoke: type an existing key name to revoke it.
    fields.push(adminapi::Field {
        name: "_revoke_name".into(),
        label: "Revoke key (name)".into(),
        value: String::new(),
        ..Default::default()
    });

    let submit_svc = svc.clone();
    let form = adminapi::Form {
        action: String::new(),
        fields,
        hidden: vec![adminapi::HiddenField {
            name: EXPECTED_STATE_FIELD.into(),
            value: expected_state,
        }],
        submit: Some(Arc::new(move |values: adminapi::Params| {
            let svc = submit_svc.clone();
            Box::pin(async move {
                apply_edit(&svc, values).await?;
                Ok(adminapi::SubmitOutcome::default())
            })
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

/// One write the posted form plans against the state rendered by its GET. Updates
/// combine policy and revoke changes so editing and revoking one key is one CAS write.
enum PlannedWrite {
    Update {
        expected: ExpectedRow,
        policy: String,
        revoked: bool,
    },
    Insert { name: String, key: String, policy: String },
}

/// Applies a posted whole-form edit using the rendered rows as compare-and-swap
/// evidence. Every expected row is locked and checked before planning or executing a
/// write. An unrelated row inserted after GET is outside that evidence and does not
/// conflict; any changed, revoked, or deleted expected row rejects the entire batch.
pub(crate) async fn apply_edit(
    svc: &Service,
    values: adminapi::Params,
) -> Result<(), adminapi::SubmitError> {
    let expected_json = values
        .get(EXPECTED_STATE_FIELD)
        .ok_or(adminapi::SubmitError::Conflict)?;
    let mut expected_rows: Vec<ExpectedRow> =
        serde_json::from_str(expected_json).map_err(|_| adminapi::SubmitError::Conflict)?;
    expected_rows.sort_by(|a, b| a.name.cmp(&b.name));
    if expected_rows
        .windows(2)
        .any(|rows| rows[0].name == rows[1].name)
    {
        return Err(adminapi::SubmitError::Conflict);
    }

    let mut tx = svc
        .store
        .pool
        .begin()
        .await
        .map_err(anyhow::Error::from)?;
    for expected in &expected_rows {
        let actual = crate::store::Store::lock_admin_state_tx(&mut tx, &expected.name)
            .await
            .map_err(anyhow::Error::from)?;
        match actual {
            Some((policy, revoked))
                if policy == expected.policy && revoked == expected.revoked => {}
            _ => return Err(adminapi::SubmitError::Conflict),
        }
    }

    let mut planned: Vec<PlannedWrite> = Vec::new();

    let revoke_name = adminapi::param(&values, "_revoke_name");
    if !revoke_name.is_empty()
        && !expected_rows
            .iter()
            .any(|expected| expected.name == revoke_name)
    {
        return Err(adminapi::SubmitError::Conflict);
    }

    for expected in &expected_rows {
        let policy = values
            .get(&expected.name)
            .cloned()
            .unwrap_or_else(|| expected.policy.clone());
        let revoked = expected.revoked || expected.name == revoke_name;
        if policy != expected.policy {
            check_policy(&policy).map_err(adminapi::SubmitError::from)?;
        }
        if policy != expected.policy || revoked != expected.revoked {
            planned.push(PlannedWrite::Update {
                expected: expected.clone(),
                policy,
                revoked,
            });
        }
    }

    let new_name = adminapi::param(&values, "_new_name");
    let new_key = adminapi::param(&values, "_new_key");
    let new_policy = adminapi::param(&values, "_new_policy");
    if !new_name.is_empty() && !new_key.is_empty() && !new_policy.is_empty() {
        check_name(new_name).map_err(adminapi::SubmitError::from)?;
        check_key_length(new_key).map_err(adminapi::SubmitError::from)?;
        check_policy(new_policy).map_err(adminapi::SubmitError::from)?;
        planned.push(PlannedWrite::Insert {
            name: new_name.to_string(),
            key: new_key.to_string(),
            policy: new_policy.to_string(),
        });
    }

    for write in planned {
        match write {
            PlannedWrite::Update {
                expected,
                policy,
                revoked,
            } => {
                let affected = crate::store::Store::update_admin_state_tx(
                    &mut tx,
                    &expected.name,
                    &expected.policy,
                    expected.revoked,
                    &policy,
                    revoked,
                )
                .await
                .map_err(anyhow::Error::from)?;
                if affected != 1 {
                    return Err(adminapi::SubmitError::Conflict);
                }
            }
            PlannedWrite::Insert { name, key, policy } => {
                if let Err(error) = svc.store.insert_tx(&mut tx, &name, &key, &policy).await {
                    if is_unique_violation(&error) {
                        return Err(adminapi::SubmitError::Conflict);
                    }
                    return Err(adminapi::SubmitError::Other(error.into()));
                }
            }
        }
    }
    tx.commit().await.map_err(anyhow::Error::from)?;
    Ok(())
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    matches!(
        error,
        sqlx::Error::Database(database) if database.code().as_deref() == Some("23505")
    )
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
