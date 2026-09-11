//! The read-only "Cloud Saves" page under `Player Support`: what the object table holds
//! across every owner, a per-player drill-down (`?player=<uuid>`), and the purge backlog.
//!
//! `UILayout/GameOps Admin.dc.html` has no cloud-save section, so there is no 1:1 spec to
//! translate. The page is COMPOSED from the shipped `adminapi` widget vocabulary in the
//! shape the mockup's Players page gives (a KPI row over a table whose player column is an
//! avatar chip plus a label, a status pill per row, a row drill-down) — no new widget.
//!
//! There is no [`adminapi::AdminSubmit`]: a saved document is the player's, and the one
//! operator write that exists over this data — erasing it — is the deletion purge's, which
//! runs from a durable tombstone rather than from a form.

use std::sync::Arc;

use storageapi::ACL_PUBLIC_READ;

use crate::service::is_uuid_text;
use crate::store::AdminCounts;
use crate::Service;

pub(crate) const ADMIN_ITEM_ID: &str = "storage";
pub(crate) const ADMIN_SECTION: &str = "Player Support";
pub(crate) const ADMIN_LABEL: &str = "Cloud Saves";

/// The drill-down param. `player:<uuid>` (the `PLAYERS_ROW_MENU` entity-ref spelling) and a
/// bare uuid are both accepted, so a hand-typed link works. The name is the one the
/// player-scoped pages already answer on, so a forwarded link drills all of them alike.
const PARAM_PLAYER: &str = "player";
const PLAYER_REF_PREFIX: &str = "player:";

/// How many objects either view lists. Both views are unpaged — the operator narrows by
/// drilling into one player, not by walking the table.
const PAGE: i64 = 50;

/// The route this page answers on. The portal derives it from `slug(LABEL)`
/// (`admin::resolve_items`), NEVER from the item id — which differs here — so every
/// self-link is built from HERE.
fn admin_slug() -> String {
    adminapi::slug(ADMIN_LABEL)
}

fn drill_link(player_id: &str) -> String {
    format!("{}?{PARAM_PLAYER}={player_id}", admin_slug())
}

/// The page, LOCAL and REMOTE alike. It is INFALLIBLE by construction: a malformed param
/// and a store failure each render a card. The portal forwards every page's params to every
/// resolved provider and turns an `Err` into an error card whose section and label collapse
/// to the item id (`admin::resolve_items`), so an `Err` here would move this page out of its
/// sidebar section — for a param that belongs to another page, split-only.
pub(crate) async fn build_content(svc: &Service, params: &adminapi::Params) -> adminapi::Content {
    let raw = adminapi::param(params, PARAM_PLAYER).trim();
    if raw.is_empty() {
        return overview(svc).await;
    }
    let player_id = raw.strip_prefix(PLAYER_REF_PREFIX).unwrap_or(raw);
    if !is_uuid_text(player_id) {
        return error_content("Invalid player — expected a uuid.");
    }
    player_view(svc, player_id).await
}

async fn overview(svc: &Service) -> adminapi::Content {
    let counts = match svc.store.admin_counts(None).await {
        Ok(counts) => counts,
        Err(e) => return error_content(&store_message(e)),
    };
    // One row past the page decides "older exist" — the idiom `Player::list` uses.
    let mut rows = match svc.store.admin_recent(None, PAGE + 1).await {
        Ok(rows) => rows,
        Err(e) => return error_content(&store_message(e)),
    };
    let truncated = rows.len() as i64 > PAGE;
    rows.truncate(PAGE as usize);

    let (purges, oldest_purge) = match svc.store.pending_purges().await {
        Ok(backlog) => backlog,
        Err(e) => return error_content(&store_message(e)),
    };

    let mut table = adminapi::Table {
        columns: vec![
            "UPDATED".into(),
            "OWNER".into(),
            "COLLECTION".into(),
            "KEY".into(),
            "VERSION".into(),
            "ACL".into(),
            "SIZE".into(),
        ],
        rows: Vec::with_capacity(rows.len()),
        ..Default::default()
    };
    for r in &rows {
        table.rows.push(vec![
            adminapi::Cell::text(short_ts(&r.updated_at)),
            owner_cell(&r.owner_id),
            adminapi::Cell::text(r.collection.as_str()),
            adminapi::Cell::mono(r.key.as_str()),
            adminapi::Cell::text(r.version.to_string()),
            acl_cell(&r.acl),
            adminapi::Cell::text(bytes_text(r.bytes)),
        ]);
    }

    let mut tiles = kpis(&counts, rows.len(), truncated, false);
    tiles.push(purge_kpi(purges, &oldest_purge));
    adminapi::Content {
        kpis: tiles,
        table: Some(table),
        ..Default::default()
    }
}

