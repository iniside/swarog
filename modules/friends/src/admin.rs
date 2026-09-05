//! The read-only "Friends" page under `Player Support`: the social graph's three counts,
//! the newest relations, and a per-player drill-down (`?player=<uuid>`).
//!
//! `UILayout/GameOps Admin.dc.html` has NO social section, so there is no 1:1 spec to
//! translate. The page is COMPOSED from the shipped `adminapi` widget vocabulary in the
//! shape the mockup's Players page gives (a KPI row over a table whose player column is an
//! avatar chip plus a name, a status pill per row, a row drill-down) — no new widget, and
//! nothing decorative: every value is read from `friends.edges` or from the `accounts`
//! directory.
//!
//! There is no [`adminapi::AdminSubmit`]: the operator has no friends action, and a
//! relation is the two players' consent to hold — not an operator's to mint.

use std::collections::HashMap;
use std::sync::Arc;

use accountsapi::{PlayerSummary, MAX_LOOKUP_IDS};
use friendsapi::{
    DIRECTION_INCOMING, DIRECTION_OUTGOING, STATE_ACCEPTED, STATE_PENDING,
};

use crate::service::is_uuid_text;
use crate::store::AdminEdgeRow;
use crate::Service;

pub(crate) const ADMIN_ITEM_ID: &str = "friends";
pub(crate) const ADMIN_SECTION: &str = "Player Support";
pub(crate) const ADMIN_LABEL: &str = "Friends";

/// The drill-down param. `player:<uuid>` (the `PLAYERS_ROW_MENU` entity-ref spelling) and a
/// bare uuid are both accepted, so a hand-typed link works.
const PARAM_PLAYER: &str = "player";
const PLAYER_REF_PREFIX: &str = "player:";

/// How many relations either view lists. Both views are unpaged — the operator narrows by
/// drilling into one player, not by walking the table.
const PAGE: i64 = 50;

/// The route this page answers on. The portal derives it from `slug(LABEL)`
/// (`admin::resolve_items`), NEVER from the item id, so every self-link is built from HERE:
/// a link built from the id 404s the moment the two differ, and nothing forces them to agree.
fn admin_slug() -> String {
    adminapi::slug(ADMIN_LABEL)
}

fn drill_link(player_id: &str) -> String {
    format!("{}?{PARAM_PLAYER}={player_id}", admin_slug())
}

/// This module's cross-page contribution: a "View Friends" drill-down on each Players row.
/// The link interpolates `{id}` — a key `PLAYERS_ROW_MENU` DECLARES, and the value it
/// supplies is the `player:<uuid>` entity ref [`build_content`] strips. LOCAL
/// `Item::with_extensions` and REMOTE `ItemData::extensions` carry this SAME vec, so the two
/// cannot drift.
pub(crate) fn extension_entries() -> Vec<adminapi::ExtensionEntry> {
    vec![adminapi::ExtensionEntry {
        point: accountsapi::admin::PLAYERS_ROW_MENU.id.into(),
        label: "View Friends".into(),
        icon: "users".into(),
        link: format!("{}?{PARAM_PLAYER}={{id}}", admin_slug()),
        present: adminapi::Present::Navigate,
        priority: 0,
    }]
}

/// The page, LOCAL and REMOTE alike. It is INFALLIBLE by construction: a malformed param, a
/// store failure and a directory outage each render a card. The portal forwards every page's
/// params to every resolved provider and turns an `Err` into an error card whose section and
/// label collapse to the item id (`admin::resolve_items`), so an `Err` here would move this
/// page out of its sidebar section — for a param that belongs to another page, split-only.
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
    let counts = match svc.store.admin_counts(None, STATE_PENDING, STATE_ACCEPTED).await {
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

    let ids: Vec<String> = rows
        .iter()
        .flat_map(|r| [r.requester_id.clone(), r.addressee_id.clone()])
        .collect();
    let names = Names::resolve(svc, ids).await;

    let mut table = adminapi::Table {
        columns: vec![
            "WHEN".into(),
            "REQUESTER".into(),
            "ADDRESSEE".into(),
            "STATE".into(),
            "EDGE".into(),
        ],
        rows: Vec::with_capacity(rows.len()),
        ..Default::default()
    };
    for r in &rows {
        table.rows.push(vec![
            adminapi::Cell::text(short_ts(&r.created_at)),
            player_cell(&names, &r.requester_id),
            player_cell(&names, &r.addressee_id),
            state_cell(&r.state),
            adminapi::Cell::mono(short_uuid(&r.edge_id)),
        ]);
    }

    adminapi::Content {
        kpis: kpis(counts, rows.len(), truncated, false, &names),
        table: Some(table),
        ..Default::default()
    }
}

