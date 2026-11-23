//! The `config` control-plane provider: the Config & flags page (the Keys table with each key's
//! derived type, value, revision and update time, the Edit form of the selected key and its
//! Revisions timeline). It declares no action yet, so the Edit form draws read-only.

use std::collections::BTreeMap;

use async_trait::async_trait;
use controlplane::order;
use controlplane::{
    Align, Cell, CellKind, Column, ContributionDecl, DataTable, Field, FieldKind, FilterDef, FilterKind, FormSpec,
    InvokeCall, InvokeOutcome, Layout, Link, Manifest, NavEntry, Noun, PageDecl, PageInfo, PanelBody, PanelQuery,
    Point, Primitive, Provider, ReasonReq, Role, Row, SearchHit, SearchQuery, Timeline, TimelineEntry, Tone,
    DEFAULT_PANEL_DEADLINE_MS, PAGE_PARAM_SEARCH,
};
use opsapi::Error;
use sqlx::PgPool;

use crate::store::{self, KeyPage, KeyRow, Selected};
use crate::valid_ident;

const MODULE: &str = "config";
pub(crate) const PAGE: &str = "settings";

pub(crate) const KEYS: &str = "keys";
pub(crate) const EDIT: &str = "edit";
pub(crate) const REVISIONS: &str = "revisions";

/// The page param naming the selected key as `namespace.key`.
pub(crate) const PARAM_KEY: &str = "key";
/// The page param of the Keys table's Type filter.
pub(crate) const PARAM_TYPE: &str = "type";

pub(crate) const TITLE: &str = "Config & flags";
pub(crate) const SUBTITLE: &str = "Typed runtime configuration. Every save creates a revision.";

/// The keys one Keys read returns; the type filter applies to these only.
pub(crate) const MAX_READ: usize = 500;
/// The keys the Keys table draws.
pub(crate) const MAX_ROWS: usize = 100;
pub(crate) const MAX_NEEDLE_BYTES: usize = 256;

/// What the Edit and Revisions panels show with no key to show.
pub(crate) const NO_KEY: &str = "No key selected";

/// The logins the FAKE writer of a revision is drawn from.
pub(crate) const FAKE_LOGINS: [&str; 4] = ["marta.k", "jonas.l", "ola.w", "piotr.n"];

/// A value's type, derived from its stored text: `config.settings` stores no type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ValueType {
    Bool,
    Int,
    Float,
    String,
    Json,
}

impl ValueType {
    pub(crate) const ALL: [ValueType; 5] =
        [ValueType::Bool, ValueType::Int, ValueType::Float, ValueType::String, ValueType::Json];

    pub(crate) fn name(self) -> &'static str {
        match self {
            ValueType::Bool => "bool",
            ValueType::Int => "int",
            ValueType::Float => "float",
            ValueType::String => "string",
            ValueType::Json => "json",
        }
    }

    pub(crate) fn parse(name: &str) -> Option<ValueType> {
        ValueType::ALL.into_iter().find(|t| t.name() == name)
    }

    pub(crate) fn field_kind(self) -> FieldKind {
        match self {
            ValueType::Bool => FieldKind::Bool,
            ValueType::Int => FieldKind::Int,
            ValueType::Float => FieldKind::Float,
            ValueType::String => FieldKind::String,
            ValueType::Json => FieldKind::Json,
        }
    }

    /// The type of the exact stored text `v`: empty or space-padded text is a string;
    /// otherwise its JSON reading decides (a bool, an integer, another number, any other JSON
    /// value), and text that is not JSON is a string.
    pub(crate) fn of(v: &str) -> ValueType {
        if v.is_empty() || v.trim() != v {
            return ValueType::String;
        }
        match serde_json::from_str::<serde_json::Value>(v) {
            Ok(serde_json::Value::Bool(_)) => ValueType::Bool,
            Ok(serde_json::Value::Number(n)) if n.as_i64().is_some() => ValueType::Int,
            Ok(serde_json::Value::Number(_)) => ValueType::Float,
            Ok(_) => ValueType::Json,
            Err(_) => ValueType::String,
        }
    }
}

