//! The "Groups" page under `Player Support`: every group with its live counts, a
//! per-group drill-down (`?group=<uuid>`) listing that group's rows, and ONE operator
//! action — promoting a member to admin.
//!
//! `UILayout/GameOps Admin.dc.html` has no social section, so there is no 1:1 spec to
//! translate. The page is COMPOSED from the shipped `adminapi` widget vocabulary in the
//! shape the mockup's Players page gives (a KPI row over a table whose entity column is an
//! avatar chip plus a name, a status pill per row, a row drill-down) — no new widget, and
//! nothing decorative: every value is read from `groups.groups`/`groups.memberships` or
//! from the `accounts` directory.
//!
//! **Why the one write is a promotion and nothing else.** A group is the players' own
//! state, not an operator's to mint — creating, joining and kicking all stay in the player
//! face. But the last-admin rule makes one state unrecoverable from inside the domain: an
//! abandoned sole admin never leaves, so the group can never accept, invite, kick or be
//! deleted again. Promoting a second admin is the only exit, and there is no player-facing
//! op for it (promote/demote is a stated non-goal of the player contract).

use std::collections::HashMap;
use std::sync::Arc;

use accountsapi::{PlayerSummary, MAX_LOOKUP_IDS};
use bus::AnyTx;
use groupsapi::{
    JOIN_INVITE, JOIN_OPEN, JOIN_REQUEST, ROLE_ADMIN, ROLE_MEMBER, STATE_INVITED, STATE_MEMBER,
    STATE_REQUESTED,
};

use crate::service::is_uuid_text;
use crate::store::{AdminGroupRow, ALL_ROWS};
use crate::Service;

pub(crate) const ADMIN_ITEM_ID: &str = "groups";
pub(crate) const ADMIN_SECTION: &str = "Player Support";
pub(crate) const ADMIN_LABEL: &str = "Groups";

/// The drill-down param. `group:<uuid>` (the entity-ref spelling every other page's
/// context uses) and a bare uuid are both accepted, so a hand-typed link works.
const PARAM_GROUP: &str = "group";
const GROUP_REF_PREFIX: &str = "group:";

/// How many rows either view lists. Both views are unpaged — the operator narrows by
/// drilling into one group, not by walking the table.
const PAGE: i64 = 50;

/// The form's fields. `GROUP_FIELD` shares its name with [`PARAM_GROUP`] deliberately: the
/// drill-down's group is the one the form preselects.
const ACTION_FIELD: &str = "action";
const ACTION_PROMOTE: &str = "promote-admin";
const GROUP_FIELD: &str = "group";
const PLAYER_FIELD: &str = "player";

/// The route this page answers on. The portal derives it from `slug(LABEL)`
/// (`admin::resolve_items`), NEVER from the item id, so every self-link is built from HERE:
/// a link built from the id 404s the moment the two differ, and nothing forces them to agree.
fn admin_slug() -> String {
    adminapi::slug(ADMIN_LABEL)
}

fn drill_link(group_id: &str) -> String {
    format!("{}?{PARAM_GROUP}={group_id}", admin_slug())
}

/// The page, LOCAL and REMOTE alike. It is INFALLIBLE by construction: a malformed param, a
/// store failure and a directory outage each render a card. The portal forwards every page's
/// params to every resolved provider and turns an `Err` into an error card whose section and
/// label collapse to the item id (`admin::resolve_items`), so an `Err` here would move this
/// page out of its sidebar section — for a param that belongs to another page, split-only.
pub(crate) async fn build_content(svc: &Service, params: &adminapi::Params) -> adminapi::Content {
    let raw = adminapi::param(params, PARAM_GROUP).trim();
    if raw.is_empty() {
        return overview(svc).await;
    }
    let group_id = raw.strip_prefix(GROUP_REF_PREFIX).unwrap_or(raw);
    if !is_uuid_text(group_id) {
        return error_content("Invalid group — expected a uuid.");
    }
    group_view(svc, group_id).await
}