async fn player_view(svc: &Service, player_id: &str) -> adminapi::Content {
    let counts = match svc
        .store
        .admin_counts(Some(player_id), STATE_PENDING, STATE_ACCEPTED)
        .await
    {
        Ok(counts) => counts,
        Err(e) => return error_content(&store_message(e)),
    };
    let mut rows = match svc.store.admin_recent(Some(player_id), PAGE + 1).await {
        Ok(rows) => rows,
        Err(e) => return error_content(&store_message(e)),
    };
    let truncated = rows.len() as i64 > PAGE;
    rows.truncate(PAGE as usize);

    let mut ids: Vec<String> = vec![player_id.to_string()];
    ids.extend(rows.iter().map(|r| other_side(r, player_id).to_string()));
    let names = Names::resolve(svc, ids).await;

    let mut table = adminapi::Table {
        columns: vec![
            "WHEN".into(),
            "OTHER PLAYER".into(),
            "DIRECTION".into(),
            "STATE".into(),
            "EDGE".into(),
        ],
        rows: Vec::with_capacity(rows.len()),
        ..Default::default()
    };
    for r in &rows {
        table.rows.push(vec![
            adminapi::Cell::text(short_ts(&r.created_at)),
            player_cell(&names, other_side(r, player_id)),
            direction_cell(r.requester_id.eq_ignore_ascii_case(player_id)),
            state_cell(&r.state),
            adminapi::Cell::mono(short_uuid(&r.edge_id)),
        ]);
    }

    let title = names.label(player_id);
    adminapi::Content {
        header: Some(adminapi::ContextHeader {
            avatar_text: initial(&title),
            avatar_color_key: palette(player_id),
            title,
            subtitle_mono: format!("{PLAYER_REF_PREFIX}{player_id}"),
            right_note: page_note(truncated).into(),
        }),
        kpis: kpis(counts, rows.len(), truncated, true, &names),
        table: Some(table),
        ..Default::default()
    }
}

/// The graph's three counts plus the listed-row count, and a fifth card ONLY while the
/// directory is down — a page that silently swapped every handle for a uuid would read as a
/// graph of deleted accounts.
///
/// The counts are aggregates while `listed` counts the rows below them, which is why they
/// carry different subtitles: a capped table beside an uncapped total otherwise reads as one
/// number disagreeing with itself. `scoped` is the same rule applied to the OTHER axis — the
/// drill-down's totals cover one player, so a subtitle claiming the whole record would assert
/// a scope the number does not have.
fn kpis(
    counts: (i64, i64, i64),
    listed: usize,
    truncated: bool,
    scoped: bool,
    names: &Names,
) -> Vec<adminapi::Kpi> {
    let (total, pending, accepted) = counts;
    let mut kpis = vec![
        adminapi::Kpi {
            label: "Relations".into(),
            value: total.to_string(),
            sub: if scoped {
                "every pair this player is in"
            } else {
                "every pair on record"
            }
            .into(),
        },
        adminapi::Kpi {
            label: "Pending".into(),
            value: pending.to_string(),
            sub: "awaiting an answer".into(),
        },
        adminapi::Kpi {
            label: "Accepted".into(),
            value: accepted.to_string(),
            sub: "mutual".into(),
        },
        adminapi::Kpi {
            label: "Listed".into(),
            value: listed.to_string(),
            sub: page_note(truncated).into(),
        },
    ];
    if names.degraded {
        kpis.push(adminapi::Kpi {
            label: "Directory".into(),
            value: "unavailable".into(),
            sub: "showing player ids".into(),
        });
    }
    kpis
}

