//! The `groups` control-plane provider: the player entity's Groups tab (every membership row
//! the player holds, all states), the tab's count, the dashboard's "Groups purges pending"
//! attention item, the `group` entity kind with its header, and one operator write:
//! `membership.promote` makes a member row an admin (`writes` is true).

use async_trait::async_trait;
use controlplane::order;
use controlplane::{
    ActionDecl, ActionPoint, Align, Cell, CellKind, Column, ConfirmSpec, ConfirmTone, ContributionDecl, DataTable,
    Emphasis, EntityHeader, EntityKindDecl, EntityRef, InvokeCall, InvokeOutcome, Link, Manifest, Noun, PageInfo,
    PanelBody, PanelQuery, Point, PrepareCall, Primitive, Provider, ReasonReq, Role, Row, SearchHit, SearchQuery, Then,
    DEFAULT_PANEL_DEADLINE_MS,
};
use bus::AnyTx;
use groupsapi::{ROLE_ADMIN, ROLE_MEMBER, STATE_MEMBER};
use opsapi::Error;
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::internal;
use crate::service::{decode_cursor, encode_cursor, is_uuid_text, Service};
use crate::store::{AdminGroupRow, AdminMembershipRow, ADMIN_PAGE_SIZE};

const MODULE: &str = "groups";
/// The entity kind the tab and its count attach to; accounts declares it.
const KIND: &str = "player";
/// The group entity kind, declared here: the Group cell links its page, whose header this
/// provider draws.
pub(crate) const GROUP_KIND: &str = "group";
const TAB: &str = "groups";

pub(crate) const GROUPS: &str = "groups";
pub(crate) const TAB_COUNT: &str = "tab.count";
pub(crate) const ATTENTION_PURGES: &str = "attention.purges";
pub(crate) const GROUP_HEADER: &str = "group.header";
/// The operator's promote of a member row to admin, on the Groups tab's rows.
pub(crate) const PROMOTE: &str = "membership.promote";

pub(crate) struct GroupsAdmin {
    svc: Arc<Service>,
}

impl GroupsAdmin {
    pub(crate) fn new(svc: Arc<Service>) -> GroupsAdmin {
        GroupsAdmin { svc }
    }

    /// The Groups tab: one keyset page of the player's rows and the count of all of them.
    async fn groups(&self, req: &PanelQuery) -> Result<PanelBody, Error> {
        let id = player_id(req)?;
        let after = decode_cursor(req.cursor.as_deref().unwrap_or(""))?;
        let after = after.as_ref().map(|(at, group)| (at.as_str(), group.as_str()));
        let total = self.svc.store.admin_count(id).await.map_err(internal)?;
        let mut rows = self
            .svc
            .store
            .admin_page(STATE_MEMBER, id, after, ADMIN_PAGE_SIZE as i64 + 1)
            .await
            .map_err(internal)?;
        let next_cursor = if rows.len() > ADMIN_PAGE_SIZE {
            rows.truncate(ADMIN_PAGE_SIZE);
            rows.last().map(|r| encode_cursor(&r.sort_at, &r.group_id))
        } else {
            None
        };
        Ok(groups_body(&rows, total, next_cursor))
    }

    /// The tab count: every row the player holds, all states.
    async fn count(&self, req: &PanelQuery) -> Result<i64, Error> {
        let id = player_id(req)?;
        self.svc.store.admin_count(id).await.map_err(internal)
    }

    /// The purge backlog as the dashboard's attention item.
    async fn purges(&self) -> Result<PanelBody, Error> {
        let (pending, oldest) = self.svc.store.pending_purges().await.map_err(internal)?;
        Ok(purges_body(pending, &oldest))
    }

    /// The group page's header; a missing group, or an id that is not a uuid, is not found.
    async fn header(&self, req: &PanelQuery) -> Result<PanelBody, Error> {
        let id = match &req.entity {
            Some(entity) if entity.kind == GROUP_KIND => &entity.id,
            _ => return Err(Error::invalid(format!("{MODULE}: this panel needs a group"))),
        };
        let group = self.svc.store.admin_group(STATE_MEMBER, id).await.map_err(internal)?;
        let group = group.ok_or_else(|| Error::not_found(format!("{MODULE}: no group {id:?}")))?;
        Ok(PanelBody::new(Primitive::EntityHeader(group_header(&group))))
    }

