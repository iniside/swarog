//! The `config` control-plane provider: the Config & flags page (the Keys table with each key's
//! derived type, value, revision and update time, the Edit form of the selected key and its
//! Revisions timeline), and its two writes: the Edit form's compare-and-swap save and the
//! "+ Key" create.

use std::collections::BTreeMap;

use async_trait::async_trait;
use controlplane::order;
use controlplane::{
    decode_field, encode_field, ActionDecl, ActionPoint, Align, Cell, CellKind, Column, ContributionDecl, DataTable,
    Emphasis, Field, FieldKind, FilterDef, FilterKind, FormSpec, InvokeCall, InvokeOutcome, Layout, Link, Manifest,
    NavEntry, Noun, PageDecl, PageInfo, PanelBody, PanelQuery, Point, Primitive, Provider, ReasonReq, Role, Row,
    SearchHit, SearchQuery, Then, Timeline, TimelineEntry, Tone, DEFAULT_PANEL_DEADLINE_MS, PAGE_PARAM_SEARCH,
};
use opsapi::Error;
use sqlx::PgPool;

use crate::store::{self, KeyPage, KeyRow, Revision, Selected};
use crate::valid_ident;

const MODULE: &str = "config";
pub(crate) const PAGE: &str = "settings";

pub(crate) const KEYS: &str = "keys";
pub(crate) const EDIT: &str = "edit";
pub(crate) const REVISIONS: &str = "revisions";

pub(crate) const CREATE: &str = "setting.create";
pub(crate) const SAVE: &str = "setting.edit";