async fn overview(svc: &Service) -> adminapi::Content {
    let totals = match svc.store.admin_totals(STATE_MEMBER).await {
        Ok(totals) => totals,
        Err(e) => return error_content(&store_message(e)),
    };
    // One row past the page decides "older exist" — the idiom `Player::list_mine` uses.
    let mut rows = match svc.store.admin_recent_groups(STATE_MEMBER, PAGE + 1).await {
        Ok(rows) => rows,
        Err(e) => return error_content(&store_message(e)),
    };
    let truncated = rows.len() as i64 > PAGE;
    rows.truncate(PAGE as usize);

    let mut table = adminapi::Table {
        columns: vec![
            "WHEN".into(),
            "GROUP".into(),
            "POLICY".into(),
            "MEMBERS".into(),
            "PENDING".into(),
            "ID".into(),
        ],
        rows: Vec::with_capacity(rows.len()),
        ..Default::default()
    };
    for g in &rows {
        table.rows.push(vec![
            adminapi::Cell::text(short_ts(&g.created_at)),
            group_cell(g),
            policy_cell(&g.join_policy),
            adminapi::Cell::text(g.members.to_string()),
            adminapi::Cell::text(g.pending.to_string()),
            adminapi::Cell::mono(short_uuid(&g.group_id)),
        ]);
    }

    let (groups, members, pending) = totals;
    adminapi::Content {
        kpis: vec![
            count_kpi("Groups", groups, "on record"),
            count_kpi("Members", members, "across every group"),
            count_kpi("Pending", pending, "invited or requested"),
            count_kpi("Listed", rows.len() as i64, page_note(truncated)),
        ],
        table: Some(table),
        form: Some(build_form(&rows)),
        ..Default::default()
    }
}

async fn group_view(svc: &Service, group_id: &str) -> adminapi::Content {
    let group = match svc.store.admin_group(STATE_MEMBER, group_id).await {
        Ok(Some(group)) => group,
        Ok(None) => return error_content("No group with that id."),
        Err(e) => return error_content(&store_message(e)),
    };
    let mut rows = match svc
        .store
        .page_group(&group.group_id, ALL_ROWS, None, PAGE + 1)
        .await
    {
        Ok(rows) => rows,
        Err(e) => return error_content(&store_message(e)),
    };
    let truncated = rows.len() as i64 > PAGE;
    rows.truncate(PAGE as usize);

    let names = Names::resolve(svc, rows.iter().map(|r| r.player_id.clone()).collect()).await;

    let mut table = adminapi::Table {
        columns: vec![
            "WHEN".into(),
            "PLAYER".into(),
            "STATE".into(),
            "ROLE".into(),
            "ID".into(),
        ],
        rows: Vec::with_capacity(rows.len()),
        ..Default::default()
    };
    for r in &rows {
        table.rows.push(vec![
            adminapi::Cell::text(short_ts(&r.created_at)),
            player_cell(&names, &r.player_id),
            state_cell(&r.state),
            role_cell(&r.role),
            adminapi::Cell::mono(short_uuid(&r.player_id)),
        ]);
    }

    let mut kpis = vec![
        count_kpi("Members", group.members, "this group"),
        count_kpi("Pending", group.pending, "invited or requested"),
        adminapi::Kpi {
            label: "Join policy".into(),
            value: group.join_policy.clone(),
            sub: policy_note(&group.join_policy).into(),
        },
        count_kpi("Listed", rows.len() as i64, page_note(truncated)),
    ];
    if names.degraded {
        kpis.push(adminapi::Kpi {
            label: "Directory".into(),
            value: "unavailable".into(),
            sub: "showing player ids".into(),
        });
    }

    let title = group.name.clone();
    adminapi::Content {
        header: Some(adminapi::ContextHeader {
            avatar_text: initial(&title),
            avatar_color_key: palette(&group.group_id),
            title,
            subtitle_mono: format!("{GROUP_REF_PREFIX}{}", group.group_id),
            right_note: page_note(truncated).into(),
        }),
        kpis,
        table: Some(table),
        form: Some(build_form(std::slice::from_ref(&group))),
        ..Default::default()
    }
}

fn count_kpi(label: &str, value: i64, sub: &str) -> adminapi::Kpi {
    adminapi::Kpi {
        label: label.into(),
        value: value.to_string(),
        sub: sub.into(),
    }
}