    /// The player's handle for a message or a modal; the id itself when the directory is
    /// not wired, fails or has no row, so a label never fails the write or the modal.
    async fn player_label(&self, player_id: &str) -> String {
        let Some(directory) = self.svc.directory.get() else {
            return player_id.to_string();
        };
        match directory.players_by_id(vec![player_id.to_string()]).await {
            Ok(rows) => rows
                .into_iter()
                .find(|s| s.player_id.eq_ignore_ascii_case(player_id) && !s.handle.is_empty())
                .map(|s| s.handle)
                .unwrap_or_else(|| player_id.to_string()),
            Err(_) => player_id.to_string(),
        }
    }

    /// The promote modal's spec, naming the player and the group. A missing group is not
    /// found; the player label falls back to the id (`player_label`).
    async fn prepare_promote(&self, req: &PrepareCall) -> Result<Then, Error> {
        let (player, group) = bound(req.entity.as_ref(), req.item.as_deref())?;
        let name = {
            let mut conn = self.svc.store.pool.acquire().await.map_err(internal)?;
            let row = self.svc.store.group_name_tx(&mut conn, group).await.map_err(internal)?;
            row.ok_or_else(|| Error::not_found(format!("{MODULE}: no group {group}")))?.0
        };
        let player = self.player_label(player).await;
        Ok(Then::Confirm(promote_spec(format!("Promote {player} to admin of {name}?"))))
    }

    /// Makes the player's member row in the group an admin row, in one transaction under
    /// the group lock, and emits `group.role_changed` with the operator as the actor. A
    /// repeat finds the admin role and answers NoOp, so no request id is kept.
    async fn promote(&self, req: &InvokeCall) -> Result<InvokeOutcome, Error> {
        let (player, group) = bound(req.entity.as_ref(), req.item.as_deref())?;
        let store = &self.svc.store;
        let mut tx = store.pool.begin().await.map_err(internal)?;
        store.lock_group_tx(&mut tx, group).await.map_err(internal)?;
        let Some((name, group_id)) = store.group_name_tx(&mut tx, group).await.map_err(internal)? else {
            return Err(Error::not_found(format!("{MODULE}: no group {group}")));
        };
        let Some((state, role, player_id)) =
            store.membership_tx(&mut tx, &group_id, player).await.map_err(internal)?
        else {
            return Err(Error::invalid(format!("{MODULE}: {player} holds no row in {name}")));
        };
        if state != STATE_MEMBER {
            return Err(Error::invalid(format!(
                "{MODULE}: {player} holds a {state} row in {name}, not a member row"
            )));
        }
        if role == ROLE_ADMIN {
            tx.rollback().await.map_err(internal)?;
            let player = self.player_label(&player_id).await;
            return Ok(InvokeOutcome::NoOp { message: format!("{player} was already an admin of {name}") });
        }
        // A role change: the predicate's state is written back unchanged, with the role in
        // the same statement (`memberships_role_check`).
        let updated = store
            .promote_tx(&mut tx, &group_id, &player_id, STATE_MEMBER, STATE_MEMBER, ROLE_ADMIN)
            .await
            .map_err(internal)?;
        if updated.is_none() {
            return Err(Error::conflict(format!("{MODULE}: {player}'s row in {name} changed; refresh")));
        }
        self.svc
            .bus
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
            .map_err(internal)?;
        tx.commit().await.map_err(internal)?;
        let player = self.player_label(&player_id).await;
        Ok(InvokeOutcome::Ok { message: format!("{player} is now an admin of {name}"), reveal: None, redirect: None })
    }
}

/// Every input value is at most [`controlplane::MAX_INPUT_VALUE_BYTES`].
fn input_within_cap(input: &BTreeMap<String, String>) -> bool {
    input.values().all(|v| v.len() <= controlplane::MAX_INPUT_VALUE_BYTES)
}

/// A promote's bound element: the entity is `player:<uuid>` and the item the group's
/// uuid, both in the canonical spelling, or the call is invalid before any statement.
fn bound<'a>(entity: Option<&'a EntityRef>, item: Option<&'a str>) -> Result<(&'a str, &'a str), Error> {
    let player = match entity {
        Some(e) if e.kind == KIND && is_uuid_text(&e.id) => e.id.as_str(),
        _ => return Err(Error::invalid(format!("{MODULE}: the action needs a player"))),
    };
    match item {
        Some(group) if is_uuid_text(group) => Ok((player, group)),
        _ => Err(Error::invalid(format!("{MODULE}: the action needs a group"))),
    }
}

