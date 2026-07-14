//! The accounts module's admin surface (port of Go's `modules/accounts/admin.go`):
//! the live "Players" block — a KPI triple + a table of the newest 50 players. It
//! reads only its own data and returns the admin's declarative widget types; the
//! admin portal owns the look and never touches the accounts schema. Mirrors the
//! characters module's admin shape exactly (plain `accountsapi::Admin` capability +
//! a local `adminapi::Item`; the edge admin fan-out is Step 7).

use async_trait::async_trait;
use opsapi::Error;

use crate::store::Store;
use crate::Service;

/// The admin surface ids — shared by the contributed `Item` and the
/// `Admin::admin_data` reply so a (future) remote admin fetches the same
/// Section/Label the local render carries.
pub(crate) const ADMIN_ITEM_ID: &str = "accounts";
pub(crate) const ADMIN_SECTION: &str = "Identity";
pub(crate) const ADMIN_LABEL: &str = "Players";

#[async_trait]
impl adminapi::AdminData for Service {
    /// The admin fan-out: this module's page as `adminapi::ItemData` (same
    /// Section/Label the local `Item` carries), served on the edge as
    /// `admin.adminData` so a remote admin process renders the Players page.
    async fn admin_data(&self, _params: adminapi::Params) -> Result<adminapi::ItemData, Error> {
        let content = admin_content(&self.store)
            .await
            .map_err(|e| Error::internal(e.to_string()))?;
        Ok(adminapi::ItemData {
            id: ADMIN_ITEM_ID.into(),
            section: ADMIN_SECTION.into(),
            label: ADMIN_LABEL.into(),
            content,
            ..Default::default()
        })
    }
}

/// Assembles the Players page: KPIs (players / identities / active sessions) and a
/// table with a per-player provider list + Online/Offline badge (Go's `adminSection`).
pub(crate) async fn admin_content(store: &Store) -> anyhow::Result<adminapi::Content> {
    let (players, identities, sessions) = store.stats().await?;
    let rows = store.list_players(50).await?;

    let mut table = adminapi::Table {
        columns: vec![
            "PLAYER".into(),
            "PLAYER ID".into(),
            "PROVIDERS".into(),
            "STATUS".into(),
            "CREATED".into(),
        ],
        rows: Vec::with_capacity(rows.len()),
        // Bind each row's `⋯` menu to the accounts-owned point so contributors
        // (characters, inventory) can drop "View …" drill-downs onto it.
        menu_point: accountsapi::admin::PLAYERS_ROW_MENU.id.into(),
        row_meta: Vec::with_capacity(rows.len()),
    };
    for p in rows {
        let status = if p.online {
            adminapi::Cell {
                text: "Online".into(),
                badge: "green".into(),
                ..Default::default()
            }
        } else {
            adminapi::Cell {
                text: "Offline".into(),
                badge: "grey".into(),
                ..Default::default()
            }
        };
        table.rows.push(vec![
            adminapi::Cell::text(&p.display_name),
            adminapi::Cell::mono(&p.id),
            adminapi::Cell::text(or_dash(&p.providers.join(", "))),
            status,
            adminapi::Cell::text(&p.created_at),
        ]);
        // Index-aligned per-row metadata: the `{id}` interpolation source plus the
        // owner's own inert native entries (Edit/Delete are visible-but-inert per the
        // mockup — wiring them is a later op-backed phase).
        table.row_meta.push(player_row_meta(&p.id, &p.display_name));
    }

    Ok(adminapi::Content {
        kpis: vec![
            adminapi::Kpi {
                label: "Players".into(),
                value: players.to_string(),
                sub: String::new(),
            },
            adminapi::Kpi {
                label: "Identities".into(),
                value: identities.to_string(),
                sub: "linked credentials".into(),
            },
            adminapi::Kpi {
                label: "Active sessions".into(),
                value: sessions.to_string(),
                sub: String::new(),
            },
        ],
        table: Some(table),
        form: None,
        ..Default::default()
    })
}

fn or_dash(s: &str) -> &str {
    if s.is_empty() {
        "—"
    } else {
        s
    }
}

/// The per-row interpolation context + native menu for one Players-page row. The
/// `context` supplies `id` as the entity-ref composite `"player:<uuid>"` (the
/// convention every point uses) and `name` as the display name (the
/// `PLAYERS_ROW_MENU.context_keys` promise — drill-down pages show it instead of a
/// bare uuid); the native menu is the mockup's inert Edit/Delete (`disabled` —
/// rendered visibly but with no link, no op wired yet).
pub(crate) fn player_row_meta(player_id: &str, display_name: &str) -> adminapi::RowMeta {
    adminapi::RowMeta {
        context: std::collections::HashMap::from([
            ("id".into(), format!("player:{player_id}")),
            ("name".into(), display_name.to_string()),
        ]),
        menu: vec![
            adminapi::MenuEntry {
                label: "Edit".into(),
                icon: "edit".into(),
                disabled: true,
                ..Default::default()
            },
            adminapi::MenuEntry {
                label: "Delete".into(),
                icon: "delete".into(),
                danger: true,
                disabled: true,
                ..Default::default()
            },
        ],
    }
}