/// A store failure is a CARD, not an `Err` — see [`build_content`]. The message is the
/// operator's only clue, so it carries the driver's text rather than a generic apology.
fn store_message(e: sqlx::Error) -> String {
    format!("Could not read the group roster: {e}")
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

// ============================================================================
// The form
// ============================================================================

/// The one operator action, present on EVERY render because the portal posts a form back to
/// the bare page slug (`/admin/<slug>`, query dropped — `admin::render_page`): a form built
/// only on the drill-down would re-render as the overview on POST and answer 405.
///
/// The group is a SELECT over the groups this render listed, not free text — so the posted
/// group is rendered evidence, and a value naming no group means the page the operator
/// submitted from is stale rather than mistyped. The player is free text: it is the
/// operator's input, and a bad one comes back as a visible rejection from [`apply_submit`].
fn build_form(groups: &[AdminGroupRow]) -> adminapi::Form {
    let options: Vec<adminapi::FieldOption> = groups
        .iter()
        .enumerate()
        .map(|(i, g)| adminapi::FieldOption {
            value: g.group_id.clone(),
            label: format!("{} ({})", g.name, short_uuid(&g.group_id)),
            checked: i == 0,
        })
        .collect();
    adminapi::Form {
        action: String::new(),
        fields: vec![
            adminapi::Field {
                name: GROUP_FIELD.into(),
                label: "Group".into(),
                value: groups.first().map(|g| g.group_id.clone()).unwrap_or_default(),
                kind: adminapi::FieldKind::Select,
                options,
            },
            adminapi::Field {
                name: PLAYER_FIELD.into(),
                label: "Promote member to admin (player id)".into(),
                value: String::new(),
                kind: adminapi::FieldKind::Text,
                options: Vec::new(),
            },
        ],
        hidden: vec![adminapi::HiddenField {
            name: ACTION_FIELD.into(),
            value: ACTION_PROMOTE.into(),
        }],
        submit: None,
    }
}

// ============================================================================
// Render paths
// ============================================================================

/// The LOCAL editable content: the shared shape plus the in-process submit closure.
async fn admin_content_local(svc: &Arc<Service>, params: &adminapi::Params) -> adminapi::Content {
    let mut content = build_content(svc, params).await;
    if let Some(form) = content.form.as_mut() {
        let closure_svc = svc.clone();
        form.submit = Some(Arc::new(move |values: adminapi::Params| {
            let svc = closure_svc.clone();
            Box::pin(async move { apply_submit(&svc, values).await.map_err(Rejection::into_local) })
        }));
    }
    content
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
        tokio::runtime::Handle::current().block_on(admin_content_local(&svc, &params))
    }))
}

// ============================================================================
// Submit — one authority, two topologies
// ============================================================================

/// Why a submit failed, WITH its class, so the local card and the wire status say the same
/// thing. `adminapi::SubmitError` alone cannot carry the class ([`Rejection::Rejected`] and
/// [`Rejection::Internal`] both collapse into `Other`), and the wire needs the distinction:
/// an operator's bad input is not a server fault.
enum Rejection {
    /// The posted form no longer describes the store: its group is one this page did not
    /// render, or the row it named changed under the write. The remedy is a reload.
    Stale,
    /// The operator's input: 400-class, and the MESSAGE is the point.
    Rejected(String),
    Internal(String),
}

impl Rejection {
    /// LOCAL: `Stale` is the portal's 409 stale-form card; every other verdict rides `Other`
    /// so its message reaches the operator instead of a generic conflict page.
    fn into_local(self) -> adminapi::SubmitError {
        match self {
            Rejection::Stale => adminapi::SubmitError::Conflict,
            Rejection::Rejected(msg) => adminapi::SubmitError::Other(anyhow::anyhow!(msg)),
            Rejection::Internal(msg) => adminapi::SubmitError::Other(anyhow::anyhow!(msg)),
        }
    }

    /// REMOTE: the same three verdicts as typed statuses. NEVER `NotFound` — the edge makes
    /// it indistinguishable from `UnknownMethod`, which would degrade this page to read-only
    /// and hide a real domain error.
    fn into_ops(self) -> opsapi::Error {
        match self {
            Rejection::Stale => opsapi::Error::conflict(
                "groups: the submitted form is stale — reload the page and try again",
            ),
            Rejection::Rejected(msg) => opsapi::Error::invalid(msg),
            Rejection::Internal(msg) => opsapi::Error::internal(msg),
        }
    }
}

fn store_rejection(e: sqlx::Error) -> Rejection {
    Rejection::Internal(format!("groups: {e}"))
}

/// The rendered group. Anything but a uuid means the posted form did not come from a render
/// of THIS page, and the remedy is the same reload a vanished group needs.
fn rendered_group(values: &adminapi::Params) -> Result<&str, Rejection> {
    let group = adminapi::param(values, GROUP_FIELD).trim();
    if !is_uuid_text(group) {
        return Err(Rejection::Stale);
    }
    Ok(group)
}

fn required<'a>(
    values: &'a adminapi::Params,
    field: &str,
    action: &str,
) -> Result<&'a str, Rejection> {
    let v = adminapi::param(values, field).trim();
    if v.is_empty() {
        return Err(Rejection::Rejected(format!(
            "groups: action {action:?} requires a non-empty {field:?}"
        )));
    }
    Ok(v)
}

