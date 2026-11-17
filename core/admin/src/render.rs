//! The portal's template layer: the view models the templates render, their conversion from
//! control-plane types, and the embedded templates.

use std::collections::BTreeMap;

use controlplane::{
    decode_field, Align, CellKind, ConfirmSpec, ConfirmTone, EntityRef, Field, FieldKind, FieldValue, FormSpec, Link,
    PanelBody, Point, Primitive, ReasonReq, Severity, ServiceState, SortDir, Tone,
};
use minijinja::{Environment, Value};
use serde::Serialize;

use crate::engine::actions::{self, money_currency_key, Chrome};
use crate::engine::columns::{ColumnOutcome, ListColumn};
use crate::engine::panels::{self, LastValue, PanelState};

const SHELL: &str = include_str!("templates/shell.html.tmpl");
const PRIMITIVES: &str = include_str!("templates/primitives.html.tmpl");
const MODAL: &str = include_str!("templates/modal.html.tmpl");
const LOGIN: &str = include_str!("templates/login.html.tmpl");

/// Compiles the embedded templates; `.html` names turn auto-escaping on.
pub(crate) fn env() -> anyhow::Result<Environment<'static>> {
    let mut env = Environment::new();
    for (name, source) in [
        ("primitives.html", PRIMITIVES),
        ("shell.html", SHELL),
        ("modal.html", MODAL),
        ("login.html", LOGIN),
    ] {
        env.add_template(name, source)
            .map_err(|e| anyhow::anyhow!("admin: template {name} compile: {e}"))?;
    }
    Ok(env)
}

pub(crate) fn page<S: Serialize>(env: &Environment<'_>, template: &str, view: &S) -> Result<String, minijinja::Error> {
    env.get_template(template)?.render(view)
}

pub(crate) fn fragment<S: Serialize>(
    env: &Environment<'_>,
    template: &str,
    name: &str,
    arg: &S,
) -> Result<String, minijinja::Error> {
    let captured = env.get_template(template)?.render_captured(minijinja::context! {})?;
    captured.state().call_macro(name, &[Value::from_serialize(arg)])
}

// ---------------------------------------------------------------------------
// URLs
// ---------------------------------------------------------------------------

/// Percent-encodes everything outside RFC 3986's unreserved set.
pub(crate) fn encode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for b in raw.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub(crate) fn entity_url(entity: &EntityRef) -> String {
    format!("/admin/e/{}/{}", encode(&entity.kind), encode(&entity.id))
}

pub(crate) fn link_url(link: &Link) -> String {
    match link {
        Link::Entity(entity) => entity_url(entity),
        Link::Page { module, page, params } => {
            let mut url = format!("/admin/p/{}/{}", encode(module), encode(page));
            let query: Vec<String> = params.iter().map(|(k, v)| format!("{}={}", encode(k), encode(v))).collect();
            if !query.is_empty() {
                url.push('?');
                url.push_str(&query.join("&"));
            }
            url
        }
    }
}

pub(crate) fn page_url(module: &str, page: &str) -> String {
    format!("/admin/p/{}/{}", encode(module), encode(page))
}

pub(crate) fn entity_param(entity: &EntityRef) -> String {
    format!("{}:{}", entity.kind, entity.id)
}

/// Panel URL query keys of the typed `PanelQuery` fields.
pub(crate) const QUERY_SEARCH: &str = controlplane::PAGE_PARAM_SEARCH;
pub(crate) const QUERY_CURSOR: &str = "cursor";
pub(crate) const QUERY_BEFORE: &str = "before";
pub(crate) const QUERY_STATE: &str = controlplane::PAGE_PARAM_QUEUE_STATE;
/// A modal URL's bound element.
pub(crate) const QUERY_ITEM: &str = "item";

/// `path` with `query` appended, `key` replaced by `value` (dropped when `value` is `None`).
pub(crate) fn with_param(path: &str, query: &[(String, String)], key: &str, value: Option<&str>) -> String {
    let mut parts: Vec<String> = query
        .iter()
        .filter(|(k, _)| k != key)
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect();
    if let Some(v) = value {
        parts.push(format!("{}={}", encode(key), encode(v)));
    }
    if parts.is_empty() {
        path.to_string()
    } else {
        format!("{path}?{}", parts.join("&"))
    }
}

// ---------------------------------------------------------------------------
// Atoms
// ---------------------------------------------------------------------------

/// The avatar palette of the mockup's users (`UILayout/redesign/assets/data.js:4-17`).
const AVATAR_COLORS: [&str; 14] = [
    "#d9488a", "#4f5bd5", "#0f9488", "#c2630f", "#7c3aed", "#2563eb", "#be123c", "#65a30d", "#0891b2", "#9333ea",
    "#dc2626", "#059669", "#64748b", "#ca8a04",
];

fn fnv1a64(raw: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in raw.bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

pub(crate) fn avatar_color(seed: &str) -> &'static str {
    AVATAR_COLORS[(fnv1a64(seed) % AVATAR_COLORS.len() as u64) as usize]
}

/// The mockup's `initials` (`bundle.js:100`).
pub(crate) fn initials(name: &str) -> String {
    let kept: String = name.chars().filter(|c| c.is_ascii_alphabetic() || matches!(c, ' ' | '.' | '_')).collect();
    let out: String = kept
        .split([' ', '.', '_'])
        .filter(|p| !p.is_empty())
        .take(2)
        .filter_map(|p| p.chars().next())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    if out.is_empty() {
        "?".to_string()
    } else {
        out
    }
}

/// The mockup's value-derived badge tone (`bundle.js:98`).
fn status_tone(value: &str) -> &'static str {
    match value {
        "active" | "running" | "ok" | "sent" | "enabled" | "healthy" => "ok",
        "pending" => "info",
        "retrying" | "lagging" | "degraded" | "restarting" | "suspended" => "warn",
        "failed" | "fail" | "paused" | "down" | "crashed" | "banned" => "err",
        _ => "",
    }
}

fn tone(t: Tone) -> &'static str {
    match t {
        Tone::Ok => "ok",
        Tone::Warn => "warn",
        Tone::Err => "err",
        Tone::Info => "info",
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct CellView {
    pub(crate) kind: &'static str,
    pub(crate) value: String,
    pub(crate) tone: &'static str,
    pub(crate) href: Option<String>,
    pub(crate) color: &'static str,
    pub(crate) initials: String,
    /// The bar's fill percentage, at most 100.
    pub(crate) bar: Option<u8>,
    /// The bar fill's `lag-bar` class: `""`, `w` or `e`.
    pub(crate) bar_tone: &'static str,
    /// `Cell::more`, flattened to one level; each has an empty `more`.
    pub(crate) more: Vec<CellView>,
    /// A user cell in a table row that opens an entity: its link sits on the name inside
    /// the cell, not around it.
    pub(crate) inner_link: bool,
}

/// With a bar, the cell's tone colours the fill only: `Warn` is `w`, `Err` is `e`, every
/// other tone the default fill.
fn bar_tone(t: Option<Tone>) -> &'static str {
    match t {
        Some(Tone::Warn) => "w",
        Some(Tone::Err) => "e",
        Some(Tone::Ok | Tone::Info) | None => "",
    }
}

pub(crate) fn cell(c: &controlplane::Cell) -> CellView {
    let mut more = Vec::new();
    let mut pending: Vec<&controlplane::Cell> = c.more.iter().rev().collect();
    while let Some(next) = pending.pop() {
        more.push(one_cell(next));
        pending.extend(next.more.iter().rev());
    }
    CellView { more, ..one_cell(c) }
}

/// `c` without its `more`.
fn one_cell(c: &controlplane::Cell) -> CellView {
    let kind = match c.kind {
        CellKind::Text => "text",
        CellKind::Mono => "mono",
        CellKind::Badge => "badge",
        CellKind::Ref => "ref",
        CellKind::Number => "number",
        CellKind::Bytes => "bytes",
        CellKind::Time => "time",
        CellKind::User => "user",
        CellKind::Muted => "muted",
        CellKind::Label => "label",
    };
    let tone = match (c.bar, c.tone, c.kind) {
        (Some(_), _, _) => "",
        (None, Some(t), _) => tone(t),
        (None, None, CellKind::Badge) => status_tone(&c.value),
        (None, None, _) => "",
    };
    CellView {
        kind,
        value: c.value.clone(),
        tone,
        href: c.link.as_ref().map(link_url),
        color: avatar_color(&c.value),
        initials: initials(&c.value),
        bar: c.bar.map(|pct| pct.min(100)),
        bar_tone: c.bar.map_or("", |_| bar_tone(c.tone)),
        more: Vec::new(),
        inner_link: false,
    }
}