/// The promote modal: a Warn confirm (a promote cannot be undone from the portal), an
/// optional reason, no type-to-confirm and no impact lines.
fn promote_spec(body: String) -> ConfirmSpec {
    ConfirmSpec {
        title: "Promote to admin".to_string(),
        body,
        tone: ConfirmTone::Warn,
        impact: Vec::new(),
        reason: ReasonReq::Optional,
        reason_hint: None,
        type_to_confirm: None,
        confirm_label: "Promote".to_string(),
    }
}

/// The entity id of a player panel's request.
fn player_id(req: &PanelQuery) -> Result<&str, Error> {
    match &req.entity {
        Some(entity) if entity.kind == KIND && is_uuid_text(&entity.id) => Ok(&entity.id),
        Some(entity) if entity.kind == KIND => Err(Error::invalid(format!("{MODULE}: malformed id"))),
        _ => Err(Error::invalid(format!("{MODULE}: this panel needs a player"))),
    }
}

fn text(kind: CellKind, value: impl Into<String>) -> Cell {
    Cell {
        kind,
        value: value.into(),
        tone: None,
        link: None,
        bar: None,
        more: Vec::new(),
        row: false,
        label: None,
    }
}

fn column(key: &str, label: &str, kind: CellKind, align: Align) -> Column {
    Column {
        key: key.to_string(),
        label: label.to_string(),
        kind,
        sortable: false,
        align,
        sorted: None,
        wrap: false,
    }
}

/// The tab count: a number cell for every count, 0 included.
pub(crate) fn count_cell(n: i64) -> Cell {
    text(CellKind::Number, n.to_string())
}

/// The Group cell: the group's name, linked to the group's entity page.
pub(crate) fn group_cell(group_id: &str, name: &str) -> Cell {
    Cell {
        link: Some(Link::Entity(EntityRef { kind: GROUP_KIND.to_string(), id: group_id.to_string() })),
        ..text(CellKind::Text, name)
    }
}

/// The Role label: a member row's role (`admin`/`member`), or a pending row's state
/// (`invited`/`requested`), since a pending row has no role.
pub(crate) fn role_cell(state: &str, role: &str) -> Cell {
    let word = if state == STATE_MEMBER { role } else { state };
    text(CellKind::Label, word)
}

/// A tab row: not clickable (no `entity`), so only the name cell links; `item` is the group.
/// Only a member row whose role is member lists the promote.
fn membership_row(m: &AdminMembershipRow) -> Row {
    let promotable = m.state == STATE_MEMBER && m.role == ROLE_MEMBER;
    let actions = if promotable { vec![PROMOTE.to_string()] } else { Vec::new() };
    Row {
        entity: None,
        cells: vec![
            group_cell(&m.group_id, &m.name),
            role_cell(&m.state, &m.role),
            text(CellKind::Number, m.members.to_string()),
            text(CellKind::Text, m.joined.clone()),
        ],
        highlight: false,
        item: Some(m.group_id.clone()),
        actions,
    }
}

/// The Groups tab panel: the page's rows, the meta "N" over every row the player holds,
/// and no total, so the portal draws a foot only when a next page exists.
pub(crate) fn groups_body(rows: &[AdminMembershipRow], total: i64, next_cursor: Option<String>) -> PanelBody {
    let table = DataTable {
        columns: vec![
            column("group", "Group", CellKind::Text, Align::Left),
            column("role", "Role", CellKind::Label, Align::Left),
            column("members", "Members", CellKind::Number, Align::Right),
            column("joined", "Joined", CellKind::Text, Align::Left),
        ],
        rows: rows.iter().map(membership_row).collect(),
        page: PageInfo {
            next_cursor,
            size: ADMIN_PAGE_SIZE as u32,
            total: None,
        },
        filters: Vec::new(),
        search_placeholder: None,
        selectable: false,
        list_columns_for: None,
        list_columns_at: 0,
        item_noun: Some(Noun::new("group", "groups")),
    };
    PanelBody {
        meta: Some(total.max(0).to_string()),
        ..PanelBody::new(Primitive::DataTable(table))
    }
}

/// The group page's header: the name, the join policy and creation day, and the live member
/// and pending counts as labels.
pub(crate) fn group_header(g: &AdminGroupRow) -> EntityHeader {
    let day = g.created_at.get(..10).unwrap_or(&g.created_at);
    let members = if g.members == 1 { "1 member".to_string() } else { format!("{} members", g.members) };
    EntityHeader {
        title: g.name.clone(),
        crumb: None,
        subtitle: format!("{} · created {day}", g.join_policy),
        status: None,
        badges: vec![
            text(CellKind::Label, members),
            text(CellKind::Label, format!("{} pending", g.pending)),
        ],
        avatar_seed: g.group_id.clone(),
    }
}

