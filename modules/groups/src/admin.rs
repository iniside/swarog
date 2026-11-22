//! The `groups` control-plane provider: the player entity's Groups tab (every membership row
//! the player holds, all states), the tab's count, the dashboard's "Groups purges pending"
//! attention item, and the `group` entity kind with its header. Read-only for now:
//! `writes` is false.

use async_trait::async_trait;
use controlplane::order;
use controlplane::{
    Align, Cell, CellKind, Column, ContributionDecl, DataTable, EntityHeader, EntityKindDecl, EntityRef, InvokeCall,
    InvokeOutcome, Link, Manifest, Noun, PageInfo, PanelBody, PanelQuery, Point, Primitive, Provider, Role, Row,
    SearchHit, SearchQuery, DEFAULT_PANEL_DEADLINE_MS,
};
use groupsapi::STATE_MEMBER;
use opsapi::Error;
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
fn membership_row(m: &AdminMembershipRow) -> Row {
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
        actions: Vec::new(),
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
        actions: Vec::new(),
        search: Vec::new(),
        writes: false,
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
        Err(Error::not_found(format!("{MODULE} has no action {:?}", req.action)))
    }
}