/// What the page's `key` param selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pick<'a> {
    /// No param: the first key of all.
    First,
    Key(&'a str, &'a str),
    /// A param that can name no key.
    None,
}

pub(crate) fn pick(params: &BTreeMap<String, String>) -> Pick<'_> {
    let Some(raw) = params.get(PARAM_KEY) else {
        return Pick::First;
    };
    match raw.split_once('.') {
        Some((ns, key)) if valid_ident(ns) && valid_ident(key) => Pick::Key(ns, key),
        _ => Pick::None,
    }
}

pub(crate) struct ConfigAdmin {
    pool: PgPool,
}

impl ConfigAdmin {
    pub(crate) fn new(pool: PgPool) -> ConfigAdmin {
        ConfigAdmin { pool }
    }

    async fn keys(&self, req: &PanelQuery) -> Result<PanelBody, Error> {
        let needle = req.search.as_deref().map(str::trim).filter(|s| !s.is_empty());
        if needle.is_some_and(|n| n.len() > MAX_NEEDLE_BYTES) {
            return Err(Error::invalid(format!("{MODULE}: the search is longer than {MAX_NEEDLE_BYTES} bytes")));
        }
        let filter = match req.params.get(PARAM_TYPE).map(String::as_str).filter(|s| !s.is_empty()) {
            None => None,
            Some(name) => Some(
                ValueType::parse(name).ok_or_else(|| Error::invalid(format!("{MODULE}: unknown type {name:?}")))?,
            ),
        };
        let page = store::keys(&self.pool, needle, MAX_READ as i64 + 1).await.map_err(internal)?;
        let selected = match pick(&req.params) {
            Pick::First => page.first.clone(),
            Pick::Key(ns, key) => Some(format!("{ns}.{key}")),
            Pick::None => None,
        };
        Ok(keys_body(&page, filter, needle, selected.as_deref()))
    }

    async fn selected(&self, req: &PanelQuery) -> Result<Option<Selected>, Error> {
        let id = match pick(&req.params) {
            Pick::First => None,
            Pick::Key(ns, key) => Some((ns, key)),
            Pick::None => return Ok(None),
        };
        store::selected(&self.pool, id).await.map_err(internal)
    }
}

fn internal<E: std::fmt::Display>(e: E) -> Error {
    Error::internal(e.to_string())
}

fn cell(kind: CellKind, value: impl Into<String>) -> Cell {
    Cell { kind, value: value.into(), tone: None, link: None, bar: None, more: Vec::new(), row: false, label: None }
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

/// The FAKE writer of the revision `id` names.
pub(crate) fn fake_by(id: &str) -> &'static str {
    // FAKE: config.settings stores no writer.
    FAKE_LOGINS[(controlplane::fake_hash("config.by", id) % FAKE_LOGINS.len() as u64) as usize]
}

/// The FAKE revisions before the current revision `current` of the key `display`, newest
/// first: each one's revision and how many days before the current one it was written. A
/// revision below 1 is dropped.
pub(crate) fn fake_prior(display: &str, current: i64) -> Vec<(i64, usize)> {
    // FAKE: no history is kept; config.settings holds only each key's current value.
    let rev = controlplane::fake_hash("config.rev", display);
    let days = controlplane::fake_hash("config.days", display);
    let first = current - 1 - (rev % 3) as i64;
    let second = first - 1 - (rev / 3 % 3) as i64;
    let first_days = 1 + (days % 7) as usize;
    let second_days = first_days + 1 + (days / 7 % 7) as usize;
    [(first, first_days), (second, second_days)].into_iter().filter(|(r, _)| *r >= 1).collect()
}