fn ref_cell(e: &EntityRef) -> CellView {
    let user = matches!(e.kind.as_str(), "player" | "user");
    CellView {
        kind: if user { "user" } else { "ref" },
        value: if user { e.id.clone() } else { entity_param(e) },
        tone: "",
        href: Some(entity_url(e)),
        color: avatar_color(&e.id),
        initials: initials(&e.id),
        bar: None,
        bar_tone: "",
        more: Vec::new(),
        inner_link: false,
    }
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

/// Disabled actions carry their reason in `hint`.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ActionItem {
    pub(crate) module: String,
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) icon: String,
    pub(crate) danger: bool,
    /// `primary`, `default` or `ghost`.
    pub(crate) emphasis: &'static str,
    /// The icon follows the label.
    pub(crate) trailing_icon: bool,
    /// Shown on an empty panel.
    pub(crate) when_empty: bool,
    /// An `ActionPoint::QueueRow` action: drawn on the queue items listing it, never in the
    /// panel tools.
    pub(crate) queue_row: bool,
    /// An `ActionPoint::TableRow` action: drawn on the table rows listing it, never in the
    /// panel tools.
    pub(crate) table_row: bool,
    /// `GET` opens the modal, `POST` invokes.
    pub(crate) url: String,
    pub(crate) enabled: bool,
    pub(crate) hint: Option<&'static str>,
    /// `invoke`, `form`, `confirm` or `link`.
    pub(crate) then: &'static str,
    pub(crate) href: Option<String>,
    /// Fields an `invoke` posts without a modal; rendered as `hx-vals` JSON.
    #[serde(serialize_with = "as_json")]
    pub(crate) vals: BTreeMap<String, String>,
}

fn as_json<S: serde::Serializer>(vals: &BTreeMap<String, String>, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&serde_json::to_string(vals).map_err(serde::ser::Error::custom)?)
}

pub(crate) fn then_name(then: &controlplane::Then) -> &'static str {
    match then {
        controlplane::Then::Invoke => "invoke",
        controlplane::Then::Form(_) => "form",
        controlplane::Then::Confirm(_) => "confirm",
        controlplane::Then::Link(_) => "link",
    }
}

pub(crate) fn emphasis_name(emphasis: controlplane::Emphasis) -> &'static str {
    match emphasis {
        controlplane::Emphasis::Primary => "primary",
        controlplane::Emphasis::Default => "default",
        controlplane::Emphasis::Ghost => "ghost",
    }
}

