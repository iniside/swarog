use async_trait::async_trait;
use opsapi::Error;

use crate::{internal, Inner, Owner, Store, ADMIN_ITEM_ID, ADMIN_LABEL, ADMIN_SECTION};

const ADMIN_OWNERS_LIMIT: i64 = 200;

// ============================================================================
// Admin — two views off the SAME item, switched by the ?owner= drill-down param.
// ============================================================================

#[async_trait]
impl adminapi::AdminData for Inner {
    /// The admin fan-out (`admin.adminData` on the edge): this module's page as
    /// `adminapi::ItemData`, scoped by the request `params` (which now ride the wire).
    /// No params → owners LIST; `?owner=<type>:<id>` → that owner's items — the same
    /// dispatch LOCAL and REMOTE. A malformed/foreign `owner` renders error-content,
    /// never an `Err` (the foreign-params tolerance contract on `AdminData`).
    async fn admin_data(&self, params: adminapi::Params) -> Result<adminapi::ItemData, Error> {
        let content = admin_content(&self.store, &params)
            .await
            .map_err(internal)?;
        Ok(adminapi::ItemData {
            id: ADMIN_ITEM_ID.into(),
            section: ADMIN_SECTION.into(),
            label: ADMIN_LABEL.into(),
            content,
            ..Default::default()
        })
    }
}

/// Renders the owners list (no `?owner=`) or one owner's items (`?owner=<type>:<id>`).
pub(crate) async fn admin_content(store: &Store, params: &adminapi::Params) -> anyhow::Result<adminapi::Content> {
    let owner = adminapi::param(params, "owner");
    if owner.is_empty() {
        admin_owners_list(store).await
    } else {
        admin_owner_detail(store, owner).await
    }
}

/// The top-level view: KPIs plus one row per owner, the owner-id cell linking to that
/// owner's items page (`inventory?owner=<type>:<id>`).
async fn admin_owners_list(store: &Store) -> anyhow::Result<adminapi::Content> {
    let (holdings, owners) = store.stats().await?;
    let rows = store.list_owners(ADMIN_OWNERS_LIMIT).await?;

    let mut table = adminapi::Table {
        columns: vec!["OWNER".into(), "OWNER ID".into(), "ITEMS".into(), "TOTAL QTY".into()],
        rows: Vec::with_capacity(rows.len()),
        ..Default::default()
    };
    for o in rows {
        table.rows.push(vec![
            adminapi::Cell {
                text: o.owner_type.clone(),
                badge: owner_badge(&o.owner_type).into(),
                ..Default::default()
            },
            adminapi::Cell {
                text: o.owner_id.clone(),
                mono: true,
                link: format!("{ADMIN_ITEM_ID}?owner={}:{}", o.owner_type, o.owner_id),
                ..Default::default()
            },
            adminapi::Cell::text(o.items.to_string()),
            adminapi::Cell::text(o.qty.to_string()),
        ]);
    }

    Ok(adminapi::Content {
        kpis: vec![
            adminapi::Kpi { label: "Holdings".into(), value: holdings.to_string(), sub: String::new() },
            adminapi::Kpi { label: "Owners".into(), value: owners.to_string(), sub: "players + characters".into() },
        ],
        table: Some(table),
        form: None,
        ..Default::default()
    })
}

/// The drill-down view for one owner (`"<type>:<id>"`): its items. A malformed owner
/// param renders an error card (not a 500).
async fn admin_owner_detail(store: &Store, owner: &str) -> anyhow::Result<adminapi::Content> {
    let Some((otype, id)) = owner.split_once(':') else {
        return Ok(error_content("Invalid owner — expected player:<uuid> or character:<uuid>."));
    };
    if otype != "player" && otype != "character" {
        return Ok(error_content("Invalid owner — expected player:<uuid> or character:<uuid>."));
    }
    if !is_uuid(id) {
        return Ok(error_content("Invalid owner id — not a uuid."));
    }

    let holdings = store.list(&Owner::new(otype, id)).await?;
    let mut table = adminapi::Table {
        columns: vec!["ITEM".into(), "QTY".into()],
        rows: Vec::with_capacity(holdings.len()),
        ..Default::default()
    };
    for h in &holdings {
        table.rows.push(vec![
            adminapi::Cell::text(&h.item_name),
            adminapi::Cell::text(h.quantity.to_string()),
        ]);
    }

    Ok(adminapi::Content {
        kpis: vec![
            adminapi::Kpi { label: "Owner".into(), value: otype.into(), sub: owner_badge_sub(otype).into() },
            adminapi::Kpi { label: "Owner ID".into(), value: id.into(), sub: String::new() },
            adminapi::Kpi { label: "Items".into(), value: holdings.len().to_string(), sub: String::new() },
        ],
        table: Some(table),
        form: None,
        ..Default::default()
    })
}

/// A canonical 8-4-4-4-12 hex uuid check — guards the drill-down param before it
/// reaches the store's `$id::uuid` cast, so a malformed id renders an error card
/// instead of a Postgres cast error (a 500). Avoids a uuid dependency (Go's `isUUID`).
fn is_uuid(s: &str) -> bool {
    if s.len() != 36 {
        return false;
    }
    for (i, c) in s.chars().enumerate() {
        if i == 8 || i == 13 || i == 18 || i == 23 {
            if c != '-' {
                return false;
            }
        } else if !c.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

fn owner_badge(owner_type: &str) -> &'static str {
    if owner_type == "character" {
        "blue"
    } else {
        "grey"
    }
}

fn owner_badge_sub(owner_type: &str) -> &'static str {
    if owner_type == "character" {
        "character-scoped"
    } else {
        "player-scoped"
    }
}

/// Renders a single message as an error card (a lone KPI, so the page is a clean
/// card, never a 500).
fn error_content(msg: &str) -> adminapi::Content {
    adminapi::Content {
        kpis: vec![adminapi::Kpi { label: "Error".into(), value: msg.into(), sub: String::new() }],
        table: None,
        form: None,
        ..Default::default()
    }
}