fn key_link(display: &str, filter: Option<ValueType>, needle: Option<&str>) -> Link {
    let mut params = BTreeMap::from([(PARAM_KEY.to_string(), display.to_string())]);
    if let Some(needle) = needle {
        params.insert(PAGE_PARAM_SEARCH.to_string(), needle.to_string());
    }
    if let Some(filter) = filter {
        params.insert(PARAM_TYPE.to_string(), filter.name().to_string());
    }
    Link::Page { module: MODULE.to_string(), page: PAGE.to_string(), params }
}

fn key_row(row: &KeyRow, kind: ValueType, link: Link, selected: bool) -> Row {
    let display = row.display();
    let value = if kind == ValueType::Bool {
        cell(CellKind::Toggle, row.value.as_str())
    } else {
        cell(CellKind::Mono, row.value.as_str())
    };
    Row {
        entity: None,
        cells: vec![
            Cell { link: Some(link), ..cell(CellKind::Mono, display.as_str()) },
            cell(CellKind::Ref, kind.name()),
            value,
            cell(CellKind::Number, row.revision.to_string()),
            Cell {
                row: true,
                more: vec![cell(CellKind::User, fake_by(&display))],
                ..cell(CellKind::Dim, row.updated.as_str())
            },
        ],
        highlight: selected,
        item: None,
        actions: Vec::new(),
    }
}

/// The Keys body: the keys of `page` whose type `filter` admits, at most [`MAX_ROWS`], the
/// count of all keys as its meta, and a foot when more keys exist than it draws. `page` holds
/// at most [`MAX_READ`] + 1 rows; the filter sees the first [`MAX_READ`].
pub(crate) fn keys_body(
    page: &KeyPage,
    filter: Option<ValueType>,
    needle: Option<&str>,
    selected: Option<&str>,
) -> PanelBody {
    let read = &page.rows[..page.rows.len().min(MAX_READ)];
    let matched: Vec<(&KeyRow, ValueType)> = read
        .iter()
        .map(|r| (r, ValueType::of(&r.value)))
        .filter(|(_, t)| filter.is_none_or(|f| f == *t))
        .collect();
    let rows = matched
        .iter()
        .take(MAX_ROWS)
        .map(|(r, t)| {
            let display = r.display();
            let link = key_link(&display, filter, needle);
            key_row(r, *t, link, selected == Some(display.as_str()))
        })
        .collect();
    let mut body = PanelBody::new(Primitive::DataTable(DataTable {
        columns: vec![
            column("key", "Key", CellKind::Mono),
            column("type", "Type", CellKind::Ref),
            Column { wrap: true, ..column("value", "Value", CellKind::Mono) },
            Column { align: Align::Right, ..column("rev", "Rev", CellKind::Number) },
            column("updated", "Updated", CellKind::Dim),
        ],
        rows,
        page: PageInfo { next_cursor: None, size: MAX_ROWS as u32, total: None },
        filters: vec![FilterDef {
            key: PARAM_TYPE.to_string(),
            label: "Type".to_string(),
            kind: FilterKind::Select(ValueType::ALL.iter().map(|t| t.name().to_string()).collect()),
        }],
        search_placeholder: Some("Key prefix".to_string()),
        selectable: false,
        list_columns_for: None,
        list_columns_at: 0,
        item_noun: Some(Noun::new("key", "keys")),
    }));
    body.meta = Some(page.total.to_string());
    if matched.len() > MAX_ROWS {
        body.foot = Some(format!("first {MAX_ROWS} keys; narrow with a key prefix"));
    } else if page.rows.len() > MAX_READ && filter.is_some() {
        body.foot = Some(format!("the type filter reads the first {MAX_READ} keys; narrow with a key prefix"));
    }
    body
}

fn no_key() -> PanelBody {
    PanelBody::new(Primitive::Empty { title: NO_KEY.to_string(), detail: String::new() })
}

