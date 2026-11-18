use controlplane::EntityRef;
use serde::Serialize;
use crate::engine::panels;
use super::*;

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
    pub(crate) extra: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct FlashView {
    pub(crate) message: String,
    pub(crate) reveal: Option<RevealView>,
    /// `ok` or `info`.
    pub(crate) tone: &'static str,
}

pub(crate) fn reveal(r: &controlplane::SecretReveal) -> RevealView {
    RevealView {
        label: r.label.clone(),
        value: r.value.clone(),
        hint: r.hint.clone(),
        expires_at: r.expires_at.clone(),
        extra: r.extra.clone(),
    }
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
        .map(|(range, key, label)| {
            seen::record(range);
            RangeView { label, href: format!("/admin?{}={key}", panels::QUERY_RANGE), on: *range == selected }
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
    pub(crate) rows: Vec<LayoutRow>,
    /// Tile and layout ids naming no contribution of this module.
    pub(crate) missing: Vec<String>,
}

/// One row of a page layout.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct LayoutRow {
    /// `stack`, `g-21` or `g-2`.
    pub(crate) layout: &'static str,
    /// Columns in declaration order, each a list of panels.
    pub(crate) columns: Vec<Vec<SlotView>>,
}

/// The layout's rows top to bottom, each its name and its columns of contribution ids in
/// declaration order.
pub(crate) fn layout_rows(layout: &controlplane::Layout) -> Vec<(&'static str, Vec<&[String]>)> {
    seen::record(layout);
    match layout {
        controlplane::Layout::Stack(a) => vec![("stack", vec![a])],
        controlplane::Layout::Grid21(a, b) => vec![("g-21", vec![a, b])],
        controlplane::Layout::Grid2(a, b) => vec![("g-2", vec![a, b])],
        controlplane::Layout::Rows(rows) => rows.iter().flat_map(layout_rows).collect(),
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct TabView {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) url: String,
    pub(crate) counts: Vec<SlotView>,
    /// A module contributing the tab missed the latest manifest refresh.
    pub(crate) down: bool,
    /// A contribution of the tab declares `readonly`.
    pub(crate) readonly: bool,
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
    /// The selected tab's `Point::EntityTabTile` contributions, one row above its panels.
    pub(crate) tab_tiles: Vec<SlotView>,
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