/// A store failure is a CARD, not an `Err` — see [`build_content`]. The message is the
/// operator's only clue, so it carries the driver's text rather than a generic apology.
fn store_message(e: sqlx::Error) -> String {
    format!("Could not read the social graph: {e}")
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

fn other_side<'a>(row: &'a AdminEdgeRow, player_id: &str) -> &'a str {
    if row.requester_id.eq_ignore_ascii_case(player_id) {
        &row.addressee_id
    } else {
        &row.requester_id
    }
}

/// The rendered names for one page's players.
///
/// `friends` already REQUIRES `accountsapi::Directory`, so this page resolves handles rather
/// than printing uuids the operator would have to look up elsewhere. Both ways the lookup can
/// fall short degrade to the id, never to an error: a MISS keeps its row with a short uuid,
/// and an outage — this call is a network hop to accounts in a split — sets `degraded`, which
/// [`kpis`] reports on the page itself.
struct Names {
    by_id: HashMap<String, String>,
    degraded: bool,
}

impl Names {
    async fn resolve(svc: &Service, ids: Vec<String>) -> Names {
        let mut wanted: Vec<String> = Vec::new();
        for id in ids {
            if !wanted.iter().any(|w: &String| w.eq_ignore_ascii_case(&id)) {
                wanted.push(id);
            }
        }
        wanted.truncate(MAX_LOOKUP_IDS);
        if wanted.is_empty() {
            return Names {
                by_id: HashMap::new(),
                degraded: false,
            };
        }
        // `Service::directory()` PANICS when unresolved; the admin page reports it as a card
        // instead, because a panic here would take down an unrelated page's request too.
        let Some(directory) = svc.directory.get() else {
            return Names {
                by_id: HashMap::new(),
                degraded: true,
            };
        };
        match directory.players_by_id(wanted).await {
            Ok(summaries) => Names {
                by_id: summaries
                    .into_iter()
                    .map(|s: PlayerSummary| (s.player_id.to_ascii_lowercase(), s.handle))
                    .collect(),
                degraded: false,
            },
            Err(_) => Names {
                by_id: HashMap::new(),
                degraded: true,
            },
        }
    }

    /// The handle when accounts knows one, the short uuid otherwise — a directory miss must
    /// still name its row.
    fn label(&self, player_id: &str) -> String {
        match self.by_id.get(&player_id.to_ascii_lowercase()) {
            Some(handle) if !handle.is_empty() => handle.clone(),
            _ => short_uuid(player_id).to_string(),
        }
    }
}

/// The mockup's player cell: an avatar chip plus the name, the whole thing a drill-down
/// anchor. `Cell` renders one line, so the mockup's second `#uid` line lives on the
/// drill-down's context header instead.
fn player_cell(names: &Names, player_id: &str) -> adminapi::Cell {
    let label = names.label(player_id);
    adminapi::Cell {
        text: label.clone(),
        icon_text: initial(&label),
        icon_color_key: palette(player_id),
        link: drill_link(player_id),
        ..Default::default()
    }
}

/// `state` is the table's own vocabulary, but a row written by a future version must not be
/// asserted as a known class, so anything else takes the neutral chip.
fn state_cell(state: &str) -> adminapi::Cell {
    let badge = match state {
        STATE_ACCEPTED => "green",
        STATE_PENDING => "amber",
        _ => "grey",
    };
    adminapi::Cell {
        text: state.into(),
        badge: badge.into(),
        ..Default::default()
    }
}

/// Relative to the DRILLED-INTO player, the only caller this page has.
fn direction_cell(requested_by_this_player: bool) -> adminapi::Cell {
    if requested_by_this_player {
        adminapi::Cell {
            text: DIRECTION_OUTGOING.into(),
            badge: "blue".into(),
            ..Default::default()
        }
    } else {
        adminapi::Cell {
            text: DIRECTION_INCOMING.into(),
            badge: "grey".into(),
            ..Default::default()
        }
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
            extensions: extension_entries(),
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
/// characters/inventory/notifications/wallet ON PURPOSE: one player must draw the same colour
/// on every page of the portal, which a differently-mixed hash would silently break.
fn palette(seed: &str) -> String {
    format!("av-{}", seed.bytes().map(|b| b as usize).sum::<usize>() % 6)
}