/// THE submit authority for this page, run by BOTH topologies: the local closure calls it
/// in-process and `AdminSubmit::admin_submit` calls it server-side after the edge hop, so
/// monolith and split apply identical rules.
async fn apply_submit(
    svc: &Service,
    values: adminapi::Params,
) -> Result<adminapi::SubmitOutcome, Rejection> {
    let action = adminapi::param(&values, ACTION_FIELD).trim();
    match action {
        ACTION_PROMOTE => {
            let group_id = rendered_group(&values)?;
            let player_id = required(&values, PLAYER_FIELD, ACTION_PROMOTE)?;
            if !is_uuid_text(player_id) {
                return Err(Rejection::Rejected(format!(
                    "groups: {player_id:?} is not a player uuid"
                )));
            }
            let done = promote(svc, group_id, player_id).await?;
            let verb = if done.already_admin {
                "was already"
            } else {
                "is now"
            };
            let notice = format!(
                "{} {verb} an admin of {} ({})",
                done.player_id, done.group_name, done.group_id
            );
            Ok(adminapi::SubmitOutcome {
                reveal: Vec::new(),
                notice: Some(notice),
            })
        }
        "" => Err(Rejection::Rejected(
            "groups: no action posted — reload the page and try again".into(),
        )),
        other => Err(Rejection::Rejected(format!(
            "groups: unknown action {other:?}"
        ))),
    }
}

/// The write's verdict, carrying the group and player as the DATABASE spells them: the
/// notice names the row that was locked, not the operator's spelling of it.
struct Promotion {
    group_id: String,
    group_name: String,
    player_id: String,
    already_admin: bool,
}

/// The operator's role change, decided under the group's advisory lock exactly like every
/// other write that reads the roster first ([`crate::store::Store::lock_group_tx`]).
///
/// It emits durable `group.role_changed` in the write's OWN transaction: the portal's
/// `admin.action{form-submit}` row records the operator's username, the page slug and the
/// posted field NAMES only (`modules/admin::emit_form_submit`), so nothing there says WHICH
/// group or WHICH player gained admin — and this promotion is what `role_of` answers, the
/// predicate `chat` authorizes channels with.
async fn promote(
    svc: &Service,
    group_id: &str,
    player_id: &str,
) -> Result<Promotion, Rejection> {
    let mut tx = svc.store.pool.begin().await.map_err(store_rejection)?;
    svc.store
        .lock_group_tx(&mut tx, group_id)
        .await
        .map_err(store_rejection)?;
    let (group_name, group_id) = match svc
        .store
        .group_name_tx(&mut tx, group_id)
        .await
        .map_err(store_rejection)?
    {
        Some(row) => row,
        None => {
            tx.rollback().await.map_err(store_rejection)?;
            return Err(Rejection::Stale);
        }
    };
    let (state, role, player_id) = match svc
        .store
        .membership_tx(&mut tx, &group_id, player_id)
        .await
        .map_err(store_rejection)?
    {
        Some(row) => row,
        None => {
            tx.rollback().await.map_err(store_rejection)?;
            return Err(Rejection::Rejected(format!(
                "groups: {player_id} holds no row in this group"
            )));
        }
    };
    if state != STATE_MEMBER {
        tx.rollback().await.map_err(store_rejection)?;
        return Err(Rejection::Rejected(format!(
            "groups: {player_id} holds a {state:?} row, not a member row"
        )));
    }
    if role == ROLE_ADMIN {
        tx.rollback().await.map_err(store_rejection)?;
        return Ok(Promotion {
            group_id,
            group_name,
            player_id,
            already_admin: true,
        });
    }
    // A ROLE change: the new state is the state the predicate requires, so the row's state
    // is written back unchanged while `memberships_role_check`'s equivalence stays true.
    let updated = svc
        .store
        .promote_tx(
            &mut tx,
            &group_id,
            &player_id,
            STATE_MEMBER,
            STATE_MEMBER,
            ROLE_ADMIN,
        )
        .await
        .map_err(store_rejection)?;
    let (group_id, player_id) = match updated {
        Some(row) => row,
        None => {
            tx.rollback().await.map_err(store_rejection)?;
            return Err(Rejection::Stale);
        }
    };
    svc.bus
        .emit_tx(
            AnyTx::new(&mut *tx),
            &groupsevents::ROLE_CHANGED,
            &groupsevents::RoleChanged {
                group_id: group_id.clone(),
                player_id: player_id.clone(),
                role: ROLE_ADMIN.to_string(),
                actor_kind: groupsevents::ACTOR_OPERATOR.to_string(),
                actor_id: String::new(),
            },
        )
        .await
        .map_err(|e| Rejection::Internal(format!("groups: {e}")))?;
    tx.commit().await.map_err(store_rejection)?;
    Ok(Promotion {
        group_id,
        group_name,
        player_id,
        already_admin: false,
    })
}