async fn player_view(svc: &Service, player_id: &str) -> adminapi::Content {
    let counts = match svc.store.admin_counts(Some(player_id)).await {
        Ok(counts) => counts,
        Err(e) => return error_content(&store_message(e)),
    };
    let mut rows = match svc.store.admin_recent(Some(player_id), PAGE + 1).await {
        Ok(rows) => rows,
        Err(e) => return error_content(&store_message(e)),
    };
    let truncated = rows.len() as i64 > PAGE;
    rows.truncate(PAGE as usize);

    let mut table = adminapi::Table {
        columns: vec![
            "UPDATED".into(),
            "COLLECTION".into(),
            "KEY".into(),
            "VERSION".into(),
            "ACL".into(),
            "SIZE".into(),
        ],
        rows: Vec::with_capacity(rows.len()),
        ..Default::default()
    };
    for r in &rows {
        table.rows.push(vec![
            adminapi::Cell::text(short_ts(&r.updated_at)),
            adminapi::Cell::text(r.collection.as_str()),
            adminapi::Cell::mono(r.key.as_str()),
            adminapi::Cell::text(r.version.to_string()),
            acl_cell(&r.acl),
            adminapi::Cell::text(bytes_text(r.bytes)),
        ]);
    }

    let title = short_uuid(player_id).to_string();
    adminapi::Content {
        header: Some(adminapi::ContextHeader {
            avatar_text: initial(&title),
            avatar_color_key: palette(player_id),
            title,
            subtitle_mono: format!("{PLAYER_REF_PREFIX}{player_id}"),
            right_note: page_note(truncated).into(),
        }),
        kpis: kpis(&counts, rows.len(), truncated, true),
        table: Some(table),
        ..Default::default()
    }
}

/// The table's totals plus the listed-row count.
///
/// The first three are aggregates while `listed` counts the rows below them, which is why
/// they carry different subtitles: a capped table beside an uncapped total otherwise reads
/// as one number disagreeing with itself. The second tile switches with the scope —
/// `owners` is 1 for a drill-down, where the collections the player spreads across is the
/// number that says something.
fn kpis(counts: &AdminCounts, listed: usize, truncated: bool, scoped: bool) -> Vec<adminapi::Kpi> {
    vec![
        adminapi::Kpi {
            label: "Objects".into(),
            value: counts.objects.to_string(),
            sub: if scoped {
                "this player's saves"
            } else {
                "every saved object"
            }
            .into(),
        },
        if scoped {
            adminapi::Kpi {
                label: "Collections".into(),
                value: counts.collections.to_string(),
                sub: "this player writes to".into(),
            }
        } else {
            adminapi::Kpi {
                label: "Owners".into(),
                value: counts.owners.to_string(),
                sub: "players holding a save".into(),
            }
        },
        adminapi::Kpi {
            label: "Public".into(),
            value: counts.public.to_string(),
            sub: format!("{ACL_PUBLIC_READ}, readable by whoever can name them"),
        },
        adminapi::Kpi {
            label: "Listed".into(),
            value: listed.to_string(),
            sub: page_note(truncated).into(),
        },
    ]
}

/// A store failure is a CARD, not an `Err` — see [`build_content`]. The message is the
/// operator's only clue, so it carries the driver's text rather than a generic apology.
fn store_message(e: sqlx::Error) -> String {
    format!("Could not read the object store: {e}")
}