/// The "Groups purges pending" attention item over groups' purge sweep backlog.
pub(crate) fn purges_body(pending: i64, oldest: &str) -> PanelBody {
    controlplane::purge_backlog_body("Groups purges pending", pending, oldest, crate::projection::PURGE_SWEEP_SUB.id)
}

fn contribution(id: &str, point: Point, order: i32, title: &str) -> ContributionDecl {
    ContributionDecl {
        id: id.to_string(),
        point,
        order,
        title: title.to_string(),
        icon: String::new(),
        min_role: Role::Viewer,
        cache_ttl_s: None,
        deadline_ms: DEFAULT_PANEL_DEADLINE_MS,
        readonly: None,
    }
}

/// `membership.promote` on the Groups tab's rows; the modal's text comes from `prepare`.
fn promote_decl() -> ActionDecl {
    ActionDecl {
        id: PROMOTE.to_string(),
        point: ActionPoint::TableRow(GROUPS.to_string()),
        label: "Promote".to_string(),
        icon: "shield".to_string(),
        order: 0,
        danger: false,
        multi: false,
        min_role: Role::Operator,
        then: Then::Confirm(promote_spec("Promote this player to admin of this group?".to_string())),
        implemented: true,
        emphasis: Emphasis::Ghost,
        trailing_icon: false,
        icon_only: false,
        when_empty: false,
        prepare: true,
    }
}

pub(crate) fn manifest() -> Manifest {
    let tab = Point::EntityTab {
        kind: KIND.to_string(),
        tab: TAB.to_string(),
        tab_label: "Groups".to_string(),
        columns: 1,
        params: Vec::new(),
    };
    let tab_count = Point::EntityTabCount {
        kind: KIND.to_string(),
        tab: TAB.to_string(),
    };
    Manifest {
        module: MODULE.to_string(),
        sections: Vec::new(),
        entries: Vec::new(),
        pages: Vec::new(),
        entity_kinds: vec![EntityKindDecl {
            kind: GROUP_KIND.to_string(),
            label: "Group".to_string(),
            icon: "users".to_string(),
        }],
        contributions: vec![
            contribution(GROUPS, tab, order::player_tab::GROUPS, "Groups"),
            contribution(TAB_COUNT, tab_count, 0, "Groups"),
            contribution(
                ATTENTION_PURGES,
                Point::DashboardAttention,
                order::dashboard_attention::GROUPS_PURGES,
                "Groups purges pending",
            ),
            contribution(GROUP_HEADER, Point::EntityHeader(GROUP_KIND.to_string()), 0, "Group"),
        ],
        actions: vec![promote_decl()],
        search: Vec::new(),
        writes: true,
    }
}

#[async_trait]
impl Provider for GroupsAdmin {
    fn manifest(&self) -> Manifest {
        manifest()
    }

    async fn panel(&self, req: PanelQuery) -> Result<PanelBody, Error> {
        match req.contribution.as_str() {
            GROUPS => self.groups(&req).await,
            TAB_COUNT => {
                let n = self.count(&req).await?;
                Ok(PanelBody::new(Primitive::Cell(count_cell(n))))
            }
            ATTENTION_PURGES => self.purges().await,
            GROUP_HEADER => self.header(&req).await,
            other => Err(Error::not_found(format!("{MODULE} has no contribution {other:?}"))),
        }
    }

    async fn search(&self, _req: SearchQuery) -> Result<Vec<SearchHit>, Error> {
        Ok(Vec::new())
    }

    async fn invoke(&self, req: InvokeCall) -> Result<InvokeOutcome, Error> {
        if !input_within_cap(&req.input) {
            return Err(Error::invalid(format!(
                "an input value is longer than {} bytes",
                controlplane::MAX_INPUT_VALUE_BYTES
            )));
        }
        match req.action.as_str() {
            PROMOTE => self.promote(&req).await,
            other => Err(Error::not_found(format!("{MODULE} has no action {other:?}"))),
        }
    }

    async fn prepare(&self, req: PrepareCall) -> Result<Then, Error> {
        match req.action.as_str() {
            PROMOTE => self.prepare_promote(&req).await,
            other => Err(Error::not_found(format!("{MODULE} prepares no action {other:?}"))),
        }
    }
}
