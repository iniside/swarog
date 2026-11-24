//! The `audit` control-plane provider: the Audit log page (`/admin/p/audit/log`), one
//! untitled table over every `audit.log` row, newest first, narrowed by topic, actor and a
//! target-or-detail search, paged on `(at, id)`; and the dashboard's "Recent operator
//! activity" timeline of the newest `admin.action` rows, whose tool links to the page.
//! Nothing here writes: the module declares no writes and the dashboard tool is a link. It reads only schema `audit` (through [`crate::store`]),
//! never another module's tables.

use std::collections::BTreeMap;

use async_trait::async_trait;
use controlplane::order;
use controlplane::{
    ActionDecl, ActionPoint, Align, Cell, CellKind, Column, ContributionDecl, DataTable, Emphasis, FilterDef,
    FilterKind, InvokeCall, InvokeOutcome, Layout, Link, Manifest, NavEntry, Noun, PageDecl, PageInfo, PanelBody,
    PanelQuery, Point, Primitive, Provider, Role, Row, SearchHit, SearchQuery, SortDir, Then, Timeline,
    TimelineEntry, Tone, DEFAULT_PANEL_DEADLINE_MS,
};
use opsapi::Error;
use sqlx::PgPool;

use crate::store::{self, ActivityRow, LogCursor, LogFilter, LogPage, LogRow, Store, MISSING};

const MODULE: &str = "audit";
pub(crate) const PAGE: &str = "log";

/// The page's one table.
pub(crate) const LOG: &str = "log";

/// The dashboard's "Recent operator activity" panel and its "Audit log →" tool.
pub(crate) const ACTIVITY: &str = "activity";
pub(crate) const ACTIVITY_TITLE: &str = "Recent operator activity";
pub(crate) const LOG_OPEN: &str = "log.open";

/// The `admin.action` detail prefixes of an invoke that did not succeed
/// (`core/admin/src/engine/actions.rs` outcome text); such an entry draws in the error tone.
const FAILED_DETAIL_PREFIXES: [&str; 3] = ["failed:", "unknown:", "rejected:"];

pub(crate) const TITLE: &str = "Audit log";
pub(crate) const SUBTITLE: &str = "Append-only record of operator, key and system actions.";
pub(crate) const SEARCH_PLACEHOLDER: &str = "Target or detail contains…";

/// The page params the table's filters travel in; the search travels as `q`.
pub(crate) const PARAM_TOPIC: &str = "topic";
pub(crate) const PARAM_ACTOR: &str = "actor";
/// The Time chip's param. It narrows nothing: the table lists every retained row.
pub(crate) const PARAM_TIME: &str = "time";

pub(crate) const MAX_NEEDLE_BYTES: usize = 256;
pub(crate) const MAX_ACTOR_BYTES: usize = 256;
/// The largest `at` a cursor may carry, in microseconds: far past any real row.
const MAX_CURSOR_MICROS: i64 = 1 << 53;

pub(crate) struct AuditAdmin {
    store: Store,
}

impl AuditAdmin {
    pub(crate) fn new(pool: PgPool) -> AuditAdmin {
        AuditAdmin { store: Store::new(pool) }
    }

    async fn log(&self, req: &PanelQuery) -> Result<PanelBody, Error> {
        let search = req.search.as_deref().map(str::trim).filter(|s| !s.is_empty());
        if search.is_some_and(|s| s.len() > MAX_NEEDLE_BYTES) {
            return Err(Error::invalid(format!("{MODULE}: the search is longer than {MAX_NEEDLE_BYTES} bytes")));
        }
        let topic = param(req, PARAM_TOPIC);
        if let Some(t) = topic {
            if !store::topic_options().contains(&t) {
                return Err(Error::invalid(format!("{MODULE}: unknown topic {t:?}")));
            }
        }
        let actor = param(req, PARAM_ACTOR);
        if actor.is_some_and(|a| a.len() > MAX_ACTOR_BYTES) {
            return Err(Error::invalid(format!("{MODULE}: the actor filter is longer than {MAX_ACTOR_BYTES} bytes")));
        }
        let after = req.cursor.as_deref().map(parse_cursor).transpose()?;
        let filter = LogFilter { topic, actor, search };
        let page = self.store.log_page(&filter, after, store::PAGE_SIZE).await.map_err(internal)?;
        let total = self.store.log_total(&filter).await.map_err(internal)?;
        let actors = self.store.actor_options().await.map_err(internal)?;
        Ok(log_body(page, total, actors))
    }

    /// The newest operator rows whatever the dashboard's range: the panel shows the latest
    /// activity, not a window.
    async fn activity(&self) -> Result<PanelBody, Error> {
        let rows = self.store.recent_operator_actions(store::ACTIVITY_LIMIT).await.map_err(internal)?;
        Ok(activity_body(&rows))
    }
}

fn internal<E: std::fmt::Display>(e: E) -> Error {
    Error::internal(e.to_string())
}

