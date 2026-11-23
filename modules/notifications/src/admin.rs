//! The `notifications` control-plane provider: the player entity's Inbox tab (one keyset page
//! of the player's messages as a timeline, newest first), its tab count and the dashboard's
//! "Notifications purges pending" attention item.

use async_trait::async_trait;
use controlplane::order;
use controlplane::{
    Cell, CellKind, ContributionDecl, InvokeCall, InvokeOutcome, Manifest, PanelBody, PanelQuery, Point, Primitive, Provider, Role,
    SearchHit, SearchQuery, Timeline, TimelineEntry, Tone, DEFAULT_PANEL_DEADLINE_MS,
};
use notificationsapi::Notification;
use opsapi::Error;
use std::sync::Arc;

use crate::internal;
use crate::projection::{KIND_FRIEND_ACCEPTED, KIND_FRIEND_REQUESTED, KIND_WALLET_CREDIT};
use crate::service::{decode_cursor, encode_cursor, is_uuid_text, Service};

const MODULE: &str = "notifications";
/// The entity kind the tab attaches to; accounts declares it.
const KIND: &str = "player";
const TAB: &str = "inbox";
/// Entries per page of the Inbox tab.
pub(crate) const PAGE_SIZE: usize = 25;

pub(crate) const INBOX: &str = "inbox";
pub(crate) const TAB_COUNT: &str = "inbox.count";
pub(crate) const ATTENTION_PURGES: &str = "attention.purges";

/// The kind of an operator-sent message.
pub(crate) const KIND_OPERATOR_MAIL: &str = "operator.mail";

pub(crate) struct NotificationsAdmin {
    svc: Arc<Service>,
}

impl NotificationsAdmin {
    pub(crate) fn new(svc: Arc<Service>) -> NotificationsAdmin {
        NotificationsAdmin { svc }
    }

    /// The Inbox tab: one page older than `before`, newest first, and the meta over every
    /// message the player holds.
    async fn inbox(&self, req: &PanelQuery) -> Result<PanelBody, Error> {
        let id = player_id(req)?;
        let before = decode_cursor(req.before.as_deref().unwrap_or(""))?;
        let before = before.as_ref().map(|(at, id)| (at.as_str(), id.as_str()));
        let (total, unread) = self.svc.store.player_stats(id).await.map_err(internal)?;
        let rows = self.svc.store.page_by_player(id, before, PAGE_SIZE as i64 + 1).await.map_err(internal)?;
        Ok(inbox_body(&rows, total, unread))
    }

    /// The tab count: every message the player holds, read or not.
    async fn count(&self, req: &PanelQuery) -> Result<i64, Error> {
        let id = player_id(req)?;
        let (total, _) = self.svc.store.player_stats(id).await.map_err(internal)?;
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

/// The tab's meta: "{n} · {u} unread" over every message the player holds.
pub(crate) fn meta_text(total: i64, unread: i64) -> String {
    format!("{total} · {unread} unread")
}

/// The entry icon of a message kind.
pub(crate) fn kind_icon(kind: &str) -> &'static str {
    match kind {
        KIND_WALLET_CREDIT => "gift",
        KIND_FRIEND_REQUESTED | KIND_FRIEND_ACCEPTED => "heart",
        KIND_OPERATOR_MAIL => "mail",
        // `account.promoted` and every other kind.
        _ => "bell",
    }
}

/// One message as a timeline entry: the bold title, the body after it, and the kind with the
/// read state under it; an unread message is accent-toned.
pub(crate) fn entry(n: &Notification) -> TimelineEntry {
    let unread = n.read_at.is_empty();
    TimelineEntry {
        at: n.created_at.clone(),
        actor: None,
        title: Some(n.title.clone()),
        verb: String::new(),
        target: None,
        summary: n.body.clone(),
        badges: Vec::new(),
        detail: Some(format!("{} · {}", n.kind, if unread { "unread" } else { "read" })),
        icon: kind_icon(&n.kind).to_string(),
        tone: unread.then_some(Tone::Accent),
    }
}

/// The Inbox tab panel from up to `PAGE_SIZE + 1` rows, newest first: the surplus row only
/// decides that an older page exists, and `cursor_before` is then the last drawn row.
pub(crate) fn inbox_body(rows: &[Notification], total: i64, unread: i64) -> PanelBody {
    let page = &rows[..rows.len().min(PAGE_SIZE)];
    let cursor_before = match page.last() {
        Some(last) if rows.len() > PAGE_SIZE => Some(encode_cursor(&last.created_at, &last.id)),
        _ => None,
    };
    PanelBody {
        meta: Some(meta_text(total, unread)),
        ..PanelBody::new(Primitive::Timeline(Timeline {
            entries: page.iter().map(entry).collect(),
            cursor_before,
            live: false,
            days: true,
            date_only: false,
        }))
    }
}

/// The tab count: a number cell for every count, 0 included.
pub(crate) fn count_cell(n: i64) -> Cell {
    Cell {
        kind: CellKind::Number,
        value: n.to_string(),
        tone: None,
        link: None,
        bar: None,
        more: Vec::new(),
        row: false,
        label: None,
    }
}

/// The "Notifications purges pending" attention item over the purge sweep backlog.
pub(crate) fn purges_body(pending: i64, oldest: &str) -> PanelBody {
    controlplane::purge_backlog_body(
        "Notifications purges pending",
        pending,
        oldest,
        crate::projection::PURGE_SWEEP_SUB.id,
    )
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
        tab_label: "Inbox".to_string(),
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
        entity_kinds: Vec::new(),
        contributions: vec![
            contribution(INBOX, tab, order::player_tab::NOTIFICATIONS, "Inbox"),
            contribution(TAB_COUNT, tab_count, 0, "Inbox"),
            contribution(
                ATTENTION_PURGES,
                Point::DashboardAttention,
                order::dashboard_attention::NOTIFICATIONS_PURGES,
                "Notifications purges pending",
            ),
        ],
        actions: Vec::new(),
        search: Vec::new(),
        writes: false,
    }
}

#[async_trait]
impl Provider for NotificationsAdmin {
    fn manifest(&self) -> Manifest {
        manifest()
    }

    async fn panel(&self, req: PanelQuery) -> Result<PanelBody, Error> {
        match req.contribution.as_str() {
            INBOX => self.inbox(&req).await,
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

    /// The manifest declares no action and `writes` is false, so the portal never invokes one.
    async fn invoke(&self, req: InvokeCall) -> Result<InvokeOutcome, Error> {
        Err(Error::not_found(format!("{MODULE} has no action {:?}", req.action)))
    }
}