fn error_content(msg: &str) -> adminapi::Content {
    adminapi::Content {
        kpis: vec![adminapi::Kpi {
            label: "Error".into(),
            value: msg.into(),
            sub: String::new(),
        }],
        ..Default::default()
    }
}

/// The mockup's player cell: an avatar chip plus a label, the whole thing a drill-down
/// anchor. The label is the id's first group, not a handle: `storage` resolves the owner
/// from the caller's own identity and holds no `accountsapi::Directory`, so a name here
/// would be one this module cannot read. The chip's colour is the SAME function the
/// player-bearing pages mix, so one player draws one colour portal-wide.
fn owner_cell(owner_id: &str) -> adminapi::Cell {
    let label = short_uuid(owner_id);
    adminapi::Cell {
        text: label.into(),
        icon_text: initial(label),
        icon_color_key: palette(owner_id),
        link: drill_link(owner_id),
        ..Default::default()
    }
}

/// Only a shared object earns a colour: `private` is the state an operator scans past, and
/// a value some future version writes must not be asserted as a known class either.
fn acl_cell(acl: &str) -> adminapi::Cell {
    let badge = if acl == ACL_PUBLIC_READ { "amber" } else { "grey" };
    adminapi::Cell {
        text: acl.into(),
        badge: badge.into(),
        ..Default::default()
    }
}

/// The synchronous LOCAL render: the store reads are async, the `RenderFn` contract is not,
/// so it bridges via `block_in_place` (requires the multi-thread rt).
pub(crate) fn admin_render(
    svc: &Arc<Service>,
    params: &adminapi::Params,
) -> anyhow::Result<adminapi::Content> {
    let svc = svc.clone();
    let params = params.clone();
    Ok(tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(build_content(&svc, &params))
    }))
}

#[async_trait::async_trait]
impl adminapi::AdminData for Service {
    /// The admin fan-out READ (`admin.adminData`): the same page a local render produces.
    /// It NEVER returns `Err` — foreign-params tolerance is a contract, and the failure
    /// modes this page has are cards ([`build_content`]).
    async fn admin_data(
        &self,
        params: adminapi::Params,
    ) -> Result<adminapi::ItemData, opsapi::Error> {
        Ok(adminapi::ItemData {
            id: ADMIN_ITEM_ID.into(),
            section: ADMIN_SECTION.into(),
            label: ADMIN_LABEL.into(),
            content: build_content(self, &params).await,
            extensions: Vec::new(),
        })
    }
}

/// A capped table MUST say it is capped: the page has no cursor, so a row count beside 50
/// rows otherwise reads as the whole story.
fn page_note(truncated: bool) -> &'static str {
    if truncated {
        "newest shown — older rows exist"
    } else {
        "all of them"
    }
}

/// The purge-backlog KPI: deletion tombstones this module's sweep still owes work on.
/// Always rendered, `0` included — an absent tile would read as an empty backlog rather
/// than as a page that never asked.
fn purge_kpi(pending: i64, oldest: &str) -> adminapi::Kpi {
    adminapi::Kpi {
        label: "Purges Pending".into(),
        value: pending.to_string(),
        sub: if pending == 0 {
            String::new()
        } else {
            format!("oldest {}", short_ts(oldest))
        },
    }
}

/// Whole kibibytes once a document passes 1024 bytes: the operator is looking for the save
/// that is orders of magnitude off, not for its exact byte count.
fn bytes_text(bytes: i64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else {
        format!("{} KiB", bytes / 1024)
    }
}

fn short_ts(ts: &str) -> &str {
    ts.get(..16).unwrap_or(ts)
}

fn short_uuid(uuid: &str) -> &str {
    uuid.split('-').next().unwrap_or(uuid)
}

fn initial(label: &str) -> String {
    label
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".into())
}

/// Deterministic avatar-palette key (`.av-0`…`.av-5`). Byte-identical to the same helper in
/// characters/inventory/notifications/wallet/friends ON PURPOSE: one player must draw the
/// same colour on every page of the portal, which a differently-mixed hash would silently
/// break.
fn palette(seed: &str) -> String {
    format!("av-{}", seed.bytes().map(|b| b as usize).sum::<usize>() % 6)
}