/// A filter param as the store takes it: an empty value (the select's "all") narrows nothing.
fn param<'a>(req: &'a PanelQuery, key: &str) -> Option<&'a str> {
    req.params.get(key).map(String::as_str).filter(|v| !v.is_empty())
}

/// A row's keyset position as the pager carries it: `<at_us>.<id>`.
pub(crate) fn cursor_text(c: LogCursor) -> String {
    format!("{}.{}", c.at_us, c.id)
}

pub(crate) fn parse_cursor(raw: &str) -> Result<LogCursor, Error> {
    let invalid = || Error::invalid(format!("{MODULE}: the page cursor is malformed"));
    let (at, id) = raw.split_once('.').ok_or_else(invalid)?;
    let digits = |s: &str| !s.is_empty() && s.len() <= 18 && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(at) || !digits(id) {
        return Err(invalid());
    }
    let at_us: i64 = at.parse().map_err(|_| invalid())?;
    let id: i64 = id.parse().map_err(|_| invalid())?;
    if at_us > MAX_CURSOR_MICROS {
        return Err(invalid());
    }
    Ok(LogCursor { at_us, id })
}

fn cell(kind: CellKind, value: impl Into<String>) -> Cell {
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

fn select(key: &str, label: &str, options: Vec<String>) -> FilterDef {
    FilterDef {
        key: key.to_string(),
        label: label.to_string(),
        kind: FilterKind::Select(options),
    }
}

/// The page narrowed to one actor: where an operator's user cell leads.
pub(crate) fn actor_link(login: &str) -> Link {
    Link::Page {
        module: MODULE.to_string(),
        page: PAGE.to_string(),
        params: BTreeMap::from([(PARAM_ACTOR.to_string(), login.to_string())]),
    }
}

/// An operator login is a user cell linked to the page narrowed to that login; every
/// other actor (`system`, or a missing login) is a tag.
fn actor_cell(row: &LogRow) -> Cell {
    if row.is_operator() {
        operator_cell(&row.actor)
    } else {
        cell(CellKind::Ref, row.actor.as_str())
    }
}

/// The actor of an `admin.action` row: a user cell linked to the page narrowed to that
/// login, or a tag when the login is missing.
fn operator_cell(login: &str) -> Cell {
    if login == MISSING {
        return cell(CellKind::Ref, login);
    }
    Cell {
        link: Some(actor_link(login)),
        ..cell(CellKind::User, login)
    }
}

/// The dashboard timeline: one entry per operator row, newest first, meta "audit".
pub(crate) fn activity_body(rows: &[ActivityRow]) -> PanelBody {
    PanelBody {
        meta: Some(MODULE.to_string()),
        ..PanelBody::new(Primitive::Timeline(Timeline {
            entries: rows.iter().map(activity_entry).collect(),
            cursor_before: None,
            live: false,
            days: false,
            date_only: false,
        }))
    }
}

/// The target is a free string (`session`, `ip:…`, `kind:id`), not an entity, so it
/// travels in the plain-text meta line before the detail instead of as a linked target.
fn activity_entry(row: &ActivityRow) -> TimelineEntry {
    let detail = if row.target == MISSING {
        row.detail.clone()
    } else {
        format!("{} · {}", row.target, row.detail)
    };
    TimelineEntry {
        at: row.at.clone(),
        actor: Some(operator_cell(&row.actor)),
        title: None,
        verb: row.action.clone(),
        target: None,
        summary: String::new(),
        badges: Vec::new(),
        detail: Some(detail),
        icon: activity_icon(&row.action).to_string(),
        tone: activity_tone(row),
    }
}

/// The icon of an operator action, by the module its id starts with (`{module}.{action}`)
/// or, for the portal's own login events, the operator.
pub(crate) fn activity_icon(action: &str) -> &'static str {
    if action.starts_with("wallet.") {
        "coins"
    } else if action.starts_with("core.events.") {
        "zap"
    } else if action.starts_with("login-") || action == "logout" {
        "user"
    } else {
        "activity"
    }
}

/// The error tone for an invoke that did not succeed and for a locked login.
pub(crate) fn activity_tone(row: &ActivityRow) -> Option<Tone> {
    let failed = FAILED_DETAIL_PREFIXES.iter().any(|p| row.detail.starts_with(p));
    (failed || row.action == "login-locked").then_some(Tone::Err)
}

fn log_row(row: &LogRow) -> Row {
    Row {
        entity: None,
        cells: vec![
            cell(CellKind::Mono, row.time.as_str()),
            actor_cell(row),
            cell(CellKind::Ref, row.topic.as_str()),
            cell(CellKind::Mono, row.action.as_str()),
            cell(CellKind::Mono, row.target.as_str()),
            cell(CellKind::Dim, row.detail.as_str()),
        ],
        highlight: false,
        item: None,
        actions: Vec::new(),
    }
}