/// `action` bound to an entity and an element: the modal URL carries `?entity=` and
/// `?item=`, a token-less invoke posts `_entity` and `_item`.
pub(crate) fn bind(action: &ActionItem, entity: Option<&EntityRef>, element: Option<&str>) -> ActionItem {
    let mut bound = action.clone();
    let mut query = Vec::new();
    if let Some(e) = entity {
        query.push(("entity".to_string(), entity_param(e)));
        bound.vals.insert(actions::FIELD_ENTITY.to_string(), entity_param(e));
    }
    if let Some(element) = element {
        query.push((QUERY_ITEM.to_string(), element.to_string()));
        bound.vals.insert(actions::FIELD_ITEM.to_string(), element.to_string());
    }
    bound.url = with_param(&action.url, &query, "", None);
    bound
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct MenuGroup {
    pub(crate) module: String,
    pub(crate) items: Vec<ActionItem>,
}

pub(crate) fn menu_groups(items: Vec<ActionItem>) -> Vec<MenuGroup> {
    let mut groups: Vec<MenuGroup> = Vec::new();
    for item in items {
        match groups.iter_mut().find(|g| g.module == item.module) {
            Some(group) => group.items.push(item),
            None => groups.push(MenuGroup { module: item.module.clone(), items: vec![item] }),
        }
    }
    groups
}

// ---------------------------------------------------------------------------
// Shell
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
pub(crate) struct OperatorView {
    pub(crate) login: String,
    pub(crate) role: &'static str,
    pub(crate) initials: String,
    pub(crate) color: &'static str,
}

impl OperatorView {
    pub(crate) fn new(login: &str, role: &'static str) -> OperatorView {
        OperatorView { login: login.to_string(), role, initials: initials(login), color: avatar_color(login) }
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct NavView {
    pub(crate) key: String,
    pub(crate) label: String,
    pub(crate) icon: String,
    pub(crate) href: String,
    pub(crate) module: String,
    pub(crate) down: bool,
    pub(crate) counter_url: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct NavSectionView {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) items: Vec<NavView>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Crumb {
    pub(crate) label: String,
    pub(crate) href: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ScopeView {
    pub(crate) kind: String,
    pub(crate) label: String,
    pub(crate) icon: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct RevealView {
    pub(crate) label: String,
    pub(crate) value: String,
    pub(crate) hint: Option<String>,
    pub(crate) expires_at: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct FlashView {
    pub(crate) message: String,
    pub(crate) reveal: Option<RevealView>,
    /// `ok` or `info`.
    pub(crate) tone: &'static str,
}

pub(crate) fn reveal(r: &controlplane::SecretReveal) -> RevealView {
    RevealView { label: r.label.clone(), value: r.value.clone(), hint: r.hint.clone(), expires_at: r.expires_at.clone() }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct HealthView {
    pub(crate) healthy: usize,
    pub(crate) total: usize,
    pub(crate) unhealthy: Vec<ServiceView>,
    pub(crate) note: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct NotificationView {
    /// `err`, `warn` or `muted`.
    pub(crate) tone: &'static str,
    pub(crate) title: String,
    pub(crate) text: String,
    pub(crate) when: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ShellView {
    pub(crate) operator: OperatorView,
    pub(crate) csrf: String,
    pub(crate) sections: Vec<NavSectionView>,
    /// The `NavView.key` of the current page.
    pub(crate) active: Option<String>,
    pub(crate) crumbs: Vec<Crumb>,
    pub(crate) scopes: Vec<ScopeView>,
    pub(crate) flash: Option<FlashView>,
    pub(crate) deployment: Option<String>,
    pub(crate) health: Option<HealthView>,
    pub(crate) notifications: Option<Vec<NotificationView>>,
}

/// A panel the view loads from `url`; `denied` panels are never fetched.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct SlotView {
    pub(crate) module: String,
    pub(crate) contribution: String,
    pub(crate) title: String,
    pub(crate) icon: String,
    pub(crate) url: String,
    pub(crate) denied: bool,
    pub(crate) min_role: &'static str,
    pub(crate) mode: &'static str,
}

/// One button of the dashboard's time-range switch.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct RangeView {
    pub(crate) label: &'static str,
    pub(crate) href: String,
    pub(crate) on: bool,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct DashboardView {
    pub(crate) shell: ShellView,
    pub(crate) subtitle: String,
    pub(crate) tiles: Vec<SlotView>,
    pub(crate) attention: Vec<SlotView>,
    pub(crate) panels: Vec<SlotView>,
    /// `Point::DashboardWide`, full width below the attention grid.
    pub(crate) wide: Vec<SlotView>,
    pub(crate) ranges: Vec<RangeView>,
}

/// The time-range switch with `selected` on; each button reloads the dashboard.
pub(crate) fn ranges(selected: controlplane::TimeRange) -> Vec<RangeView> {
    panels::RANGES
        .iter()
        .map(|(range, key, label)| RangeView {
            label,
            href: format!("/admin?{}={key}", panels::QUERY_RANGE),
            on: *range == selected,
        })
        .collect()
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct PageView {
    pub(crate) shell: ShellView,
    pub(crate) module: String,
    pub(crate) title: String,
    pub(crate) subtitle: String,
    pub(crate) head_actions: Vec<ActionItem>,
    /// `PageDecl::tiles`, one KPI tile row above the layout.
    pub(crate) tiles: Vec<SlotView>,
    /// `stack`, `g-21` or `g-2`.
    pub(crate) layout: &'static str,
    /// Layout columns in declaration order, each a list of panels.
    pub(crate) columns: Vec<Vec<SlotView>>,
    /// Tile and layout ids naming no contribution of this module.
    pub(crate) missing: Vec<String>,
}

pub(crate) fn layout_name(layout: &controlplane::Layout) -> &'static str {
    match layout {
        controlplane::Layout::Stack(_) => "stack",
        controlplane::Layout::Grid21(..) => "g-21",
        controlplane::Layout::Grid2(..) => "g-2",
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct TabView {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) url: String,
    pub(crate) counts: Vec<SlotView>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct EntityView {
    pub(crate) shell: ShellView,
    pub(crate) kind_label: String,
    pub(crate) entity_id: String,
    pub(crate) avatar_color: &'static str,
    pub(crate) avatar_initials: String,
    pub(crate) menu_url: Option<String>,
    pub(crate) headers: Vec<SlotView>,
    pub(crate) summaries: Vec<SlotView>,
    pub(crate) header_actions: Vec<ActionItem>,
    pub(crate) tabs: Vec<TabView>,
    pub(crate) selected_tab: Option<String>,
    /// The selected tab's panels sit in two columns (`Point::EntityTab::columns`).
    pub(crate) tab_grid: bool,
    pub(crate) tab_panels: Vec<SlotView>,
}

/// A full page whose module, page or entity kind is unknown.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct MissingView {
    pub(crate) shell: ShellView,
    pub(crate) title: String,
    pub(crate) detail: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct MenuView {
    pub(crate) entity: EntityRef,
    pub(crate) groups: Vec<MenuGroup>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SearchHitView {
    /// The palette's first hit, highlighted.
    pub(crate) hl: bool,
    pub(crate) title: String,
    pub(crate) subtitle: String,
    pub(crate) badge: Option<String>,
    pub(crate) href: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SearchGroupView {
    pub(crate) module: String,
    pub(crate) label: String,
    pub(crate) icon: String,
    pub(crate) hits: Vec<SearchHitView>,
    pub(crate) error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SearchView {
    pub(crate) q: String,
    pub(crate) kind: String,
    pub(crate) groups: Vec<SearchGroupView>,
    pub(crate) any: bool,
}

impl SearchView {
    pub(crate) fn new(q: String, kind: String, mut groups: Vec<SearchGroupView>) -> SearchView {
        if let Some(first) = groups.iter_mut().flat_map(|g| g.hits.iter_mut()).next() {
            first.hl = true;
        }
        let any = groups.iter().any(|g| !g.hits.is_empty() || g.error.is_some());
        SearchView { q, kind, groups, any }
    }
}

// ---------------------------------------------------------------------------
// Panels
// ---------------------------------------------------------------------------

/// How a contribution's fragment is drawn: inside panel chrome, as a KPI tile, as rows of
/// the attention host, or inline in the entity header, a tab or the sidebar. A `column`
/// has no fragment of its own: the panel route refuses it, and its table draws its cells.
pub(crate) fn mode_of(point: &Point, in_tiles: bool) -> &'static str {
    match point {
        Point::DashboardTile => "tile",
        Point::DashboardAttention => "attention",
        Point::EntityHeader(_) => "header",
        Point::EntitySummary(_) => "fact",
        Point::EntityTabCount { .. } => "count",
        Point::NavCounter => "nav",
        Point::EntityListColumn { .. } => "column",
        Point::Page(_) if in_tiles => "tile",
        Point::Page(_) | Point::DashboardPanel | Point::DashboardWide | Point::EntityTab { .. } => "panel",
    }
}

pub(crate) struct PanelInput<'a> {
    pub(crate) module: &'a str,
    pub(crate) contribution: &'a str,
    pub(crate) title: &'a str,
    pub(crate) icon: &'a str,
    pub(crate) mode: &'static str,
    pub(crate) state: PanelState,
    pub(crate) path: &'a str,
    /// The request's query (entity first), cursors included.
    pub(crate) query: &'a [(String, String)],
    pub(crate) entity: Option<&'a EntityRef>,
    /// `PanelQuery::search`.
    pub(crate) search: Option<&'a str>,
    pub(crate) error: Option<&'a str>,
    pub(crate) age_s: Option<u64>,
    pub(crate) last_value: Option<&'a LastValue>,
    pub(crate) min_role: &'static str,
    pub(crate) fallback_title: &'a str,
    pub(crate) body: Option<&'a PanelBody>,
    /// This contribution's `PanelTool`, `QueueRow` and `TableRow` actions, unbound.
    pub(crate) actions: &'a [ActionItem],
    /// A table's `EntityListColumn` columns for this page, in contribution order.
    pub(crate) list_columns: &'a [ListColumn],
    pub(crate) row_menu_kinds: &'a [String],
    pub(crate) csrf: &'a str,
    /// The form instance a `Primitive::Form` posts as; `None` renders it without a submit.
    pub(crate) form: Option<FormInstance<'a>>,
}

/// A form's claimed instance (a panel form or a modal) and, when it is shown again, what
/// was posted.
pub(crate) struct FormInstance<'a> {
    pub(crate) post_url: String,
    pub(crate) form_id: String,
    pub(crate) posted: Option<&'a [(String, String)]>,
    pub(crate) errors: &'a BTreeMap<String, String>,
    pub(crate) banner: Option<String>,
    /// Submit stays disabled: the instance is consumed.
    pub(crate) locked: bool,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct PanelRender {
    pub(crate) module: String,
    pub(crate) contribution: String,
    pub(crate) title: String,
    pub(crate) icon: String,
    pub(crate) mode: &'static str,
    /// `ok`, `empty`, `stale`, `error`, `readonly` or `denied`.
    pub(crate) state: &'static str,
    pub(crate) url: String,
    /// The provider's `PanelBody` chrome, plain text.
    pub(crate) meta: Option<String>,
    pub(crate) heading: Option<String>,
    pub(crate) note: Option<String>,
    pub(crate) banner: Option<BannerView>,
    pub(crate) error: Option<String>,
    pub(crate) age: Option<String>,
    /// The KPI tile's stale badge: `"3m"`.
    pub(crate) age_short: Option<String>,
    /// An error KPI tile's last value, shown only as its muted detail text.
    pub(crate) last_value: Option<LastValueView>,
    pub(crate) min_role: &'static str,
    pub(crate) fallback_title: String,
    pub(crate) flush: bool,
    pub(crate) body: Option<PrimView>,
    /// The `PanelTool` actions shown in the panel head, bound to the panel's element.
    pub(crate) tools: Vec<ActionItem>,
    /// One warn banner per failed list column.
    pub(crate) column_banners: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct LastValueView {
    pub(crate) value: String,
    /// `"3 min"`.
    pub(crate) age: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct BannerView {
    pub(crate) tone: &'static str,
    pub(crate) text: String,
}

/// `"42 s"`, `"3 min"` or `"2 h"`.
pub(crate) fn age_text(secs: u64) -> String {
    if secs < 60 {
        format!("{secs} s")
    } else if secs < 3600 {
        format!("{} min", secs / 60)
    } else {
        format!("{} h", secs / 3600)
    }
}

pub(crate) fn panel(input: &PanelInput<'_>) -> PanelRender {
    let paging: Vec<(String, String)> =
        input.query.iter().filter(|(k, _)| k != QUERY_CURSOR && k != QUERY_BEFORE).cloned().collect();
    let ctx = Ctx { path: input.path, query: &paging, params: input.query, input };
    let primitive = input.body.map(|b| &b.primitive);
    let body = primitive.map(|p| prim(p, &ctx));
    let shown = input
        .actions
        .iter()
        .filter(|a| !a.queue_row && !a.table_row && (input.state != PanelState::Empty || a.when_empty));
    let tools = match primitive {
        Some(Primitive::PropertyList(list)) => shown.map(|a| bind(a, input.entity, list.item.as_deref())).collect(),
        _ => shown.map(|a| bind(a, input.entity, None)).collect(),
    };
    let flush = matches!(
        body,
        Some(
            PrimView::Table(_)
                | PrimView::Timeline(_)
                | PrimView::Queue(_)
                | PrimView::Services(_)
                | PrimView::Attention(_)
        )
    );
    PanelRender {
        module: input.module.to_string(),
        contribution: input.contribution.to_string(),
        title: input.title.to_string(),
        icon: input.icon.to_string(),
        mode: input.mode,
        state: input.state.as_str(),
        url: with_param(input.path, input.query, "", None),
        meta: input.body.and_then(|b| b.meta.clone()),
        heading: input.body.and_then(|b| b.heading.clone()),
        note: input.body.and_then(|b| b.note.clone()),
        banner: panels::module_banner(input.state, input.body.and_then(|b| b.banner.as_ref()))
            .map(|b| BannerView { tone: tone(b.tone), text: b.text.clone() }),
        error: input.error.map(str::to_string),
        age: input.age_s.map(age_text),
        age_short: input.age_s.map(|s| age_text(s).replace(" min", "m").replace(' ', "")),
        last_value: input
            .last_value
            .map(|last| LastValueView { value: last.value.clone(), age: age_text(last.age_s) }),
        min_role: input.min_role,
        fallback_title: input.fallback_title.to_string(),
        flush,
        body,
        tools,
        column_banners: input
            .list_columns
            .iter()
            .filter(|c| matches!(c.outcome, ColumnOutcome::Failed(_)))
            .map(|c| {
                format!("{} column unavailable — {} module did not answer. Other columns are live.", c.label, c.module)
            })
            .collect(),
    }
}

struct Ctx<'a> {
    path: &'a str,
    /// The request's query without its paging keys, so a tab or search URL built from it
    /// starts the result set over; the pager adds its own key back.
    query: &'a [(String, String)],
    params: &'a [(String, String)],
    input: &'a PanelInput<'a>,
}

impl Ctx<'_> {
    fn param(&self, key: &str) -> Option<&str> {
        self.params.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type")]
pub(crate) enum PrimView {
    Table(TableView),
    Props(PropsView),
    Kpi(KpiView),
    Timeline(TimelineView),
    Form(PanelFormView),
    Queue(QueueView),
    Services(ServicesView),
    Chart(ChartView),
    Attention(AttentionView),
    Cell(CellView),
    Header(HeaderView),
    Empty { title: String, detail: String },
}

fn prim(p: &Primitive, ctx: &Ctx<'_>) -> PrimView {
    match p {
        Primitive::DataTable(t) => PrimView::Table(table(t, ctx)),
        Primitive::PropertyList(p) => PrimView::Props(PropsView {
            columns: p.columns.max(1),
            items: p.items.iter().map(|(k, v)| (k.clone(), cell(v))).collect(),
            code: p.code.clone(),
        }),
        // `panels::resolve` turns a ColumnCells body into the error state, so no panel draws one.
        Primitive::ColumnCells(_) => PrimView::Empty { title: String::new(), detail: String::new() },
        Primitive::KpiTile(k) => PrimView::Kpi(kpi(k)),
        Primitive::Timeline(t) => PrimView::Timeline(timeline(t, ctx)),
        Primitive::Form(f) => PrimView::Form(panel_form(f, ctx.input)),
        Primitive::WorkQueue(q) => PrimView::Queue(queue(q, ctx)),
        Primitive::ServiceStatus(s) => PrimView::Services(services(s)),
        Primitive::TimeSeriesChart(c) => PrimView::Chart(chart(c)),
        Primitive::AttentionItems(items) => PrimView::Attention(AttentionView {
            items: items
                .iter()
                .map(|a| AttentionRow {
                    tone: match a.severity {
                        Severity::Err => "danger",
                        Severity::Warn => "warn",
                    },
                    icon: if ctx.input.icon.is_empty() { "alert".to_string() } else { ctx.input.icon.to_string() },
                    title: a.title.clone(),
                    text: a.text.clone(),
                    href: link_url(&a.link),
                    action_label: a.action_label.clone(),
                })
                .collect(),
        }),
        Primitive::Cell(c) => PrimView::Cell(cell(c)),
        Primitive::EntityHeader(h) => PrimView::Header(HeaderView {
            title: h.title.clone(),
            subtitle: h.subtitle.clone(),
            status: h.status.as_ref().map(cell),
            badges: h.badges.iter().map(cell).collect(),
            color: avatar_color(&h.avatar_seed),
            initials: initials(&h.title),
        }),
        Primitive::Empty { title, detail } => PrimView::Empty { title: title.clone(), detail: detail.clone() },
    }
}

// ---- DataTable ----

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ColumnView {
    pub(crate) label: String,
    pub(crate) num: bool,
    /// The sort arrow of the column the rows are ordered by.
    pub(crate) sorted: Option<&'static str>,
    /// An `EntityListColumn`: its label is wrapped in a `span`.
    pub(crate) contributed: bool,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct TableCell {
    pub(crate) cell: CellView,
    pub(crate) num: bool,
    /// A failed list column's marker title; the cell draws the marker instead of `cell`.
    pub(crate) failed: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct RowView {
    pub(crate) cells: Vec<TableCell>,
    pub(crate) href: Option<String>,
    pub(crate) menu_url: Option<String>,
    pub(crate) highlight: bool,
    /// The row's `TableRow` actions, bound to its entity and `item`.
    pub(crate) actions: Vec<ActionItem>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct FilterView {
    pub(crate) label: String,
    pub(crate) value: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct TableView {
    pub(crate) columns: Vec<ColumnView>,
    pub(crate) rows: Vec<RowView>,
    pub(crate) selectable: bool,
    pub(crate) row_menu: bool,
    /// Some row has `TableRow` actions: every row gets a trailing actions cell.
    pub(crate) row_actions: bool,
    pub(crate) toolbar: bool,
    pub(crate) search_placeholder: Option<String>,
    pub(crate) q: String,
    pub(crate) search_url: String,
    pub(crate) filters: Vec<FilterView>,
    pub(crate) foot: bool,
    pub(crate) total: String,
    pub(crate) size: u32,
    pub(crate) next_url: Option<String>,
    /// The pager's `"1 / N"` between its buttons; `None` past the first page.
    pub(crate) page_label: Option<String>,
}

/// `"1 / N"` on the first page only: a cursor does not say how far into the list it is.
fn page_label(page: &controlplane::PageInfo, ctx: &Ctx<'_>) -> Option<String> {
    if ctx.param(QUERY_CURSOR).is_some() {
        return None;
    }
    match page.total {
        Some(total) if page.size > 0 => Some(format!("1 / {}", total.div_ceil(u64::from(page.size)).max(1))),
        _ if page.next_cursor.is_none() => Some("1 / 1".to_string()),
        _ => None,
    }
}

fn filter_views(filters: &[controlplane::FilterDef], ctx: &Ctx<'_>) -> Vec<FilterView> {
    filters
        .iter()
        .map(|f| FilterView { label: f.label.clone(), value: ctx.param(&f.key).unwrap_or("any").to_string() })
        .collect()
}

fn column_views(columns: &[controlplane::Column]) -> Vec<ColumnView> {
    columns
        .iter()
        .map(|c| ColumnView {
            label: c.label.clone(),
            num: c.align == Align::Right,
            sorted: c.sorted.map(|dir| match dir {
                SortDir::Asc => "↑",
                SortDir::Desc => "↓",
            }),
            contributed: false,
        })
        .collect()
}

/// `in_list`: the cells belong to a table row that opens an entity, so a linked user cell
/// carries its link inside.
fn table_cells(cells: &[controlplane::Cell], columns: &[controlplane::Column], in_list: bool) -> Vec<TableCell> {
    cells
        .iter()
        .enumerate()
        .map(|(i, c)| TableCell {
            cell: list_cell(c, in_list),
            num: columns.get(i).is_some_and(|col| col.align == Align::Right),
            failed: None,
        })
        .collect()
}

fn list_cell(c: &controlplane::Cell, in_list: bool) -> CellView {
    let mut view = cell(c);
    view.inner_link = in_list && view.kind == "user" && view.href.is_some();
    view
}

/// The cell of list column `column` in a row whose entity id is `id`: the column's cell for
/// it, an empty cell when it has none, or the failure marker.
fn list_column_cell(column: &ListColumn, id: Option<&str>, in_list: bool) -> TableCell {
    let num = column.align == Align::Right;
    let blank = controlplane::Cell {
        kind: CellKind::Text,
        value: String::new(),
        tone: None,
        link: None,
        bar: None,
        more: Vec::new(),
    };
    match &column.outcome {
        ColumnOutcome::Cells(cells) => {
            let found = id.and_then(|id| cells.get(id)).unwrap_or(&blank);
            TableCell { cell: list_cell(found, in_list), num, failed: None }
        }
        ColumnOutcome::Failed(_) => TableCell {
            cell: cell(&blank),
            num,
            failed: Some(format!("{} module unavailable", column.module)),
        },
    }
}

pub(crate) fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The foot's count: with an `item_noun`, `"M accounts · showing 1–N"` on the first page
/// and `"M accounts"` past it (a cursor does not say how far in it is), `"N accounts"`
/// without a total; without one, `"N of M"` or `"N rows"`.
fn table_total(t: &controlplane::DataTable, shown: u64, ctx: &Ctx<'_>) -> String {
    let noun = &t.item_noun;
    match t.page.total {
        Some(total) if noun.is_empty() => format!("{} of {}", thousands(shown), thousands(total)),
        None if noun.is_empty() => format!("{} rows", thousands(shown)),
        Some(total) if ctx.param(QUERY_CURSOR).is_none() => {
            format!("{} {noun} · showing 1–{}", thousands(total), thousands(shown))
        }
        Some(total) => format!("{} {noun}", thousands(total)),
        None => format!("{} {noun}", thousands(shown)),
    }
}

fn table(t: &controlplane::DataTable, ctx: &Ctx<'_>) -> TableView {
    let kind = t.list_columns_for.as_deref();
    let at = t.list_columns_at.min(t.columns.len());
    let list_columns = ctx.input.list_columns;
    let rows: Vec<RowView> = t
        .rows
        .iter()
        .map(|r| {
            let in_list = r.entity.is_some();
            let mut cells = table_cells(&r.cells, &t.columns, in_list);
            let id = r.entity.as_ref().filter(|e| Some(e.kind.as_str()) == kind).map(|e| e.id.as_str());
            let at = at.min(cells.len());
            cells.splice(at..at, list_columns.iter().map(|c| list_column_cell(c, id, in_list)));
            RowView {
                cells,
                href: r.entity.as_ref().map(entity_url),
                menu_url: r
                    .entity
                    .as_ref()
                    .filter(|e| ctx.input.row_menu_kinds.contains(&e.kind))
                    .map(|e| format!("/admin/_menu/{}/{}", encode(&e.kind), encode(&e.id))),
                highlight: r.highlight,
                actions: r
                    .actions
                    .iter()
                    .filter_map(|id| ctx.input.actions.iter().find(|a| a.table_row && &a.id == id))
                    .map(|a| bind(a, r.entity.as_ref(), r.item.as_deref()))
                    .collect(),
            }
        })
        .collect();
    let mut columns = column_views(&t.columns);
    columns.splice(
        at..at,
        list_columns.iter().map(|c| ColumnView {
            label: c.label.clone(),
            num: c.align == Align::Right,
            sorted: None,
            contributed: true,
        }),
    );
    let total = table_total(t, rows.len() as u64, ctx);
    TableView {
        columns,
        row_menu: rows.iter().any(|r| r.menu_url.is_some()),
        row_actions: rows.iter().any(|r| !r.actions.is_empty()),
        rows,
        selectable: t.selectable,
        toolbar: !t.filters.is_empty() || t.search_placeholder.is_some(),
        search_placeholder: t.search_placeholder.clone(),
        q: ctx.input.search.unwrap_or_default().to_string(),
        search_url: with_param(ctx.path, ctx.query, QUERY_SEARCH, None),
        filters: filter_views(&t.filters, ctx),
        foot: t.page.next_cursor.is_some() || t.page.total.is_some_and(|n| n > 0 || !t.rows.is_empty()),
        total,
        size: t.page.size,
        next_url: t.page.next_cursor.as_deref().map(|c| with_param(ctx.path, ctx.query, QUERY_CURSOR, Some(c))),
        page_label: page_label(&t.page, ctx),
    }
}

// ---- PropertyList ----

#[derive(Clone, Debug, Serialize)]
pub(crate) struct PropsView {
    pub(crate) columns: u8,
    pub(crate) items: Vec<(String, CellView)>,
    pub(crate) code: Option<String>,
}

// ---- KpiTile ----

#[derive(Clone, Debug, Serialize)]
pub(crate) struct KpiView {
    pub(crate) label: String,
    pub(crate) value: String,
    pub(crate) unit: Option<String>,
    pub(crate) delta_value: Option<String>,
    /// `up`, `down`, `flat` or `bad-up` (`bundle.css:251-252`).
    pub(crate) delta_class: &'static str,
    pub(crate) delta_period: String,
    pub(crate) bar: Option<f64>,
    pub(crate) spark: Option<SparkView>,
    pub(crate) href: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SparkView {
    pub(crate) line: String,
    pub(crate) area: String,
    pub(crate) color: &'static str,
}

/// The delta's colour class (`bundle.css:251-252`) from `tone` alone, never from the
/// value's text.
fn delta_class(d: &controlplane::Delta) -> &'static str {
    match d.tone {
        Some(Tone::Ok) => "up",
        Some(Tone::Err) => "down",
        Some(Tone::Warn) => "bad-up",
        Some(Tone::Info) | None => "flat",
    }
}

fn palette(p: controlplane::Palette) -> &'static str {
    match p {
        controlplane::Palette::Chart1 => "var(--chart-1)",
        controlplane::Palette::Chart2 => "var(--chart-2)",
        controlplane::Palette::Warn => "var(--warn)",
    }
}

/// The mockup's `spark` path (`bundle.js:170-175`).
fn spark(values: &[f64], color: controlplane::Palette) -> Option<SparkView> {
    if values.len() < 2 {
        return None;
    }
    let (w, h) = (160.0_f64, 26.0_f64);
    let max = values.iter().copied().fold(f64::MIN, f64::max);
    let min = values.iter().copied().fold(f64::MAX, f64::min);
    let span = if max - min == 0.0 { 1.0 } else { max - min };
    let step = w / (values.len() - 1) as f64;
    let mut line = String::new();
    for (i, v) in values.iter().enumerate() {
        let x = i as f64 * step;
        let y = h - 2.0 - ((v - min) / span) * (h - 4.0);
        line.push_str(&format!("{}{x:.1},{y:.1}", if i == 0 { 'M' } else { 'L' }));
    }
    let area = format!("{line}L{w},{h}L0,{h}Z");
    Some(SparkView { line, area, color: palette(color) })
}

fn kpi(k: &controlplane::KpiTile) -> KpiView {
    KpiView {
        label: k.label.clone(),
        value: k.value.clone(),
        unit: k.unit.clone(),
        delta_value: k.delta.as_ref().map(|d| d.value.clone()),
        delta_class: k.delta.as_ref().map(delta_class).unwrap_or("flat"),
        delta_period: k.delta.as_ref().map(|d| d.period.clone()).unwrap_or_default(),
        bar: k.capacity.map(|(used, total)| if total > 0.0 { (used / total * 100.0).clamp(0.0, 100.0) } else { 0.0 }),
        spark: spark(&k.series, k.spark),
        href: k.link.as_ref().map(link_url),
    }
}

// ---- Timeline ----

#[derive(Clone, Debug, Serialize)]
pub(crate) struct TimelineItem {
    pub(crate) time: String,
    pub(crate) icon: String,
    pub(crate) tone: &'static str,
    pub(crate) actor: Option<CellView>,
    pub(crate) verb: String,
    pub(crate) summary: String,
    pub(crate) target: Option<CellView>,
    pub(crate) detail: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct TimelineDay {
    pub(crate) label: String,
    pub(crate) items: Vec<TimelineItem>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct TimelineView {
    pub(crate) days: Vec<TimelineDay>,
    pub(crate) older_url: Option<String>,
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// `("2026-10-20", "Oct 20", "17:44")` from an RFC 3339 timestamp or Postgres timestamptz
/// text (`2026-10-20 17:44:05+01`); unparseable `at` gives `("", "", at)`.
pub(crate) fn day_and_time(at: &str) -> (String, String, String) {
    let date = at.get(0..10).unwrap_or("");
    let time = at.get(11..16).unwrap_or(at);
    let month = date.get(5..7).and_then(|m| m.parse::<usize>().ok()).filter(|m| (1..=12).contains(m));
    let day = date.get(8..10).and_then(|d| d.parse::<u32>().ok());
    match (month, day) {
        (Some(m), Some(d)) => (date.to_string(), format!("{} {d}", MONTHS[m - 1]), time.to_string()),
        _ => (String::new(), String::new(), at.to_string()),
    }
}

fn timeline(t: &controlplane::Timeline, ctx: &Ctx<'_>) -> TimelineView {
    let mut days: Vec<(String, TimelineDay)> = Vec::new();
    for e in &t.entries {
        let (key, label, time) = day_and_time(&e.at);
        let item = TimelineItem {
            time,
            icon: if e.icon.is_empty() { "activity".to_string() } else { e.icon.clone() },
            tone: e.tone.map(tone).unwrap_or(""),
            actor: e.actor.as_ref().map(ref_cell),
            verb: e.verb.clone(),
            summary: e.summary.clone(),
            target: e.target.as_ref().map(|t| CellView {
                kind: "text",
                value: entity_param(t),
                tone: "",
                href: Some(entity_url(t)),
                color: "",
                initials: String::new(),
                bar: None,
                bar_tone: "",
                more: Vec::new(),
                inner_link: false,
            }),
            detail: e.detail.clone(),
        };
        // An entry whose `at` does not parse (empty key) joins the current run, and a run of
        // only such entries takes the day of the first parsed entry after them.
        match days.last_mut() {
            Some((k, day)) if *k == key || key.is_empty() => day.items.push(item),
            Some((k, day)) if k.is_empty() => {
                *k = key;
                day.label = label;
                day.items.push(item);
            }
            _ => days.push((key, TimelineDay { label, items: vec![item] })),
        }
    }
    TimelineView {
        days: days.into_iter().map(|(_, d)| d).collect(),
        older_url: t.cursor_before.as_deref().map(|c| with_param(ctx.path, ctx.query, QUERY_BEFORE, Some(c))),
    }
}

// ---- Form ----

#[derive(Clone, Debug, Serialize)]
pub(crate) struct FieldView {
    pub(crate) key: String,
    pub(crate) label: String,
    pub(crate) type_name: String,
    /// `input`, `int`, `textarea`, `toggle`, `select`, `checks`, `ref` or `money`.
    pub(crate) control: &'static str,
    pub(crate) mono: bool,
    pub(crate) required: bool,
    pub(crate) readonly: bool,
    pub(crate) help: Option<String>,
    pub(crate) value: String,
    pub(crate) options: Vec<String>,
    pub(crate) selected: Vec<String>,
    pub(crate) currency: String,
    /// The body key of a Money field's currency input.
    pub(crate) currency_name: String,
    pub(crate) ref_icon: &'static str,
    pub(crate) ref_kind: String,
    pub(crate) error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct FormView {
    pub(crate) fields: Vec<FieldView>,
    pub(crate) submit_label: String,
    pub(crate) note: Option<String>,
}

/// A `Primitive::Form` in a panel; `form_id` and `post_url` are set when it has a form
/// instance to post as.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct PanelFormView {
    #[serde(flatten)]
    pub(crate) form: FormView,
    pub(crate) post_url: Option<String>,
    pub(crate) csrf: String,
    pub(crate) form_id: Option<String>,
    pub(crate) banner: Option<String>,
    /// Save stays disabled: the instance is consumed.
    pub(crate) locked: bool,
}

fn posted_values(posted: Option<&[(String, String)]>, key: &str) -> Vec<String> {
    posted.unwrap_or_default().iter().filter(|(k, _)| k == key).map(|(_, v)| v.clone()).collect()
}

/// A field shows its stored default when the form is fresh or the field readonly, else what
/// was posted; Multi and Money show the decoded value where it decodes.
fn field(f: &Field, posted: Option<&[(String, String)]>, errors: &BTreeMap<String, String>) -> FieldView {
    let raw = match posted {
        Some(body) if !f.readonly => actions::posted_raw(f, body),
        _ => f.default.clone(),
    };
    let decoded = raw.as_deref().and_then(|r| decode_field(&f.kind, r).ok());
    let currency_name = money_currency_key(&f.key);
    let reposted = posted.filter(|_| !f.readonly);
    let (value, selected, currency) = match (&f.kind, decoded) {
        (_, Some(FieldValue::Multi(picked))) => (String::new(), picked, String::new()),
        (_, Some(FieldValue::Money { amount, currency })) => (amount, Vec::new(), currency),
        (FieldKind::Multi(_), _) => (String::new(), posted_values(reposted, &f.key), String::new()),
        (FieldKind::Money(_), _) => {
            let amount = posted_values(reposted, &f.key).into_iter().next().unwrap_or_default();
            let currency = posted_values(reposted, &currency_name).into_iter().next().unwrap_or_default();
            (amount, Vec::new(), currency)
        }
        _ => (raw.unwrap_or_default(), Vec::new(), String::new()),
    };
    let (control, type_name, options, mono) = match &f.kind {
        FieldKind::String => ("input", "string".to_string(), Vec::new(), false),
        FieldKind::Text => ("textarea", "text".to_string(), Vec::new(), false),
        FieldKind::Int => ("int", "int".to_string(), Vec::new(), false),
        FieldKind::Float => ("input", "float".to_string(), Vec::new(), false),
        FieldKind::Bool => ("toggle", "bool".to_string(), Vec::new(), false),
        FieldKind::Enum(o) => ("select", "enum".to_string(), o.clone(), false),
        FieldKind::Multi(o) => ("checks", "multi".to_string(), o.clone(), false),
        FieldKind::Json => ("textarea", "json".to_string(), Vec::new(), true),
        FieldKind::Ref(kind) => ("ref", format!("ref {kind}"), Vec::new(), false),
        FieldKind::Money(c) => ("money", "money".to_string(), c.clone(), false),
        FieldKind::Duration => ("input", "duration".to_string(), Vec::new(), false),
    };
    let ref_kind = match &f.kind {
        FieldKind::Ref(kind) => kind.clone(),
        _ => String::new(),
    };
    FieldView {
        key: f.key.clone(),
        label: f.label.clone(),
        type_name,
        control,
        mono,
        required: f.required,
        readonly: f.readonly,
        help: f.help.clone(),
        selected,
        currency,
        currency_name,
        value,
        options,
        ref_icon: if matches!(ref_kind.as_str(), "player" | "user") { "user" } else { "hash" },
        ref_kind,
        error: errors.get(&f.key).cloned(),
    }
}

/// `posted` is the body being shown again, `None` for a fresh form.
pub(crate) fn form(
    spec: &FormSpec,
    posted: Option<&[(String, String)]>,
    errors: &BTreeMap<String, String>,
) -> FormView {
    FormView {
        fields: spec.fields.iter().map(|f| field(f, posted, errors)).collect(),
        submit_label: spec.submit_label.clone(),
        note: spec.note.clone(),
    }
}

fn panel_form(spec: &FormSpec, input: &PanelInput<'_>) -> PanelFormView {
    let none = BTreeMap::new();
    let instance = input.form.as_ref();
    PanelFormView {
        form: form(spec, instance.and_then(|i| i.posted), instance.map_or(&none, |i| i.errors)),
        post_url: instance.map(|i| i.post_url.clone()),
        csrf: input.csrf.to_string(),
        form_id: instance.map(|i| i.form_id.clone()),
        banner: instance.and_then(|i| i.banner.clone()),
        locked: instance.is_some_and(|i| i.locked),
    }
}

/// A panel form's frame: the contribution its instance was minted for.
pub(crate) struct FormFrame<'a> {
    pub(crate) module: &'a str,
    pub(crate) decl: &'a controlplane::ContributionDecl,
    pub(crate) mode: &'static str,
    pub(crate) path: &'a str,
    pub(crate) query: &'a [(String, String)],
    pub(crate) entity: Option<&'a EntityRef>,
    pub(crate) actions: &'a [ActionItem],
    pub(crate) csrf: &'a str,
}

/// The panel fragment of a panel form shown again from its stored spec and chrome after a
/// 422 or 409.
pub(crate) fn panel_form_result(
    frame: &FormFrame<'_>,
    spec: &FormSpec,
    chrome: &Chrome,
    instance: FormInstance<'_>,
) -> PanelRender {
    let body = chrome.around(Primitive::Form(spec.clone()));
    let input = PanelInput {
        module: frame.module,
        contribution: &frame.decl.id,
        title: &frame.decl.title,
        icon: &frame.decl.icon,
        mode: frame.mode,
        state: PanelState::Ok,
        path: frame.path,
        query: frame.query,
        entity: frame.entity,
        search: None,
        error: None,
        age_s: None,
        last_value: None,
        min_role: crate::role_name(frame.decl.min_role),
        fallback_title: "",
        body: Some(&body),
        actions: frame.actions,
        row_menu_kinds: &[],
        list_columns: &[],
        csrf: frame.csrf,
        form: Some(instance),
    };
    panel(&input)
}

// ---- WorkQueue ----

#[derive(Clone, Debug, Serialize)]
pub(crate) struct QueueStateView {
    pub(crate) label: String,
    pub(crate) count: String,
    pub(crate) tone: &'static str,
    pub(crate) on: bool,
    pub(crate) url: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct QueueItemView {
    pub(crate) id: String,
    pub(crate) entity: Option<CellView>,
    /// `WorkQueue::columns`' cells, drawn between the id and the state.
    pub(crate) cells: Vec<TableCell>,
    pub(crate) state: CellView,
    pub(crate) attempts: Vec<&'static str>,
    pub(crate) attempts_title: String,
    pub(crate) next_attempt: Option<String>,
    pub(crate) last_error: Option<String>,
    pub(crate) error_color: &'static str,
    pub(crate) payload: Option<String>,
    pub(crate) actions: Vec<ActionItem>,
}

/// The toolbar and foot fields carry `TableView`'s names, so `table_toolbar` and
/// `table_foot` draw a queue as they draw a table.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct QueueView {
    pub(crate) states: Vec<QueueStateView>,
    pub(crate) items: Vec<QueueItemView>,
    pub(crate) has_entity: bool,
    pub(crate) has_payload: bool,
    /// The id column's header.
    pub(crate) item_label: String,
    pub(crate) columns: Vec<ColumnView>,
    pub(crate) selectable: bool,
    pub(crate) toolbar: bool,
    pub(crate) search_placeholder: Option<String>,
    pub(crate) q: String,
    pub(crate) search_url: String,
    pub(crate) filters: Vec<FilterView>,
    pub(crate) foot: bool,
    /// `"7 messages"`, or `"25 of 309 messages"` when more exist.
    pub(crate) total: String,
    pub(crate) size: u32,
    pub(crate) next_url: Option<String>,
    pub(crate) page_label: Option<String>,
}

/// The attempts bar always draws this many squares (`pages-platform.js:47`); its title
/// carries the real count.
const ATTEMPT_SQUARES: u32 = 5;

fn queue(q: &controlplane::WorkQueue, ctx: &Ctx<'_>) -> QueueView {
    let items: Vec<QueueItemView> = q
        .items
        .iter()
        .map(|it| {
            let state_tone = status_tone(&it.state);
            let attempts = (0..ATTEMPT_SQUARES)
                .map(|i| match (i < it.attempts, state_tone == "ok" && i + 1 == it.attempts) {
                    (true, true) => "ok",
                    (true, false) => "f",
                    (false, _) => "",
                })
                .collect();
            let actions = it
                .actions
                .iter()
                .filter_map(|id| ctx.input.actions.iter().find(|a| a.queue_row && &a.id == id))
                .map(|a| bind(a, it.entity.as_ref(), Some(&it.id)))
                .collect();
            QueueItemView {
                id: it.id.clone(),
                entity: it.entity.as_ref().map(ref_cell),
                cells: table_cells(&it.cells, &q.columns, false),
                state: CellView {
                    kind: "badge",
                    value: it.state.clone(),
                    tone: state_tone,
                    href: None,
                    color: "",
                    initials: String::new(),
                    bar: None,
                    bar_tone: "",
                    more: Vec::new(),
                    inner_link: false,
                },
                attempts,
                attempts_title: format!("{} attempts", it.attempts),
                next_attempt: it.next_attempt_at.clone(),
                last_error: it.last_error.clone(),
                error_color: if state_tone == "err" { "var(--err)" } else { "var(--text-2)" },
                payload: it.payload_preview.clone(),
                actions,
            }
        })
        .collect();
    let shown = items.len() as u64;
    let total = match q.page.total {
        Some(total) if total != shown => format!("{} of {} {}", thousands(shown), thousands(total), q.item_noun),
        _ => format!("{} {}", thousands(shown), q.item_noun),
    };
    QueueView {
        states: q
            .states
            .iter()
            .map(|s| QueueStateView {
                label: s.label.clone(),
                count: thousands(s.count),
                tone: s.tone.map(tone).unwrap_or(""),
                on: s.key == q.selected,
                url: with_param(ctx.path, ctx.query, QUERY_STATE, Some(&s.key)),
            })
            .collect(),
        has_entity: items.iter().any(|i| i.entity.is_some()),
        has_payload: items.iter().any(|i| i.payload.is_some()),
        item_label: q.item_label.clone(),
        columns: column_views(&q.columns),
        selectable: q.selectable,
        toolbar: !q.filters.is_empty() || q.search_placeholder.is_some(),
        search_placeholder: q.search_placeholder.clone(),
        q: ctx.input.search.unwrap_or_default().to_string(),
        search_url: with_param(ctx.path, ctx.query, QUERY_SEARCH, None),
        filters: filter_views(&q.filters, ctx),
        foot: !items.is_empty() || q.page.next_cursor.is_some(),
        total: total.trim_end().to_string(),
        size: q.page.size,
        next_url: q.page.next_cursor.as_deref().map(|c| with_param(ctx.path, ctx.query, QUERY_CURSOR, Some(c))),
        page_label: page_label(&q.page, ctx),
        items,
    }
}

// ---- ServiceStatus ----

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ServiceView {
    pub(crate) name: String,
    pub(crate) state: String,
    pub(crate) running: bool,
    pub(crate) tone: &'static str,
    pub(crate) bars: Vec<&'static str>,
    pub(crate) value: Option<String>,
    pub(crate) note: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ServicesView {
    pub(crate) items: Vec<ServiceView>,
    pub(crate) columns: u8,
    pub(crate) value_label: Option<String>,
}

fn service_state(s: ServiceState) -> (&'static str, &'static str, &'static str) {
    match s {
        ServiceState::Running => ("running", "ok", ""),
        ServiceState::Degraded => ("degraded", "warn", "w"),
        ServiceState::Restarting => ("restarting", "warn", "w"),
        ServiceState::Crashed => ("crashed", "err", "e"),
        ServiceState::Down => ("down", "err", "e"),
    }
}

pub(crate) fn service_items(items: &[controlplane::ServiceItem]) -> Vec<ServiceView> {
    items
        .iter()
        .map(|s| {
            let (state, tone, _) = service_state(s.state);
            ServiceView {
                name: s.name.clone(),
                state: state.to_string(),
                running: s.state == ServiceState::Running,
                tone,
                bars: s.history.iter().map(|(_, st)| service_state(*st).2).collect(),
                value: s.value.clone(),
                note: s.note.clone(),
            }
        })
        .collect()
}

fn services(s: &controlplane::ServiceStatus) -> ServicesView {
    ServicesView { items: service_items(&s.items), columns: s.columns, value_label: s.value_label.clone() }
}

// ---- TimeSeriesChart ----

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ChartDot {
    pub(crate) x: String,
    pub(crate) y: String,
    pub(crate) title: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ChartSeries {
    pub(crate) label: String,
    pub(crate) color: &'static str,
    pub(crate) path: String,
    pub(crate) dots: Vec<ChartDot>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ChartGrid {
    pub(crate) y: String,
    pub(crate) ty: String,
    pub(crate) left: String,
    pub(crate) right: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ChartLabel {
    pub(crate) x: String,
    pub(crate) text: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ChartView {
    pub(crate) left: f64,
    pub(crate) right: f64,
    pub(crate) left_text_x: f64,
    pub(crate) right_text_x: f64,
    pub(crate) grid: Vec<ChartGrid>,
    pub(crate) labels: Vec<ChartLabel>,
    pub(crate) series: Vec<ChartSeries>,
}

const CHART_COLORS: [&str; 2] = ["var(--chart-1)", "var(--chart-2)"];

/// JavaScript's `+(n).toFixed(1)`: one decimal, a trailing `.0` dropped.
fn fixed1(n: f64) -> String {
    let s = format!("{n:.1}");
    s.strip_suffix(".0").map(str::to_string).unwrap_or(s)
}

/// The mockup's `chart` geometry (`bundle.js:244-260`), with each axis's max computed.
fn chart(c: &controlplane::TimeSeriesChart) -> ChartView {
    let (w, h, l, t, b) = (640.0_f64, 220.0_f64, 38.0_f64, 12.0_f64, 24.0_f64);
    let has_right = c.series.iter().any(|s| s.axis == controlplane::Axis::Right);
    let r = if has_right { 40.0 } else { 12.0 };
    let n = c.buckets.len().max(1);
    let step = if n > 1 { (w - l - r) / (n - 1) as f64 } else { 0.0 };
    let xs: Vec<f64> = (0..n).map(|i| l + i as f64 * step).collect();
    let axis_max = |axis: controlplane::Axis| {
        let m = c
            .series
            .iter()
            .filter(|s| s.axis == axis)
            .flat_map(|s| s.values.iter().copied())
            .fold(0.0_f64, f64::max);
        if m > 0.0 {
            m
        } else {
            1.0
        }
    };
    let (left_max, right_max) = (axis_max(controlplane::Axis::Left), axis_max(controlplane::Axis::Right));
    let y = |v: f64, max: f64| t + (1.0 - v / max) * (h - t - b);
    let series = c
        .series
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let max = if s.axis == controlplane::Axis::Right { right_max } else { left_max };
            let mut path = String::new();
            let mut dots = Vec::new();
            for (j, v) in s.values.iter().enumerate().take(xs.len()) {
                let (px, py) = (xs[j], y(*v, max));
                path.push_str(&format!("{}{px:.1},{py:.1}", if j == 0 { 'M' } else { 'L' }));
                dots.push(ChartDot {
                    x: format!("{px:.1}"),
                    y: format!("{py:.1}"),
                    title: format!("{}: {} {}", c.buckets.get(j).map(String::as_str).unwrap_or(""), fixed1(*v), s.unit),
                });
            }
            ChartSeries { label: s.label.clone(), color: CHART_COLORS[i % CHART_COLORS.len()], path, dots }
        })
        .collect();
    let grid = [0.0, 0.25, 0.5, 0.75, 1.0]
        .iter()
        .map(|f| ChartGrid {
            y: format!("{:.1}", t + (1.0 - f) * (h - t - b)),
            ty: format!("{:.1}", t + (1.0 - f) * (h - t - b) + 3.5),
            left: fixed1(left_max * f),
            right: has_right.then(|| fixed1(right_max * f)),
        })
        .collect();
    let labels = c
        .buckets
        .iter()
        .zip(&xs)
        .map(|(text, x)| ChartLabel { x: format!("{x:.1}"), text: text.clone() })
        .collect();
    ChartView { left: l, right: w - r, left_text_x: l - 6.0, right_text_x: w - r + 6.0, grid, labels, series }
}

// ---- AttentionItems / EntityHeader ----

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AttentionRow {
    /// The `modal-icon` class: `danger` or `warn`.
    pub(crate) tone: &'static str,
    pub(crate) icon: String,
    pub(crate) title: String,
    pub(crate) text: String,
    pub(crate) href: String,
    pub(crate) action_label: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AttentionView {
    pub(crate) items: Vec<AttentionRow>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct HeaderView {
    pub(crate) title: String,
    pub(crate) subtitle: String,
    pub(crate) status: Option<CellView>,
    pub(crate) badges: Vec<CellView>,
    pub(crate) color: &'static str,
    pub(crate) initials: String,
}

// ---------------------------------------------------------------------------
// Modals
// ---------------------------------------------------------------------------

/// What a modal renders: the form or the confirmation its instance stored.
#[derive(Clone, Copy, Debug)]
pub(crate) enum ModalSpec<'a> {
    Form(&'a FormSpec),
    Confirm(&'a ConfirmSpec),
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ModalView {
    pub(crate) label: String,
    pub(crate) post_url: String,
    pub(crate) csrf: String,
    /// The form instance the modal posts as `_form`.
    pub(crate) form_id: String,
    /// The `modal-icon` class.
    pub(crate) tone: &'static str,
    pub(crate) icon: String,
    /// `danger` or `primary`.
    pub(crate) go: &'static str,
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) impact: Vec<(String, CellView)>,
    /// `required`, `optional` or `none`.
    pub(crate) reason: &'static str,
    /// The reason input's placeholder.
    pub(crate) reason_hint: Option<String>,
    pub(crate) reason_value: String,
    pub(crate) reason_error: Option<String>,
    pub(crate) type_to_confirm: Option<String>,
    pub(crate) confirm_label: String,
    pub(crate) form: Option<FormView>,
    pub(crate) banner: Option<String>,
    /// Submit stays disabled: the instance is consumed.
    pub(crate) locked: bool,
}

/// The modal of `decl` for its form instance `form_id`; `posted` is the body being shown
/// again with `errors`, `None` for a fresh modal.
pub(crate) fn modal(
    decl: &controlplane::ActionDecl,
    spec: ModalSpec<'_>,
    post_url: String,
    csrf: String,
    form_id: String,
    posted: Option<&[(String, String)]>,
    errors: &BTreeMap<String, String>,
) -> ModalView {
    let reason_value = posted.and_then(|b| actions::first(b, actions::FIELD_REASON)).unwrap_or_default();
    let mut view = ModalView {
        label: decl.label.clone(),
        post_url,
        csrf,
        form_id,
        tone: "info",
        icon: if decl.icon.is_empty() { "info".to_string() } else { decl.icon.clone() },
        go: if decl.danger { "danger" } else { "primary" },
        title: decl.label.clone(),
        body: String::new(),
        impact: Vec::new(),
        reason: "none",
        reason_hint: None,
        reason_value: reason_value.to_string(),
        reason_error: errors.get(actions::FIELD_REASON).cloned(),
        type_to_confirm: None,
        confirm_label: decl.label.clone(),
        form: None,
        banner: None,
        locked: false,
    };
    match spec {
        ModalSpec::Form(spec) => {
            view.tone = "ok";
            view.go = "primary";
            view.form = Some(form(spec, posted, errors));
        }
        ModalSpec::Confirm(spec) => {
            view.tone = match spec.tone {
                ConfirmTone::Danger => "danger",
                ConfirmTone::Warn => "warn",
                ConfirmTone::Info => "info",
            };
            view.go = if spec.tone == ConfirmTone::Danger { "danger" } else { "primary" };
            if decl.icon.is_empty() {
                view.icon = if spec.tone == ConfirmTone::Danger { "alert" } else { "info" }.to_string();
            }
            view.title = spec.title.clone();
            view.body = spec.body.clone();
            view.impact = spec.impact.iter().map(|(k, v)| (k.clone(), cell(v))).collect();
            view.reason_hint = spec.reason_hint.clone();
            view.reason = match spec.reason {
                ReasonReq::Required => "required",
                ReasonReq::Optional => "optional",
                ReasonReq::None => "none",
            };
            view.type_to_confirm = spec.type_to_confirm.clone();
            view.confirm_label = spec.confirm_label.clone();
        }
    }
    let shown: Vec<&str> = view.form.iter().flat_map(|f| f.fields.iter().map(|x| x.key.as_str())).collect();
    let stray: Vec<String> = errors
        .iter()
        .filter(|(k, _)| {
            !shown.contains(&k.as_str()) && !(k.as_str() == actions::FIELD_REASON && view.reason != "none")
        })
        .map(|(k, v)| format!("{k}: {v}"))
        .collect();
    if !stray.is_empty() {
        view.banner = Some(stray.join("; "));
    }
    view
}

/// A non-redirect POST result: the modal shown again, or a toast.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ActionResultView {
    pub(crate) modal: Option<ModalView>,
    pub(crate) toast: Option<String>,
    /// The toast's tone: `err` or `info`.
    pub(crate) tone: &'static str,
}

/// The modal of `decl` shown again after a POST for `instance`: its banner, when set,
/// replaces the stray-error banner.
pub(crate) fn modal_rerender(
    decl: &controlplane::ActionDecl,
    spec: ModalSpec<'_>,
    csrf: String,
    instance: FormInstance<'_>,
) -> ActionResultView {
    let mut view = modal(decl, spec, instance.post_url, csrf, instance.form_id, instance.posted, instance.errors);
    if instance.banner.is_some() {
        view.banner = instance.banner;
    }
    view.locked = instance.locked;
    ActionResultView { modal: Some(view), toast: None, tone: "err" }
}
