//! The portal's template layer: the view models the templates render, their conversion from
//! control-plane types, and the embedded templates.

use std::collections::BTreeMap;

use controlplane::{
    Align, CellKind, ConfirmTone, EntityRef, Field, FieldKind, FormSpec, Link, Point, Primitive,
    ReasonReq, Severity, ServiceState, Tone, Trend,
};
use minijinja::{Environment, Value};
use serde::Serialize;

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
}

pub(crate) fn cell(c: &controlplane::Cell) -> CellView {
    let kind = match c.kind {
        CellKind::Text => "text",
        CellKind::Mono => "mono",
        CellKind::Badge => "badge",
        CellKind::Ref => "ref",
        CellKind::Number => "number",
        CellKind::Bytes => "bytes",
        CellKind::Time => "time",
        CellKind::User => "user",
    };
    let tone = match (c.tone, c.kind) {
        (Some(t), _) => tone(t),
        (None, CellKind::Badge) => status_tone(&c.value),
        (None, _) => "",
    };
    CellView {
        kind,
        value: c.value.clone(),
        tone,
        href: c.link.as_ref().map(link_url),
        color: avatar_color(&c.value),
        initials: initials(&c.value),
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

pub(crate) fn for_entity(item: &ActionItem, entity: &EntityRef) -> ActionItem {
    let mut bound = item.clone();
    let param = entity_param(entity);
    bound.url = with_param(&item.url, &[], "entity", Some(&param));
    bound.vals.insert("_entity".to_string(), param);
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

#[derive(Clone, Debug, Serialize)]
pub(crate) struct DashboardView {
    pub(crate) shell: ShellView,
    pub(crate) tiles: Vec<SlotView>,
    pub(crate) attention: Vec<SlotView>,
    pub(crate) panels: Vec<SlotView>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct PageView {
    pub(crate) shell: ShellView,
    pub(crate) module: String,
    pub(crate) title: String,
    pub(crate) subtitle: String,
    /// `stack`, `tiles`, `g-21` or `g-2`.
    pub(crate) layout: &'static str,
    pub(crate) head_actions: Vec<ActionItem>,
    /// Layout columns in declaration order, each a list of panels.
    pub(crate) columns: Vec<Vec<SlotView>>,
    /// Layout ids naming no contribution of this module.
    pub(crate) missing: Vec<String>,
}

pub(crate) fn layout_name(layout: &controlplane::Layout) -> &'static str {
    match layout {
        controlplane::Layout::Stack(_) => "stack",
        controlplane::Layout::Tiles(_) => "tiles",
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
/// the attention host, or inline in the entity header, a tab or the sidebar.
pub(crate) fn mode_of(point: &Point, in_tiles: bool) -> &'static str {
    match point {
        Point::DashboardTile => "tile",
        Point::DashboardAttention => "attention",
        Point::EntityHeader(_) => "header",
        Point::EntitySummary(_) => "fact",
        Point::EntityTabCount { .. } => "count",
        Point::NavCounter => "nav",
        Point::EntityListColumn { .. } => "inline",
        Point::Page(_) if in_tiles => "tile",
        Point::Page(_) | Point::DashboardPanel | Point::EntityTab { .. } => "panel",
    }
}

pub(crate) struct PanelInput<'a> {
    pub(crate) module: &'a str,
    pub(crate) contribution: &'a str,
    pub(crate) title: &'a str,
    pub(crate) icon: &'a str,
    pub(crate) mode: &'static str,
    /// `ok`, `empty`, `stale`, `error`, `readonly` or `denied`.
    pub(crate) state: &'static str,
    pub(crate) path: &'a str,
    /// The request's query (entity first), cursors included.
    pub(crate) query: &'a [(String, String)],
    pub(crate) error: Option<&'a str>,
    pub(crate) age_s: Option<u64>,
    pub(crate) min_role: &'static str,
    pub(crate) fallback_title: &'a str,
    pub(crate) primitive: Option<&'a Primitive>,
    /// The module's actions, unbound, for work-queue items.
    pub(crate) actions: &'a [ActionItem],
    pub(crate) row_menu_kinds: &'a [String],
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct PanelRender {
    pub(crate) module: String,
    pub(crate) contribution: String,
    pub(crate) title: String,
    pub(crate) icon: String,
    pub(crate) mode: &'static str,
    pub(crate) state: &'static str,
    pub(crate) url: String,
    pub(crate) error: Option<String>,
    pub(crate) age: Option<String>,
    /// The KPI tile's stale badge: `"3m"`.
    pub(crate) age_short: Option<String>,
    pub(crate) min_role: &'static str,
    pub(crate) fallback_title: String,
    pub(crate) flush: bool,
    pub(crate) body: Option<PrimView>,
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
        input.query.iter().filter(|(k, _)| k != "cursor" && k != "before").cloned().collect();
    let ctx = Ctx { path: input.path, query: &paging, params: input.query, input };
    let body = input.primitive.map(|p| prim(p, &ctx));
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
        state: input.state,
        url: with_param(input.path, input.query, "", None),
        error: input.error.map(str::to_string),
        age: input.age_s.map(age_text),
        age_short: input.age_s.map(|s| age_text(s).replace(" min", "m").replace(' ', "")),
        min_role: input.min_role,
        fallback_title: input.fallback_title.to_string(),
        flush,
        body,
    }
}

struct Ctx<'a> {
    path: &'a str,
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
    Form(FormView),
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
        }),
        Primitive::ColumnCells(cells) => {
            PrimView::Props(PropsView { columns: 1, items: cells.iter().map(|(k, v)| (k.clone(), cell(v))).collect() })
        }
        Primitive::KpiTile(k) => PrimView::Kpi(kpi(k)),
        Primitive::Timeline(t) => PrimView::Timeline(timeline(t, ctx)),
        Primitive::Form(f) => PrimView::Form(form(f, &BTreeMap::new(), &BTreeMap::new())),
        Primitive::WorkQueue(q) => PrimView::Queue(queue(q, ctx)),
        Primitive::ServiceStatus(s) => PrimView::Services(services(&s.items)),
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
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct TableCell {
    pub(crate) cell: CellView,
    pub(crate) num: bool,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct RowView {
    pub(crate) cells: Vec<TableCell>,
    pub(crate) href: Option<String>,
    pub(crate) menu_url: Option<String>,
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
    pub(crate) toolbar: bool,
    pub(crate) search_placeholder: Option<String>,
    pub(crate) q: String,
    pub(crate) search_url: String,
    pub(crate) filters: Vec<FilterView>,
    pub(crate) foot: bool,
    pub(crate) total: String,
    pub(crate) size: u32,
    pub(crate) next_url: Option<String>,
}

fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn table(t: &controlplane::DataTable, ctx: &Ctx<'_>) -> TableView {
    let rows: Vec<RowView> = t
        .rows
        .iter()
        .map(|r| RowView {
            cells: r
                .cells
                .iter()
                .enumerate()
                .map(|(i, c)| TableCell { cell: cell(c), num: t.columns.get(i).is_some_and(|col| col.align == Align::Right) })
                .collect(),
            href: r.entity.as_ref().map(entity_url),
            menu_url: r
                .entity
                .as_ref()
                .filter(|e| ctx.input.row_menu_kinds.contains(&e.kind))
                .map(|e| format!("/admin/_menu/{}/{}", encode(&e.kind), encode(&e.id))),
        })
        .collect();
    let shown = rows.len() as u64;
    let total = match t.page.total {
        Some(total) => format!("{} of {}", thousands(shown), thousands(total)),
        None => format!("{} rows", thousands(shown)),
    };
    let filters = t
        .filters
        .iter()
        .map(|f| FilterView {
            label: f.label.clone(),
            value: ctx.param(&f.key).unwrap_or("any").to_string(),
        })
        .collect::<Vec<_>>();
    TableView {
        columns: t.columns.iter().map(|c| ColumnView { label: c.label.clone(), num: c.align == Align::Right }).collect(),
        row_menu: rows.iter().any(|r| r.menu_url.is_some()),
        rows,
        selectable: t.selectable,
        toolbar: !t.filters.is_empty() || t.search_placeholder.is_some(),
        search_placeholder: t.search_placeholder.clone(),
        q: ctx.param("q").unwrap_or_default().to_string(),
        search_url: with_param(ctx.path, ctx.query, "q", None),
        filters,
        foot: t.page.next_cursor.is_some() || t.page.total.is_some_and(|n| n > 0 || !t.rows.is_empty()),
        total,
        size: t.page.size,
        next_url: t.page.next_cursor.as_deref().map(|c| with_param(ctx.path, ctx.query, "cursor", Some(c))),
    }
}

// ---- PropertyList ----

#[derive(Clone, Debug, Serialize)]
pub(crate) struct PropsView {
    pub(crate) columns: u8,
    pub(crate) items: Vec<(String, CellView)>,
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

/// The delta's colour class: the value's sign gives the direction, `good` says whether
/// that direction is good.
fn delta_class(d: &controlplane::Delta) -> &'static str {
    let v = d.value.trim_start();
    let rising = v.starts_with(['▲', '+', '↑']);
    let falling = v.starts_with(['▼', '-', '−', '↓']);
    match (d.good, rising, falling) {
        (Trend::Up, true, _) | (Trend::Down, _, true) => "up",
        (Trend::Down, true, _) => "bad-up",
        (Trend::Up, _, true) => "down",
        _ => "flat",
    }
}

/// The mockup's `spark` path (`bundle.js:170-175`).
fn spark(values: &[f64]) -> Option<SparkView> {
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
    Some(SparkView { line, area, color: "var(--chart-1)" })
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
        spark: spark(&k.series),
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

/// `("2026-10-20", "Oct 20", "17:44")` from an RFC 3339 UTC timestamp.
fn day_and_time(at: &str) -> (String, String, String) {
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
            }),
            detail: e.detail.clone(),
        };
        match days.last_mut() {
            Some((k, day)) if *k == key => day.items.push(item),
            _ => days.push((key, TimelineDay { label, items: vec![item] })),
        }
    }
    TimelineView {
        days: days.into_iter().map(|(_, d)| d).collect(),
        older_url: t.cursor_before.as_deref().map(|c| with_param(ctx.path, ctx.query, "before", Some(c))),
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
    pub(crate) ref_icon: &'static str,
    pub(crate) ref_kind: String,
    pub(crate) error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct FormView {
    pub(crate) fields: Vec<FieldView>,
    pub(crate) hidden: Vec<(String, String)>,
    pub(crate) submit_label: String,
    pub(crate) note: Option<String>,
    pub(crate) revision: Option<u64>,
}

pub(crate) fn currency_key(key: &str) -> String {
    format!("{key}_currency")
}

fn field(f: &Field, posted: &BTreeMap<String, String>, errors: &BTreeMap<String, String>) -> FieldView {
    let value = posted.get(&f.key).cloned().or_else(|| f.default.clone()).unwrap_or_default();
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
        selected: value.split(',').filter(|s| !s.is_empty()).map(str::to_string).collect(),
        currency: posted.get(&currency_key(&f.key)).cloned().unwrap_or_default(),
        value,
        options,
        ref_icon: if matches!(ref_kind.as_str(), "player" | "user") { "user" } else { "hash" },
        ref_kind,
        error: errors.get(&f.key).cloned(),
    }
}

