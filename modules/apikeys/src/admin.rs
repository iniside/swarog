//! The `apikeys` control-plane provider: the API keys page (the Keys table with its status and
//! role filters, search and keyset pager, and the Roles table with each policy's methods and
//! key count) and the `apikey` search scope.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use controlplane::order;
use controlplane::{
    ActionDecl, ActionPoint, Align, Cell, CellKind, Column, ContributionDecl, DataTable, Emphasis, EntityKindDecl,
    EntityRef, FilterDef, FilterKind, InvokeCall, InvokeOutcome, Layout, Link, Manifest, NavEntry, Noun, PageDecl,
    PageInfo, PanelBody, PanelQuery, Point, Primitive, Provider, Role, Row, SearchDecl, SearchHit, SearchQuery, Then,
    Tone, DEFAULT_PANEL_DEADLINE_MS, DEFAULT_SEARCH_DEADLINE_MS,
};
use opsapi::Error;

use crate::store::{KeyRow, KeyStatus, RoleRow, MAX_NAME_BYTES};
use crate::{internal, Service};

const MODULE: &str = "apikeys";
pub(crate) const PAGE: &str = "keys";
pub(crate) const KEY_KIND: &str = "apikey";
pub(crate) const ROLE_KIND: &str = "apirole";

pub(crate) const KEYS: &str = "keys";
pub(crate) const ROLES: &str = "roles";

pub(crate) const KEY_ROTATE: &str = "key.rotate";
pub(crate) const KEY_REQUESTS: &str = "key.requests";

pub(crate) const FILTER_STATUS: &str = "status";
pub(crate) const FILTER_ROLE: &str = "role";
const ACTIVE: &str = "active";
const REVOKED: &str = "revoked";
/// The policy that admits every method.
const FULL: &str = "full";

pub(crate) const PAGE_SIZE: usize = 25;
/// The longest table search the provider runs; a longer one is refused before any statement.
pub(crate) const MAX_NEEDLE_BYTES: usize = 256;

/// The largest cursor µs `store::keys_page` compares exactly: its
/// `$4 * interval '1 microsecond'` multiplies through float8, exact up to 2^53.
const MAX_CURSOR_MICROS: i64 = 1 << 53;

pub(crate) const ROLES_FOOT: &str = "Policies list the op methods modules serve (e.g. leaderboard.topScores).";

/// The operator logins the FAKE "created by" cell draws from.
pub(crate) const FAKE_CREATORS: [&str; 4] = ["marta.k", "jonas.l", "priya.n", "sam.o"];
/// The FAKE "last used" values after "now".
pub(crate) const FAKE_LAST_USED: [&str; 4] = ["4 min ago", "2 h ago", "9 h ago", "1 d ago"];

pub(crate) struct ApiKeysAdmin {
    svc: Arc<Service>,
}

impl ApiKeysAdmin {
    pub(crate) fn new(svc: Arc<Service>) -> ApiKeysAdmin {
        ApiKeysAdmin { svc }
    }

    async fn keys(&self, req: &PanelQuery) -> Result<PanelBody, Error> {
        let needle = req.search.as_deref().map(str::trim).filter(|s| !s.is_empty());
        if needle.is_some_and(|n| n.len() > MAX_NEEDLE_BYTES) {
            return Err(Error::invalid(format!("{MODULE}: the search is longer than {MAX_NEEDLE_BYTES} bytes")));
        }
        let status = match filter_value(req, FILTER_STATUS) {
            None => None,
            Some(ACTIVE) => Some(KeyStatus::Active),
            Some(REVOKED) => Some(KeyStatus::Revoked),
            Some(other) => return Err(Error::invalid(format!("{MODULE}: unknown status {other:?}"))),
        };
        let roles = self.svc.store.roles_with_counts().await.map_err(internal)?;
        let role_names: Vec<String> = roles.into_iter().map(|r| r.name).collect();
        let role = filter_value(req, FILTER_ROLE);
        if let Some(r) = role.filter(|r| !role_names.iter().any(|n| n == r)) {
            return Err(Error::invalid(format!("{MODULE}: unknown role {r:?}")));
        }
        let after = req.cursor.as_deref().map(parse_cursor).transpose()?;
        let rows = self
            .svc
            .store
            .keys_page(needle, status, role, after, PAGE_SIZE as i64 + 1)
            .await
            .map_err(internal)?;
        let (total, active) = self.svc.store.key_counts().await.map_err(internal)?;
        let narrowed = needle.is_some() || status.is_some() || role.is_some();
        Ok(keys_body(rows, role_names, (!narrowed).then_some(total), active))
    }

