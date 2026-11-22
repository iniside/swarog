//! The `friends` control-plane provider: the player entity's Friends tab (every relation the
//! player is party to, both states), the tab's count, the dashboard's "Friends purges pending"
//! attention item, and the `friendship` row kind. Friends has no admin write yet: the row-menu
//! "Remove friend…" is declared `implemented: false` and `writes` is false.

use async_trait::async_trait;
use controlplane::{
    ActionDecl, ActionPoint, Align, Cell, CellKind, Column, ContributionDecl, DataTable, Emphasis, EntityKindDecl,
    EntityRef, InvokeCall, InvokeOutcome, Link, Manifest, Noun, PageInfo, PanelBody, PanelQuery, Point, Primitive,
    Provider, Role, Row, SearchHit, SearchQuery, Then, Tone, DEFAULT_PANEL_DEADLINE_MS,
};
use controlplane::order;
use friendsapi::{DIRECTION_INCOMING, DIRECTION_OUTGOING, STATE_ACCEPTED, STATE_PENDING};
use opsapi::Error;
use std::sync::Arc;

use crate::internal;
use crate::service::{is_cursor_time, is_uuid_text, Service};
use crate::store::{AdminCursor, AdminEdge, ADMIN_PAGE_SIZE};

const MODULE: &str = "friends";
/// The entity kind the tab and its count attach to, and a user cell links; accounts declares it.
const KIND: &str = "player";
/// The row kind of the tab's table, declared here; it has a row menu and no entity page.
pub(crate) const FRIENDSHIP_KIND: &str = "friendship";
const TAB: &str = "friends";

pub(crate) const FRIENDS: &str = "friends";
pub(crate) const TAB_COUNT: &str = "tab.count";
pub(crate) const ATTENTION_PURGES: &str = "attention.purges";

const FRIENDSHIP_REMOVE: &str = "friendship.remove";

pub(crate) struct FriendsAdmin {
    svc: Arc<Service>,
}

impl FriendsAdmin {
    pub(crate) fn new(svc: Arc<Service>) -> FriendsAdmin {
        FriendsAdmin { svc }
    }

    /// The Friends tab: one keyset page of the player's relations, every relation's count and
    /// the pending ones'.
    async fn friends(&self, req: &PanelQuery) -> Result<PanelBody, Error> {
        let id = player_id(req)?;
        let after = req.cursor.as_deref().map(parse_cursor).transpose()?;
        let (total, pending, _) =
            self.svc.store.admin_counts(id, STATE_PENDING, STATE_ACCEPTED).await.map_err(internal)?;
        let mut rows =
            self.svc.store.admin_page(id, after.as_ref(), ADMIN_PAGE_SIZE as i64 + 1).await.map_err(internal)?;
        let next_cursor = if rows.len() > ADMIN_PAGE_SIZE {
            rows.truncate(ADMIN_PAGE_SIZE);
            rows.last().map(|r| cursor(&r.key))
        } else {
            None
        };
        Ok(friends_body(&rows, total, pending, next_cursor))
    }

    /// The tab count: every relation of the player, both states.
    async fn count(&self, req: &PanelQuery) -> Result<i64, Error> {
        let id = player_id(req)?;
        let (total, _, _) =
            self.svc.store.admin_counts(id, STATE_PENDING, STATE_ACCEPTED).await.map_err(internal)?;
        Ok(total)
    }