/// The form field naming a key as `namespace.key`: the Edit form's hidden field, the create
/// form's Key.
pub(crate) const FIELD_KEY: &str = "key";
pub(crate) const FIELD_TYPE: &str = "type";
pub(crate) const FIELD_VALUE: &str = "value";

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

    /// A compare-and-swap of the value of an existing key at the revision the form was drawn
    /// at. The value keeps the key's current type.
    async fn save(&self, req: &InvokeCall) -> Result<InvokeOutcome, Error> {
        let (ns, key) = match posted_key(&req.input) {
            Ok(id) => id,
            Err(msg) => return Ok(field_error(FIELD_KEY, msg)),
        };
        let expected = req
            .expected_revision
            .and_then(|r| i64::try_from(r).ok())
            .ok_or_else(|| Error::invalid(format!("{MODULE}: saving a value needs the form's revision")))?;
        let current =
            store::selected(&self.pool, Some((ns, key))).await.map_err(internal)?.ok_or_else(|| gone(ns, key))?;
        if current.row.revision != expected {
            return Ok(InvokeOutcome::Conflict { current_revision: current.row.revision.max(0) as u64 });
        }
        let Some(raw) = req.input.get(FIELD_VALUE) else {
            return Ok(field_error(FIELD_VALUE, "required".to_string()));
        };
        let value = match stored_value(ValueType::of(&current.row.value), raw) {
            Ok(value) => value,
            Err(msg) => return Ok(field_error(FIELD_VALUE, msg)),
        };
        swap(&self.pool, ns, key, &value, expected, &req.operator).await
    }

    /// Inserts a new key; an existing key is the key field's error and is left as it is.
    async fn create(&self, req: &InvokeCall) -> Result<InvokeOutcome, Error> {
        let mut errors = BTreeMap::new();
        let id = posted_key(&req.input).map_err(|msg| errors.insert(FIELD_KEY.to_string(), msg)).ok();
        let picked = req.input.get(FIELD_TYPE).map(String::as_str).unwrap_or_default();
        let value = match (ValueType::parse(picked), req.input.get(FIELD_VALUE)) {
            (None, _) => {
                errors.insert(FIELD_TYPE.to_string(), "is not one of the types".to_string());
                None
            }
            (Some(_), None) => {
                errors.insert(FIELD_VALUE.to_string(), "required".to_string());
                None
            }
            (Some(t), Some(raw)) => {
                stored_value(t, raw).map_err(|msg| errors.insert(FIELD_VALUE.to_string(), msg)).ok()
            }
        };
        let (Some((ns, key)), Some(value)) = (id, value) else {
            return Ok(InvokeOutcome::FieldErrors(errors));
        };
        if store::create(&self.pool, ns, key, &value, &req.operator).await.map_err(internal)? {
            Ok(InvokeOutcome::Ok { message: format!("Key {ns}.{key} created"), reveal: None, redirect: None })
        } else {
            Ok(field_error(FIELD_KEY, "already exists".to_string()))
        }
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

/// The write of a save whose revision check passed. The update compares the revision again,
/// so a write that landed in between matches no row: the answer is then the key's revision
/// now, or `Conflict` when the key is gone.
async fn swap(
    pool: &PgPool,
    ns: &str,
    key: &str,
    value: &str,
    expected: i64,
    by: &str,
) -> Result<InvokeOutcome, Error> {
    match store::update(pool, ns, key, value, expected, by).await.map_err(internal)? {
        Some(revision) => Ok(InvokeOutcome::Ok {
            message: format!("Saved {ns}.{key} at rev {revision}"),
            reveal: None,
            redirect: None,
        }),
        None => {
            let now = store::selected(pool, Some((ns, key))).await.map_err(internal)?.ok_or_else(|| gone(ns, key))?;
            Ok(InvokeOutcome::Conflict { current_revision: now.row.revision.max(0) as u64 })
        }
    }
}

fn gone(ns: &str, key: &str) -> Error {
    Error::conflict(format!("{MODULE}: key {ns}.{key} no longer exists"))
}

/// The posted `namespace.key`, split at its one '.', or the key field's error.
fn posted_key(input: &BTreeMap<String, String>) -> Result<(&str, &str), String> {
    let raw = input.get(FIELD_KEY).map(String::as_str).unwrap_or_default();
    match raw.split_once('.') {
        Some((ns, key)) if valid_ident(ns) && valid_ident(key) => Ok((ns, key)),
        _ => Err("must be namespace.key, each of a-z, 0-9 and _".to_string()),
    }
}

/// The text a value of type `t` is stored as, from the posted `raw`, or the value field's
/// error. It must read back (`ValueType::of`) as `t`.
pub(crate) fn stored_value(t: ValueType, raw: &str) -> Result<String, String> {
    let kind = t.field_kind();
    let decoded = decode_field(&kind, raw).map_err(|e| e.reason)?;
    let mut text = encode_field(&kind, &decoded);
    match t {
        // The portal re-encodes a posted float, and f64's Display writes 2.0 as "2", an int.
        ValueType::Float if !text.contains(['.', 'e', 'E']) => text.push_str(".0"),
        ValueType::Json => text = text.trim().to_string(),
        _ => {}
    }
    match ValueType::of(&text) {
        got if got == t => Ok(text),
        got => Err(format!("reads as {}, not {}", got.name(), t.name())),
    }
}

fn field_error(key: &str, msg: String) -> InvokeOutcome {
    InvokeOutcome::FieldErrors(BTreeMap::from([(key.to_string(), msg)]))
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

/// The writer cell of a key's current revision: "—" when the history holds no row for it.
fn writer(by: Option<&str>) -> Cell {
    match by {
        Some(by) => cell(CellKind::User, by),
        None => cell(CellKind::Muted, "—"),
    }
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
                more: vec![writer(row.by.as_deref())],
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
    let kind = ValueType::of(&row.value);
    let value = Field {
        key: FIELD_VALUE.to_string(),
        label: "Value".to_string(),
        kind: kind.field_kind(),
        // A required string field refuses a blank post, and "" is a string value.
        required: kind != ValueType::String,
        help: None,
        default: Some(row.value.clone()),
        readonly: false,
        mono: true,
        placeholder: None,
    };
    let mut body = PanelBody::new(Primitive::Form(FormSpec {
        fields: vec![value],
        hidden: BTreeMap::from([(FIELD_KEY.to_string(), display.clone())]),
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

/// One Revisions entry, read "rev N by <writer>": the timeline draws the actor before the verb,
/// so the text is the summary and the writer the one badge, which draw after it.
fn revision_entry(rev: &Revision, tone: Option<Tone>) -> TimelineEntry {
    let revision = rev.revision;
    let summary = if rev.operation == store::OP_DELETE {
        format!("rev {revision} deleted by")
    } else {
        format!("rev {revision} by")
    };
    TimelineEntry {
        at: rev.at.clone(),
        actor: None,
        title: None,
        verb: String::new(),
        target: None,
        summary,
        badges: vec![cell(CellKind::User, rev.by.as_str())],
        detail: rev.value.clone(),
        icon: "edit".to_string(),
        tone,
    }
}

/// The Revisions body: the selected key's newest revisions, newest first, the newest accented.
pub(crate) fn revisions_body(sel: &Selected) -> PanelBody {
    let entries = sel
        .revisions
        .iter()
        .enumerate()
        .map(|(i, rev)| revision_entry(rev, (i == 0).then_some(Tone::Accent)))
        .collect();
    PanelBody::new(Primitive::Timeline(Timeline {
        entries,
        cursor_before: None,
        live: false,
        days: false,
        date_only: true,
    }))
}

fn field(key: &str, label: &str, kind: FieldKind) -> Field {
    Field {
        key: key.to_string(),
        label: label.to_string(),
        kind,
        required: true,
        help: None,
        default: None,
        readonly: false,
        mono: false,
        placeholder: None,
    }
}

/// The "+ Key" form: the value is posted as text and decoded under the picked type.
pub(crate) fn create_form() -> FormSpec {
    FormSpec {
        fields: vec![
            Field {
                mono: true,
                placeholder: Some("namespace.key".to_string()),
                ..field(FIELD_KEY, "Key", FieldKind::String)
            },
            Field {
                default: Some(ValueType::String.name().to_string()),
                ..field(FIELD_TYPE, "Type", FieldKind::Enum(ValueType::ALL.iter().map(|t| t.name().to_string()).collect()))
            },
            Field { mono: true, ..field(FIELD_VALUE, "Value", FieldKind::Text) },
        ],
        hidden: BTreeMap::new(),
        submit_label: "Create key".to_string(),
        note: None,
        revision: None,
        title: Some("New key".to_string()),
        modes: Vec::new(),
        reason: ReasonReq::Optional,
        reason_label: None,
        reason_hint: None,
    }
}

fn action(id: &str, point: ActionPoint, label: &str, icon: &str, emphasis: Emphasis, then: Then) -> ActionDecl {
    ActionDecl {
        id: id.to_string(),
        point,
        label: label.to_string(),
        icon: icon.to_string(),
        order: 10,
        danger: false,
        multi: false,
        min_role: Role::Operator,
        then,
        implemented: true,
        emphasis,
        trailing_icon: false,
        icon_only: false,
        when_empty: false,
        prepare: false,
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
            head_actions: vec![CREATE.to_string()],
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
        actions: vec![
            action(
                CREATE,
                ActionPoint::PageHead(PAGE.to_string()),
                "Key",
                "plus",
                Emphasis::Primary,
                Then::Form(create_form()),
            ),
            action(SAVE, ActionPoint::PanelForm(EDIT.to_string()), "Save revision", "", Emphasis::Default, Then::Invoke),
        ],
        search: Vec::new(),
        writes: true,
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
        if req.input.values().any(|v| v.len() > controlplane::MAX_INPUT_VALUE_BYTES) {
            return Err(Error::invalid(format!(
                "{MODULE}: an input value is longer than {} bytes",
                controlplane::MAX_INPUT_VALUE_BYTES
            )));
        }
        match req.action.as_str() {
            SAVE => self.save(&req).await,
            CREATE => self.create(&req).await,
            other => Err(Error::not_found(format!("{MODULE} has no action {other:?}"))),
        }
    }
}

#[cfg(test)]
#[path = "admin_tests.rs"]
mod tests;