    async fn roles(&self) -> Result<PanelBody, Error> {
        let rows = self.svc.store.roles_with_counts().await.map_err(internal)?;
        Ok(roles_body(&rows))
    }
}

/// A filter value that narrows: absent, empty and `"any"` do not.
fn filter_value<'a>(req: &'a PanelQuery, key: &str) -> Option<&'a str> {
    req.params.get(key).map(String::as_str).filter(|v| !v.is_empty() && *v != "any")
}

/// `"<created_at µs>.<name>"` of the last row shown.
pub(crate) fn cursor_of(row: &KeyRow) -> String {
    format!("{}.{}", row.created_us, row.name)
}

/// A hand-edited cursor is `Invalid` here, never a database error from the page query.
pub(crate) fn parse_cursor(raw: &str) -> Result<(i64, &str), Error> {
    let invalid = || Error::invalid(format!("{MODULE}: the page cursor is malformed"));
    let (micros, name) = raw.split_once('.').ok_or_else(invalid)?;
    if micros.is_empty() || !micros.bytes().all(|b| b.is_ascii_digit()) || name.is_empty() || name.len() > MAX_NAME_BYTES
    {
        return Err(invalid());
    }
    let micros: i64 = micros.parse().map_err(|_| invalid())?;
    if micros > MAX_CURSOR_MICROS {
        return Err(invalid());
    }
    Ok((micros, name))
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

fn entity(kind: &str, id: &str) -> EntityRef {
    EntityRef {
        kind: kind.to_string(),
        id: id.to_string(),
    }
}

fn keys_page_link() -> Link {
    Link::Page {
        module: MODULE.to_string(),
        page: PAGE.to_string(),
        params: BTreeMap::new(),
    }
}

fn status_text(revoked: bool) -> &'static str {
    if revoked {
        REVOKED
    } else {
        ACTIVE
    }
}

/// The FAKE operator a key was created by.
pub(crate) fn fake_creator(name: &str) -> &'static str {
    // FAKE: keys record no creator.
    FAKE_CREATORS[(controlplane::fake_hash("apikeys.created_by", name) % FAKE_CREATORS.len() as u64) as usize]
}

/// The FAKE last-used cell: a "now" badge or a dim relative time.
pub(crate) fn fake_last_used(name: &str) -> Cell {
    // FAKE: the gateway records no key use.
    let pick = (controlplane::fake_hash("apikeys.last_used", name) % (FAKE_LAST_USED.len() as u64 + 1)) as usize;
    match pick {
        0 => Cell { tone: Some(Tone::Ok), ..cell(CellKind::Badge, "now") },
        n => cell(CellKind::Dim, FAKE_LAST_USED[n - 1]),
    }
}

fn key_row(row: &KeyRow) -> Row {
    let created = Cell {
        row: true,
        more: vec![cell(CellKind::User, fake_creator(&row.name))],
        ..cell(CellKind::Dim, row.created.clone())
    };
    Row {
        entity: Some(entity(KEY_KIND, &row.name)),
        cells: vec![
            cell(CellKind::Strong, row.name.clone()),
            cell(CellKind::MonoMuted, format!("{}…", row.prefix)),
            cell(CellKind::Ref, row.role.clone()),
            created,
            fake_last_used(&row.name),
            cell(CellKind::Badge, status_text(row.revoked)),
        ],
        highlight: false,
        item: None,
        actions: Vec::new(),
    }
}

/// The meta of the Keys panel over every key, whatever the table is narrowed to.
pub(crate) fn keys_meta(active: i64) -> String {
    format!("{active} {ACTIVE}")
}