/// The Audit log table: one page of rows, the exact total of the narrowed set and the
/// toolbar's filters.
pub(crate) fn log_body(page: LogPage, total: i64, actors: Vec<String>) -> PanelBody {
    PanelBody::new(Primitive::DataTable(DataTable {
        columns: vec![
            Column {
                sorted: Some(SortDir::Desc),
                ..column("time", "Time", CellKind::Mono)
            },
            column("actor", "Actor", CellKind::User),
            column("topic", "Topic", CellKind::Ref),
            column("action", "Action", CellKind::Mono),
            column("target", "Target", CellKind::Mono),
            Column {
                wrap: true,
                ..column("detail", "Detail", CellKind::Dim)
            },
        ],
        rows: page.rows.iter().map(log_row).collect(),
        page: PageInfo {
            next_cursor: page.next.map(cursor_text),
            size: store::PAGE_SIZE as u32,
            total: Some(total.max(0) as u64),
        },
        filters: vec![
            select(PARAM_TOPIC, "Topic", store::topic_options().iter().map(|t| t.to_string()).collect()),
            select(PARAM_ACTOR, "Actor", actors),
            FilterDef {
                key: PARAM_TIME.to_string(),
                label: "Time".to_string(),
                kind: FilterKind::Range,
            },
        ],
        search_placeholder: Some(SEARCH_PLACEHOLDER.to_string()),
        selectable: false,
        list_columns_for: None,
        list_columns_at: 0,
        item_noun: Some(Noun::new("entry", "entries")),
    }))
}

pub(crate) fn manifest() -> Manifest {
    Manifest {
        module: MODULE.to_string(),
        sections: Vec::new(),
        entries: vec![NavEntry {
            section: "platform".to_string(),
            id: MODULE.to_string(),
            label: TITLE.to_string(),
            icon: "list".to_string(),
            order: order::platform_nav::AUDIT,
            page: PAGE.to_string(),
            counter: None,
        }],
        pages: vec![PageDecl {
            id: PAGE.to_string(),
            title: TITLE.to_string(),
            subtitle: SUBTITLE.to_string(),
            head_actions: Vec::new(),
            tiles: Vec::new(),
            layout: Layout::Stack(vec![LOG.to_string()]),
            params: vec![PARAM_ACTOR.to_string(), PARAM_TOPIC.to_string(), PARAM_TIME.to_string()],
        }],
        entity_kinds: Vec::new(),
        contributions: vec![
            // The table has no head: an empty title draws no panel head.
            ContributionDecl {
                id: LOG.to_string(),
                point: Point::Page(PAGE.to_string()),
                order: 10,
                title: String::new(),
                icon: String::new(),
                min_role: Role::Viewer,
                cache_ttl_s: None,
                deadline_ms: DEFAULT_PANEL_DEADLINE_MS,
                readonly: None,
            },
            ContributionDecl {
                id: ACTIVITY.to_string(),
                point: Point::DashboardPanel,
                order: order::dashboard_panel::AUDIT,
                title: ACTIVITY_TITLE.to_string(),
                icon: String::new(),
                min_role: Role::Viewer,
                cache_ttl_s: None,
                deadline_ms: DEFAULT_PANEL_DEADLINE_MS,
                readonly: None,
            },
        ],
        actions: vec![
            ActionDecl {
                id: LOG_OPEN.to_string(),
                point: ActionPoint::PanelTool(ACTIVITY.to_string()),
                label: TITLE.to_string(),
                icon: "right".to_string(),
                order: 10,
                danger: false,
                multi: false,
                min_role: Role::Viewer,
                then: Then::Link(Link::Page {
                    module: MODULE.to_string(),
                    page: PAGE.to_string(),
                    params: BTreeMap::new(),
                }),
                implemented: true,
                emphasis: Emphasis::Ghost,
                trailing_icon: true,
                icon_only: false,
                when_empty: false,
                prepare: false,
            },
        ],
        search: Vec::new(),
        writes: false,
    }
}

#[async_trait]
impl Provider for AuditAdmin {
    fn manifest(&self) -> Manifest {
        manifest()
    }

    async fn panel(&self, req: PanelQuery) -> Result<PanelBody, Error> {
        match req.contribution.as_str() {
            LOG => self.log(&req).await,
            ACTIVITY => self.activity().await,
            other => Err(Error::not_found(format!("{MODULE} has no contribution {other:?}"))),
        }
    }

    async fn search(&self, _req: SearchQuery) -> Result<Vec<SearchHit>, Error> {
        Ok(Vec::new())
    }

    async fn invoke(&self, req: InvokeCall) -> Result<InvokeOutcome, Error> {
        match req.action.as_str() {
            other => Err(Error::not_found(format!("{MODULE} has no action {other:?}"))),
        }
    }
}

#[cfg(test)]
#[path = "admin_tests.rs"]
mod tests;

