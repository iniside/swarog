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
            extensions: extension_entries(),
        })
    }
}

/// The cross-page extension entries inventory contributes to OTHER modules' pages.
/// ONE source of truth: the LOCAL `Item` attaches these via `with_extensions` and the
/// REMOTE `ItemData` carries the SAME vec, so a split admin sees the identical menu.
/// inventory drops a "View Inventory" drill-down onto THREE owner-declared points:
/// the accounts Players row menu (Navigate), the characters card menu (Modal), and the
/// character-modal footer (Modal). Each interpolates `{id}`/`{name}` from that point's
/// context — the owner's display name rides the link so the scoped view can title
/// itself without knowing the owning module.
pub(crate) fn extension_entries() -> Vec<adminapi::ExtensionEntry> {
    let entry = |point: &str, present| adminapi::ExtensionEntry {
        point: point.into(),
        label: "View Inventory".into(),
        icon: "inventory".into(),
        link: format!("{ADMIN_ITEM_ID}?owner={{id}}&owner_name={{name}}"),
        present,
        priority: 10,
    };
    vec![
        entry(
            accountsapi::admin::PLAYERS_ROW_MENU.id,
            adminapi::Present::Navigate,
        ),
        entry(
            charactersapi::admin::CHARACTERS_CARD_MENU.id,
            adminapi::Present::Modal,
        ),
        entry(
            charactersapi::admin::CHARACTER_MODAL_ACTIONS.id,
            adminapi::Present::Modal,
        ),
    ]
}

/// Renders the owners list (no `?owner=`) or one owner's items (`?owner=<type>:<id>`).
pub(crate) async fn admin_content(store: &Store, params: &adminapi::Params) -> anyhow::Result<adminapi::Content> {
    let owner = adminapi::param(params, "owner");
    if owner.is_empty() {
        admin_owners_list(store).await
    } else {
        // Display-only: the owner's name rides the drill-down link (`owner_name=`,
        // filled from the owning page's context) — inventory itself never learns
        // account/character display names.
        admin_owner_detail(store, owner, adminapi::param(params, "owner_name")).await
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

/// The drill-down view for one owner (`"<type>:<id>"`): its items. The header titles
/// itself with `owner_name` (carried by the drill-down link) when present, else the
/// uuid short form. A malformed owner param renders an error card (not a 500).
async fn admin_owner_detail(
    store: &Store,
    owner: &str,
    owner_name: &str,
) -> anyhow::Result<adminapi::Content> {
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

    let title = if owner_name.is_empty() {
        short_id(id).to_string()
    } else {
        owner_name.to_string()
    };
    Ok(adminapi::Content {
        // Owner-rendered header (avatar + display name/short id + item count): this
        // view is reached as a modal/drill-down FROM another page, so it carries its
        // own context header — the header alone identifies the owner (name + the mono
        // `<type>:<uuid>` ref); no KPI duplicates it (a full uuid in a 27px KPI cell
        // is exactly the mockup-divergence the restyle fixed).
        header: Some(adminapi::ContextHeader {
            avatar_text: initial(&title),
            avatar_color_key: palette(id),
            title,
            subtitle_mono: format!("{otype}:{id}"),
            right_note: format!("{} item(s)", holdings.len()),
        }),
        context: std::collections::HashMap::from([("id".into(), format!("{otype}:{id}"))]),
        table: Some(table),
        ..Default::default()
    })
}

/// The first hex group of a uuid — the short header title.
fn short_id(id: &str) -> &str {
    id.split('-').next().unwrap_or(id)
}

/// The first character (uppercased) of an id as avatar text.
fn initial(s: &str) -> String {
    s.chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".into())
}

/// Deterministic cycling-palette key (`av-0`..`av-5`) from a stable seed.
fn palette(seed: &str) -> String {
    format!("av-{}", seed.bytes().map(|b| b as usize).sum::<usize>() % 6)
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