/// The Keys table from up to `PAGE_SIZE + 1` rows: the surplus row only decides that a next
/// page exists, and the cursor is then the last drawn row. `total` is `None` when narrowed.
pub(crate) fn keys_body(mut rows: Vec<KeyRow>, role_names: Vec<String>, total: Option<i64>, active: i64) -> PanelBody {
    let next_cursor = if rows.len() > PAGE_SIZE {
        rows.truncate(PAGE_SIZE);
        rows.last().map(cursor_of)
    } else {
        None
    };
    let table = DataTable {
        columns: vec![
            column("name", "Name", CellKind::Strong),
            column("key", "Key", CellKind::MonoMuted),
            column("role", "Role", CellKind::Ref),
            column("created", "Created", CellKind::Dim),
            column("last_used", "Last used", CellKind::Dim),
            column("status", "Status", CellKind::Badge),
        ],
        rows: rows.iter().map(key_row).collect(),
        page: PageInfo {
            next_cursor,
            size: PAGE_SIZE as u32,
            total: total.map(|t| t.max(0) as u64),
        },
        filters: vec![
            select(FILTER_STATUS, "Status", vec![ACTIVE.to_string(), REVOKED.to_string()]),
            select(FILTER_ROLE, "Role", role_names),
        ],
        search_placeholder: Some("Name or key prefix".to_string()),
        selectable: false,
        list_columns_for: None,
        list_columns_at: 0,
        item_noun: Some(Noun::new("key", "keys")),
    };
    PanelBody {
        meta: Some(keys_meta(active)),
        ..PanelBody::new(Primitive::DataTable(table))
    }
}

/// A policy as one cell: one tag per method, or the one tag `full`.
pub(crate) fn scopes_cell(policy: &str) -> Cell {
    let mut tags = if policy.trim() == FULL {
        vec![cell(CellKind::Ref, FULL)].into_iter()
    } else {
        policy
            .split(',')
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(|m| cell(CellKind::Ref, m))
            .collect::<Vec<_>>()
            .into_iter()
    };
    match tags.next() {
        Some(first) => Cell { more: tags.collect(), ..first },
        None => cell(CellKind::Muted, "—"),
    }
}

fn role_row(row: &RoleRow) -> Row {
    Row {
        entity: Some(entity(ROLE_KIND, &row.name)),
        cells: vec![
            cell(CellKind::Strong, row.name.clone()),
            scopes_cell(&row.policy),
            cell(CellKind::Number, row.keys.to_string()),
        ],
        highlight: false,
        item: None,
        actions: Vec::new(),
    }
}

/// The Roles table: every role, name-ordered, with no pager.
pub(crate) fn roles_body(rows: &[RoleRow]) -> PanelBody {
    let table = DataTable {
        columns: vec![
            column("role", "Role", CellKind::Strong),
            Column { wrap: true, ..column("scopes", "Scopes", CellKind::Ref) },
            Column { align: Align::Right, ..column("keys", "Keys", CellKind::Number) },
        ],
        rows: rows.iter().map(role_row).collect(),
        page: PageInfo {
            next_cursor: None,
            size: 0,
            total: None,
        },
        filters: Vec::new(),
        search_placeholder: None,
        selectable: false,
        list_columns_for: None,
        list_columns_at: 0,
        item_noun: None,
    };
    PanelBody {
        meta: Some(rows.len().to_string()),
        foot: Some(ROLES_FOOT.to_string()),
        ..PanelBody::new(Primitive::DataTable(table))
    }
}

pub(crate) fn search_hit(row: &KeyRow) -> SearchHit {
    SearchHit {
        link: keys_page_link(),
        title: row.name.clone(),
        subtitle: format!("{}… · role {}", row.prefix, row.role),
        badge: Some(status_text(row.revoked).to_string()),
    }
}

fn contribution(id: &str, order: i32, title: &str) -> ContributionDecl {
    ContributionDecl {
        id: id.to_string(),
        point: Point::Page(PAGE.to_string()),
        order,
        title: title.to_string(),
        icon: String::new(),
        min_role: Role::Viewer,
        cache_ttl_s: None,
        deadline_ms: DEFAULT_PANEL_DEADLINE_MS,
        readonly: None,
    }
}

