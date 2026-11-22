//! The order the portal draws the points several modules contribute to, read through the
//! portal's own merge and sort, so a tool holding every real manifest can pin it.

use std::collections::BTreeSet;
use std::sync::Arc;

use controlplane::{ActionPoint, LocalFace, Manifest, Point, Source};

use crate::engine::manifests::Snapshot;

/// One `(point, module, id, order)` per nav entry, contribution and action the portal
/// draws at a point several modules may share, grouped by point and in the order the
/// portal draws them within it: the nav entries first, then the contribution points, then
/// the action points, each by point name. A module's own page or panel and a kind's label
/// have no shared order and are not listed.
pub fn drawn_orders(manifests: &[Manifest]) -> Vec<(String, String, String, i32)> {
    let local = Source { peer: "local".to_string(), face: Arc::new(LocalFace::new(Vec::new())) };
    let snapshot = Snapshot::seeded(vec![local], Some(manifests.to_vec()));
    let mut out: Vec<(String, String, String, i32)> = snapshot
        .nav
        .iter()
        .map(|n| (format!("nav {}", n.section), n.module.clone(), n.id.clone(), n.order))
        .collect();
    let points: BTreeSet<String> =
        snapshot.contributions(|p| point_key(p).is_some()).iter().filter_map(|(_, c)| point_key(&c.point)).collect();
    for key in points {
        for (m, c) in snapshot.contributions(|p| point_key(p).as_ref() == Some(&key)) {
            out.push((key.clone(), m.to_string(), c.id.clone(), c.order));
        }
    }
    let points: BTreeSet<String> =
        snapshot.actions(|p| action_key(p).is_some()).iter().filter_map(|(_, _, a)| action_key(&a.point)).collect();
    for key in points {
        for (m, _, a) in snapshot.actions(|p| action_key(p).as_ref() == Some(&key)) {
            out.push((key.clone(), m.to_string(), a.id.clone(), a.order));
        }
    }
    out
}

/// The shared place a contribution is drawn in, as the portal selects it; `None` for one
/// with no shared order.
fn point_key(point: &Point) -> Option<String> {
    match point {
        Point::DashboardTile => Some("dashboard_tile".to_string()),
        Point::DashboardAttention => Some("dashboard_attention".to_string()),
        Point::DashboardPanel => Some("dashboard_panel".to_string()),
        Point::DashboardWide => Some("dashboard_wide".to_string()),
        Point::EntityHeader(kind) => Some(format!("entity_header {kind}")),
        Point::EntitySummary(kind) => Some(format!("entity_summary {kind}")),
        Point::EntityTab { kind, .. } => Some(format!("entity_tab {kind}")),
        Point::EntityTabCount { kind, tab } => Some(format!("entity_tab_count {kind}/{tab}")),
        Point::EntityTabTile { kind, tab } => Some(format!("entity_tab_tile {kind}/{tab}")),
        Point::EntityListColumn { kind, .. } => Some(format!("entity_list_column {kind}")),
        // A module's own page, the label only its kind's owner answers and the counter a
        // nav entry names.
        Point::Page(_) | Point::EntityLabel { .. } | Point::NavCounter => None,
    }
}

/// The shared place an action is drawn in; `None` for one placed on its own module's page
/// or panel.
fn action_key(point: &ActionPoint) -> Option<String> {
    match point {
        ActionPoint::HeaderAction(kind) => Some(format!("header_action {kind}")),
        ActionPoint::RowAction(kind) => Some(format!("row_action {kind}")),
        ActionPoint::PageHead(_)
        | ActionPoint::PanelTool(_)
        | ActionPoint::QueueRow(_)
        | ActionPoint::PanelForm(_)
        | ActionPoint::TableRow(_) => None,
    }
}
