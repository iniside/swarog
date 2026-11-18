use std::collections::BTreeMap;
use controlplane::{EntityRef, PanelBody, Point, Primitive, Severity};
use serde::Serialize;
use crate::engine::columns::{ColumnOutcome, ListColumn};
use crate::engine::panels::{self, LastValue, PanelState};
use super::*;

// ---------------------------------------------------------------------------
// Panels
// ---------------------------------------------------------------------------

/// How a contribution's fragment is drawn: inside panel chrome, as a KPI tile, as rows of
/// the attention host, or inline in the entity header, a tab or the sidebar. A `column`
/// has no fragment of its own: the panel route refuses it, and its table draws its cells.
pub(crate) fn mode_of(point: &Point, in_tiles: bool) -> &'static str {
    seen::record(point);
    match point {
        Point::DashboardTile => "tile",
        Point::DashboardAttention => "attention",
        Point::EntityHeader(_) => "header",
        Point::EntitySummary(_) => "fact",
        Point::EntityTabCount { .. } => "count",
        Point::EntityTabTile { .. } => "tile",
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
    /// `ContributionDecl::readonly`.
    pub(crate) readonly: Option<&'a str>,
    pub(crate) body: Option<&'a PanelBody>,
    /// This contribution's `PanelTool`, `QueueRow` and `TableRow` actions, unbound.
    pub(crate) actions: &'a [ActionItem],
    /// A table's `EntityListColumn` columns for this page, in contribution order.
    pub(crate) list_columns: &'a [ListColumn],
    pub(crate) row_menu_kinds: &'a [String],
    /// The entity kinds with an `EntityHeader` contribution: a table row of one opens its
    /// entity page.
    pub(crate) header_kinds: &'a [String],
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
    pub(crate) foot: Option<String>,
    pub(crate) tag: Option<String>,
    pub(crate) banner: Option<BannerView>,
    /// The read-only banner's text; the template's module default when absent.
    pub(crate) readonly_text: Option<String>,
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
        foot: input.body.and_then(|b| b.foot.clone()),
        tag: input.body.and_then(|b| b.tag.clone()),
        readonly_text: input.readonly.map(str::to_string),
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

pub(super) struct Ctx<'a> {
    pub(super) path: &'a str,
    /// The request's query without its paging keys, so a tab or search URL built from it
    /// starts the result set over; the pager adds its own key back.
    pub(super) query: &'a [(String, String)],
    params: &'a [(String, String)],
    pub(super) input: &'a PanelInput<'a>,
}

impl Ctx<'_> {
    pub(super) fn param(&self, key: &str) -> Option<&str> {
        self.params.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type")]
pub(crate) enum PrimView {
    Table(TableView),
    Props(PropsView),
    Kpi(KpiView),
    Kpis { tiles: Vec<KpiView> },
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

fn severity(s: Severity) -> &'static str {
    seen::record(&s);
    match s {
        Severity::Err => "danger",
        Severity::Warn => "warn",
    }
}

fn prim(p: &Primitive, ctx: &Ctx<'_>) -> PrimView {
    seen::record(p);
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
        Primitive::KpiTiles(tiles) => PrimView::Kpis { tiles: tiles.iter().map(kpi).collect() },
        Primitive::Timeline(t) => PrimView::Timeline(timeline(t, ctx)),
        Primitive::Form(f) => PrimView::Form(panel_form(f, ctx.input)),
        Primitive::WorkQueue(q) => PrimView::Queue(queue(q, ctx)),
        Primitive::ServiceStatus(s) => PrimView::Services(services(s)),
        Primitive::TimeSeriesChart(c) => PrimView::Chart(chart(c)),
        Primitive::AttentionItems(items) => PrimView::Attention(AttentionView {
            items: items
                .iter()
                .map(|a| AttentionRow {
                    tone: severity(a.severity),
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
            crumb: h.crumb.clone().unwrap_or_else(|| h.title.clone()),
            subtitle: h.subtitle.clone(),
            status: h.status.as_ref().map(cell),
            badges: h.badges.iter().map(cell).collect(),
            color: avatar_color(&h.avatar_seed),
            initials: initials(&h.title),
        }),
        Primitive::Empty { title, detail } => PrimView::Empty { title: title.clone(), detail: detail.clone() },
    }
}