pub(crate) fn form(
    spec: &FormSpec,
    posted: &BTreeMap<String, String>,
    errors: &BTreeMap<String, String>,
) -> FormView {
    FormView {
        fields: spec.fields.iter().map(|f| field(f, posted, errors)).collect(),
        hidden: spec.hidden.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        submit_label: spec.submit_label.clone(),
        note: spec.note.clone(),
        revision: spec.revision,
    }
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
    pub(crate) state: CellView,
    pub(crate) attempts: Vec<&'static str>,
    pub(crate) attempts_title: String,
    pub(crate) next_attempt: Option<String>,
    pub(crate) last_error: Option<String>,
    pub(crate) error_color: &'static str,
    pub(crate) payload: Option<String>,
    pub(crate) actions: Vec<ActionItem>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct QueueView {
    pub(crate) states: Vec<QueueStateView>,
    pub(crate) items: Vec<QueueItemView>,
    pub(crate) has_entity: bool,
    pub(crate) has_payload: bool,
}

/// Squares drawn for the attempts bar when `max_attempts` is larger.
const ATTEMPT_SQUARES_CAP: u32 = 20;

fn queue(q: &controlplane::WorkQueue, ctx: &Ctx<'_>) -> QueueView {
    let items: Vec<QueueItemView> = q
        .items
        .iter()
        .map(|it| {
            let state_tone = status_tone(&it.state);
            let squares = it.max_attempts.max(it.attempts).min(ATTEMPT_SQUARES_CAP);
            let attempts = (0..squares)
                .map(|i| match (i < it.attempts, state_tone == "ok" && i + 1 == it.attempts) {
                    (true, true) => "ok",
                    (true, false) => "f",
                    (false, _) => "",
                })
                .collect();
            let actions = it
                .actions
                .iter()
                .filter_map(|id| ctx.input.actions.iter().find(|a| &a.id == id))
                .map(|a| match &it.entity {
                    Some(e) => for_entity(a, e),
                    None => a.clone(),
                })
                .collect();
            QueueItemView {
                id: it.id.clone(),
                entity: it.entity.as_ref().map(ref_cell),
                state: CellView {
                    kind: "badge",
                    value: it.state.clone(),
                    tone: state_tone,
                    href: None,
                    color: "",
                    initials: String::new(),
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
    QueueView {
        states: q
            .states
            .iter()
            .map(|s| QueueStateView {
                label: s.label.clone(),
                count: thousands(s.count),
                tone: s.tone.map(tone).unwrap_or(""),
                on: s.key == q.selected,
                url: with_param(ctx.path, ctx.query, "state", Some(&s.key)),
            })
            .collect(),
        has_entity: items.iter().any(|i| i.entity.is_some()),
        has_payload: items.iter().any(|i| i.payload.is_some()),
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
    pub(crate) p99: String,
    pub(crate) note: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ServicesView {
    pub(crate) items: Vec<ServiceView>,
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

pub(crate) fn services(items: &[controlplane::ServiceItem]) -> ServicesView {
    ServicesView {
        items: items
            .iter()
            .map(|s| {
                let (state, tone, _) = service_state(s.state);
                ServiceView {
                    name: s.name.clone(),
                    state: state.to_string(),
                    running: s.state == ServiceState::Running,
                    tone,
                    bars: s.history.iter().map(|(_, st)| service_state(*st).2).collect(),
                    p99: s.p99_ms.map(|ms| format!("{ms} ms")).unwrap_or_else(|| "—".to_string()),
                    note: s.note.clone(),
                }
            })
            .collect(),
    }
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
    pub(crate) color: &'static str,
    pub(crate) initials: String,
}

// ---------------------------------------------------------------------------
// Modals
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ModalView {
    pub(crate) label: String,
    pub(crate) post_url: String,
    pub(crate) csrf: String,
    pub(crate) entity: Option<String>,
    /// `form`, `confirm` or `invoke`.
    pub(crate) kind: &'static str,
    /// The `modal-icon` class.
    pub(crate) tone: &'static str,
    pub(crate) icon: String,
    /// `danger` or `primary`.
    pub(crate) go: &'static str,
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) impact: Vec<(String, String)>,
    /// `required`, `optional` or `none`.
    pub(crate) reason: &'static str,
    pub(crate) reason_value: String,
    pub(crate) reason_error: Option<String>,
    pub(crate) type_to_confirm: Option<String>,
    pub(crate) confirm_label: String,
    pub(crate) form: Option<FormView>,
    pub(crate) banner: Option<String>,
}

/// The modal of `decl`, re-filled from `posted` with `errors` when it is shown again.
pub(crate) fn modal(
    decl: &controlplane::ActionDecl,
    post_url: String,
    csrf: String,
    entity: Option<&EntityRef>,
    posted: &BTreeMap<String, String>,
    errors: &BTreeMap<String, String>,
) -> ModalView {
    let mut view = ModalView {
        label: decl.label.clone(),
        post_url,
        csrf,
        entity: entity.map(entity_param),
        kind: "invoke",
        tone: "info",
        icon: if decl.icon.is_empty() { "info".to_string() } else { decl.icon.clone() },
        go: if decl.danger { "danger" } else { "primary" },
        title: decl.label.clone(),
        body: String::new(),
        impact: Vec::new(),
        reason: "none",
        reason_value: posted.get("_reason").cloned().unwrap_or_default(),
        reason_error: errors.get("_reason").cloned(),
        type_to_confirm: None,
        confirm_label: decl.label.clone(),
        form: None,
        banner: None,
    };
    match &decl.then {
        controlplane::Then::Form(spec) => {
            view.kind = "form";
            view.tone = "ok";
            view.go = "primary";
            view.form = Some(form(spec, posted, errors));
        }
        controlplane::Then::Confirm(spec) => {
            view.kind = "confirm";
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
            view.impact = spec.impact.clone();
            view.reason = match spec.reason {
                ReasonReq::Required => "required",
                ReasonReq::Optional => "optional",
                ReasonReq::None => "none",
            };
            view.type_to_confirm = spec.type_to_confirm.clone();
            view.confirm_label = spec.confirm_label.clone();
        }
        controlplane::Then::Invoke | controlplane::Then::Link(_) => {}
    }
    let shown: Vec<&str> = view.form.iter().flat_map(|f| f.fields.iter().map(|x| x.key.as_str())).collect();
    let stray: Vec<String> = errors
        .iter()
        .filter(|(k, _)| !shown.contains(&k.as_str()) && !(k.as_str() == "_reason" && view.reason != "none"))
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
}