    /// The purge backlog as the dashboard's attention item.
    async fn purges(&self) -> Result<PanelBody, Error> {
        let (pending, oldest) = self.svc.store.pending_purges().await.map_err(internal)?;
        Ok(purges_body(pending, &oldest))
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

/// The keyset cursor a page hands out: its last row's `<state>|<created_at>|<edge id>`.
pub(crate) fn cursor(key: &AdminCursor) -> String {
    format!("{}|{}|{}", key.state, key.created_at, key.edge_id)
}

/// The cursor a page handed out, [`cursor`] of its last row; every part is checked here, so
/// nothing malformed reaches the query's casts.
pub(crate) fn parse_cursor(raw: &str) -> Result<AdminCursor, Error> {
    let invalid = || Error::invalid(format!("{MODULE}: the page cursor is malformed"));
    let mut parts = raw.splitn(3, '|');
    let (Some(state), Some(created_at), Some(edge_id)) = (parts.next(), parts.next(), parts.next()) else {
        return Err(invalid());
    };
    if ![STATE_ACCEPTED, STATE_PENDING].contains(&state) || !is_cursor_time(created_at) || !is_uuid_text(edge_id) {
        return Err(invalid());
    }
    Ok(AdminCursor { state: state.to_string(), created_at: created_at.to_string(), edge_id: edge_id.to_string() })
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

fn column(key: &str, label: &str, kind: CellKind) -> Column {
    Column {
        key: key.to_string(),
        label: label.to_string(),
        kind,
        sortable: false,
        align: Align::Left,
        sorted: None,
        wrap: false,
    }
}

/// The tab count: a number cell for every count, 0 included.
pub(crate) fn count_cell(n: i64) -> Cell {
    text(CellKind::Number, n.to_string())
}

/// The State badge: "accepted", or a pending relation's direction as the viewed player sees it.
pub(crate) fn state_cell(state: &str, requester_is_player: bool) -> Cell {
    let (value, tone) = if state == STATE_ACCEPTED {
        (STATE_ACCEPTED.to_string(), Tone::Ok)
    } else {
        let direction = if requester_is_player { DIRECTION_OUTGOING } else { DIRECTION_INCOMING };
        (format!("{state} ({direction})"), Tone::Info)
    };
    Cell { tone: Some(tone), ..text(CellKind::Badge, value) }
}

/// The User cell: the other player's id, linked to its entity page, which the portal draws as
/// its label.
pub(crate) fn user_cell(other_id: &str) -> Cell {
    Cell {
        link: Some(Link::Entity(EntityRef { kind: KIND.to_string(), id: other_id.to_string() })),
        ..text(CellKind::User, other_id)
    }
}

fn friendship_row(e: &AdminEdge) -> Row {
    Row {
        entity: Some(EntityRef { kind: FRIENDSHIP_KIND.to_string(), id: e.key.edge_id.clone() }),
        cells: vec![
            user_cell(&e.other_id),
            state_cell(&e.key.state, e.requester_is_player),
            text(CellKind::Text, e.since.clone()),
        ],
        highlight: false,
        item: None,
        actions: Vec::new(),
    }
}

/// The Friends tab panel: the page's rows in tab order, a keyset pager over `total`, and the
/// meta "N · M pending"; no search and no filters.
pub(crate) fn friends_body(rows: &[AdminEdge], total: i64, pending: i64, next_cursor: Option<String>) -> PanelBody {
    let total = total.max(0) as u64;
    let table = DataTable {
        columns: vec![
            column("user", "User", CellKind::User),
            column("state", "State", CellKind::Badge),
            column("since", "Since", CellKind::Text),
        ],
        rows: rows.iter().map(friendship_row).collect(),
        page: PageInfo {
            next_cursor,
            size: ADMIN_PAGE_SIZE as u32,
            total: Some(total),
        },
        filters: Vec::new(),
        search_placeholder: None,
        selectable: false,
        list_columns_for: None,
        list_columns_at: 0,
        item_noun: Some(Noun::new("friend", "friends")),
    };
    PanelBody {
        meta: Some(format!("{total} · {pending} pending")),
        ..PanelBody::new(Primitive::DataTable(table))
    }
}

/// The "Friends purges pending" attention item over friends' purge sweep backlog.
pub(crate) fn purges_body(pending: i64, oldest: &str) -> PanelBody {
    controlplane::purge_backlog_body("Friends purges pending", pending, oldest, crate::projection::PURGE_SWEEP_SUB.id)
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
        tab_label: "Friends".to_string(),
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
            kind: FRIENDSHIP_KIND.to_string(),
            label: "Friendship".to_string(),
            icon: "users".to_string(),
        }],
        contributions: vec![
            contribution(FRIENDS, tab, order::player_tab::FRIENDS, "Friends"),
            contribution(TAB_COUNT, tab_count, 0, "Friends"),
            contribution(
                ATTENTION_PURGES,
                Point::DashboardAttention,
                order::dashboard_attention::FRIENDS_PURGES,
                "Friends purges pending",
            ),
        ],
        // No admin removal exists yet: drawn disabled, never invoked.
        actions: vec![ActionDecl {
            id: FRIENDSHIP_REMOVE.to_string(),
            point: ActionPoint::RowAction(FRIENDSHIP_KIND.to_string()),
            label: "Remove friend…".to_string(),
            icon: "trash".to_string(),
            order: 0,
            danger: true,
            multi: false,
            min_role: Role::Operator,
            then: Then::Invoke,
            implemented: false,
            emphasis: Emphasis::Default,
            trailing_icon: false,
            icon_only: false,
            when_empty: false,
            prepare: false,
        }],
        search: Vec::new(),
        writes: false,
    }
}

#[async_trait]
impl Provider for FriendsAdmin {
    fn manifest(&self) -> Manifest {
        manifest()
    }

    async fn panel(&self, req: PanelQuery) -> Result<PanelBody, Error> {
        match req.contribution.as_str() {
            FRIENDS => self.friends(&req).await,
            TAB_COUNT => {
                let n = self.count(&req).await?;
                Ok(PanelBody::new(Primitive::Cell(count_cell(n))))
            }
            ATTENTION_PURGES => self.purges().await,
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