/// The Edit body: the selected key's value under its type's field, at its revision.
pub(crate) fn edit_body(row: &KeyRow) -> PanelBody {
    let display = row.display();
    let value = Field {
        key: "value".to_string(),
        label: "Value".to_string(),
        kind: ValueType::of(&row.value).field_kind(),
        required: true,
        help: None,
        default: Some(row.value.clone()),
        readonly: false,
        mono: true,
        placeholder: None,
    };
    let mut body = PanelBody::new(Primitive::Form(FormSpec {
        fields: vec![value],
        hidden: BTreeMap::from([(PARAM_KEY.to_string(), display.clone())]),
        submit_label: "Save revision".to_string(),
        note: Some(format!("Compare-and-swap: saves only if still rev {}.", row.revision)),
        revision: u64::try_from(row.revision).ok(),
        title: None,
        modes: Vec::new(),
        reason: ReasonReq::Optional,
        reason_label: Some("Change note".to_string()),
        reason_hint: Some("Why? (recorded in the audit log)".to_string()),
    }));
    body.heading = Some(display);
    body.meta = Some(format!("rev {}", row.revision));
    body
}

fn revision_entry(at: &str, revision: i64, by: &str, value: &str, tone: Option<Tone>) -> TimelineEntry {
    TimelineEntry {
        at: at.to_string(),
        actor: Some(cell(CellKind::User, by)),
        title: None,
        verb: format!("rev {revision} by"),
        target: None,
        summary: String::new(),
        badges: Vec::new(),
        detail: Some(value.to_string()),
        icon: "edit".to_string(),
        tone,
    }
}

/// The Revisions body: the current revision, then its FAKE prior revisions.
pub(crate) fn revisions_body(sel: &Selected) -> PanelBody {
    let row = &sel.row;
    let display = row.display();
    let at = |days: usize| sel.at_days_back.get(days).map_or("", String::as_str);
    let mut entries = vec![revision_entry(at(0), row.revision, fake_by(&display), &row.value, Some(Tone::Accent))];
    for (revision, days) in fake_prior(&display, row.revision) {
        let by = fake_by(&format!("{display}@{revision}"));
        // FAKE: no history is kept, so a prior revision shows the current value.
        entries.push(revision_entry(at(days), revision, by, &row.value, None));
    }
    PanelBody::new(Primitive::Timeline(Timeline {
        entries,
        cursor_before: None,
        live: false,
        days: false,
        date_only: true,
    }))
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

pub(crate) fn manifest() -> Manifest {
    Manifest {
        module: MODULE.to_string(),
        sections: Vec::new(),
        entries: vec![NavEntry {
            section: "platform".to_string(),
            id: MODULE.to_string(),
            label: TITLE.to_string(),
            icon: "sliders".to_string(),
            order: order::platform_nav::CONFIG,
            page: PAGE.to_string(),
            counter: None,
        }],
        pages: vec![PageDecl {
            id: PAGE.to_string(),
            title: TITLE.to_string(),
            subtitle: SUBTITLE.to_string(),
            head_actions: Vec::new(),
            tiles: Vec::new(),
            layout: Layout::Grid21(vec![KEYS.to_string()], vec![EDIT.to_string(), REVISIONS.to_string()]),
            params: vec![PARAM_KEY.to_string(), PARAM_TYPE.to_string()],
        }],
        entity_kinds: Vec::new(),
        contributions: vec![
            contribution(KEYS, 10, "Keys"),
            contribution(EDIT, 20, "Edit"),
            contribution(REVISIONS, 30, "Revisions"),
        ],
        actions: Vec::new(),
        search: Vec::new(),
        writes: false,
    }
}

#[async_trait]
impl Provider for ConfigAdmin {
    fn manifest(&self) -> Manifest {
        manifest()
    }

    async fn panel(&self, req: PanelQuery) -> Result<PanelBody, Error> {
        match req.contribution.as_str() {
            KEYS => self.keys(&req).await,
            EDIT => Ok(self.selected(&req).await?.map_or_else(no_key, |s| edit_body(&s.row))),
            REVISIONS => Ok(self.selected(&req).await?.as_ref().map_or_else(no_key, revisions_body)),
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

#[cfg(test)]
#[path = "admin_tests.rs"]
mod tests;