fn key_action(id: &str, label: &str, icon: &str, order: i32, min_role: Role, then: Then, implemented: bool) -> ActionDecl {
    ActionDecl {
        id: id.to_string(),
        point: ActionPoint::RowAction(KEY_KIND.to_string()),
        label: label.to_string(),
        icon: icon.to_string(),
        order,
        danger: false,
        multi: false,
        min_role,
        then,
        implemented,
        emphasis: Emphasis::Default,
        trailing_icon: false,
        icon_only: false,
        when_empty: false,
        prepare: false,
    }
}

pub(crate) fn manifest() -> Manifest {
    let requests = Link::Page {
        module: "audit".to_string(),
        page: "log".to_string(),
        params: BTreeMap::new(),
    };
    Manifest {
        module: MODULE.to_string(),
        sections: Vec::new(),
        entries: vec![NavEntry {
            section: "platform".to_string(),
            id: MODULE.to_string(),
            label: "API keys".to_string(),
            icon: "key".to_string(),
            order: order::platform_nav::APIKEYS,
            page: PAGE.to_string(),
            counter: None,
        }],
        pages: vec![PageDecl {
            id: PAGE.to_string(),
            title: "API keys".to_string(),
            subtitle: "Machine credentials for game servers, CI and integrations.".to_string(),
            head_actions: Vec::new(),
            tiles: Vec::new(),
            layout: Layout::Stack(vec![KEYS.to_string(), ROLES.to_string()]),
            params: vec![FILTER_STATUS.to_string(), FILTER_ROLE.to_string()],
        }],
        entity_kinds: vec![
            EntityKindDecl {
                kind: KEY_KIND.to_string(),
                label: "API key".to_string(),
                icon: "key".to_string(),
            },
            EntityKindDecl {
                kind: ROLE_KIND.to_string(),
                label: "API role".to_string(),
                icon: "shield".to_string(),
            },
        ],
        contributions: vec![contribution(KEYS, 10, "Keys"), contribution(ROLES, 20, "Roles")],
        actions: vec![
            // NEW: no secret rotation exists.
            key_action(KEY_ROTATE, "Rotate secret…", "refresh", 10, Role::Operator, Then::Invoke, false),
            key_action(KEY_REQUESTS, "Requests by this key", "list", 40, Role::Viewer, Then::Link(requests), true),
        ],
        search: vec![SearchDecl {
            kind: KEY_KIND.to_string(),
            label: "API keys".to_string(),
            icon: "key".to_string(),
            deadline_ms: DEFAULT_SEARCH_DEADLINE_MS,
        }],
        writes: true,
    }
}

#[async_trait]
impl Provider for ApiKeysAdmin {
    fn manifest(&self) -> Manifest {
        manifest()
    }

    async fn panel(&self, req: PanelQuery) -> Result<PanelBody, Error> {
        match req.contribution.as_str() {
            KEYS => self.keys(&req).await,
            ROLES => self.roles().await,
            other => Err(Error::not_found(format!("{MODULE} has no contribution {other:?}"))),
        }
    }

    async fn search(&self, req: SearchQuery) -> Result<Vec<SearchHit>, Error> {
        let q = req.q.trim();
        if req.kind != KEY_KIND || q.is_empty() || q.len() > MAX_NEEDLE_BYTES {
            return Ok(Vec::new());
        }
        let limit = i64::from(req.limit.min(PAGE_SIZE as u32));
        let rows = self.svc.store.keys_page(Some(q), None, None, None, limit).await.map_err(internal)?;
        Ok(rows.iter().map(search_hit).collect())
    }

    async fn invoke(&self, req: InvokeCall) -> Result<InvokeOutcome, Error> {
        if req.input.values().any(|v| v.len() > controlplane::MAX_INPUT_VALUE_BYTES) {
            return Err(Error::invalid(format!(
                "an input value is longer than {} bytes",
                controlplane::MAX_INPUT_VALUE_BYTES
            )));
        }
        match req.action.as_str() {
            KEY_ROTATE => Err(Error::forbidden("not implemented yet")),
            other => Err(Error::not_found(format!("{MODULE} has no action {other:?}"))),
        }
    }
}

#[cfg(test)]
#[path = "admin_tests.rs"]
mod tests;