#[async_trait::async_trait]
impl adminapi::AdminData for Service {
    /// The admin fan-out READ (`admin.adminData`): the same page a local render produces,
    /// with `form.submit == None` — the write is driven remotely through `admin.adminSubmit`.
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

#[async_trait::async_trait]
impl adminapi::AdminSubmit for Service {
    /// The opt-in remote WRITE (`admin.adminSubmit`): runs the SAME [`apply_submit`]
    /// server-side, where the store is local — the submit closure never marshals. `id` is
    /// the page slug (ignored — this Service serves exactly the groups page).
    async fn admin_submit(
        &self,
        _id: String,
        params: adminapi::Params,
    ) -> Result<adminapi::SubmitOutcome, opsapi::Error> {
        apply_submit(self, params)
            .await
            .map_err(Rejection::into_ops)
    }
}

// ============================================================================
// The rendered names for one page's players
// ============================================================================

/// `groups` already REQUIRES `accountsapi::Directory`, so this page resolves handles rather
/// than printing uuids the operator would have to look up elsewhere. Both ways the lookup can
/// fall short degrade to the id, never to an error: a MISS keeps its row with a short uuid,
/// and an outage — this call is a network hop to accounts in a split — sets `degraded`, which
/// [`group_view`] reports on the page itself.
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

// ============================================================================
// Presentation helpers
// ============================================================================

fn group_cell(g: &AdminGroupRow) -> adminapi::Cell {
    adminapi::Cell {
        text: g.name.clone(),
        icon_text: initial(&g.name),
        icon_color_key: palette(&g.group_id),
        link: drill_link(&g.group_id),
        ..Default::default()
    }
}

/// The mockup's player cell: an avatar chip plus the name. `Cell` renders one line, so the
/// mockup's second `#uid` line is the row's own ID column instead.
fn player_cell(names: &Names, player_id: &str) -> adminapi::Cell {
    let label = names.label(player_id);
    adminapi::Cell {
        text: label.clone(),
        icon_text: initial(&label),
        icon_color_key: palette(player_id),
        ..Default::default()
    }
}

/// `join_policy` is the table's own vocabulary, but a row written by a future version must
/// not be asserted as a known class, so anything else takes the neutral chip.
fn policy_cell(policy: &str) -> adminapi::Cell {
    let badge = match policy {
        JOIN_OPEN => "green",
        JOIN_REQUEST => "amber",
        JOIN_INVITE => "blue",
        _ => "grey",
    };
    adminapi::Cell {
        text: policy.into(),
        badge: badge.into(),
        ..Default::default()
    }
}

fn policy_note(policy: &str) -> &'static str {
    match policy {
        JOIN_OPEN => "anyone may join",
        JOIN_REQUEST => "an admin decides each request",
        JOIN_INVITE => "invitation only",
        _ => "unknown policy",
    }
}

fn state_cell(state: &str) -> adminapi::Cell {
    let badge = match state {
        STATE_MEMBER => "green",
        STATE_INVITED => "blue",
        STATE_REQUESTED => "amber",
        _ => "grey",
    };
    adminapi::Cell {
        text: state.into(),
        badge: badge.into(),
        ..Default::default()
    }
}

/// A pending row carries no role, and an empty chip would read as a missing value rather
/// than as the absence the contract defines.
fn role_cell(role: &str) -> adminapi::Cell {
    match role {
        ROLE_ADMIN => adminapi::Cell {
            text: ROLE_ADMIN.into(),
            badge: "red".into(),
            ..Default::default()
        },
        ROLE_MEMBER => adminapi::Cell {
            text: ROLE_MEMBER.into(),
            badge: "grey".into(),
            ..Default::default()
        },
        _ => adminapi::Cell::text("—"),
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
/// characters/inventory/notifications/wallet/friends ON PURPOSE: one entity must draw the
/// same colour on every page of the portal, which a differently-mixed hash would silently
/// break.
fn palette(seed: &str) -> String {
    format!("av-{}", seed.bytes().map(|b| b as usize).sum::<usize>() % 6)
}
