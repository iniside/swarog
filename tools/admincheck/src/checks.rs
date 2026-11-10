use std::collections::{BTreeMap, BTreeSet};

use controlplane::{ActionPoint, Layout, Manifest, Point, SECTIONS};

/// Every finding over the union of `manifests`, each one line naming its module.
pub fn findings(manifests: &[Manifest]) -> Vec<String> {
    let mut out = Vec::new();
    duplicate_modules(manifests, &mut out);
    for m in manifests {
        unique_ids(m, &mut out);
    }
    entity_kinds(manifests, &mut out);
    for m in manifests {
        layouts(m, &mut out);
        head_actions(m, &mut out);
        nav_entries(m, &mut out);
    }
    nav_sections(manifests, &mut out);
    section_merge(manifests, &mut out);
    out
}

fn duplicate_modules(manifests: &[Manifest], out: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    for m in manifests {
        if !seen.insert(m.module.as_str()) {
            out.push(format!("{}: two providers declare this module", m.module));
        }
    }
}

fn unique_ids(m: &Manifest, out: &mut Vec<String>) {
    let mut check = |what: &str, ids: Vec<&str>| {
        let mut seen = BTreeSet::new();
        for id in ids {
            if !seen.insert(id) {
                out.push(format!("{}: duplicate {what} id {id:?}", m.module));
            }
        }
    };
    check("contribution", m.contributions.iter().map(|c| c.id.as_str()).collect());
    check("action", m.actions.iter().map(|a| a.id.as_str()).collect());
    check("page", m.pages.iter().map(|p| p.id.as_str()).collect());
    check("nav entry", m.entries.iter().map(|e| e.id.as_str()).collect());
    check("search kind", m.search.iter().map(|s| s.kind.as_str()).collect());
    check("entity kind", m.entity_kinds.iter().map(|k| k.kind.as_str()).collect());
}

fn point_kind(point: &Point) -> Option<&str> {
    match point {
        Point::EntityHeader(kind)
        | Point::EntitySummary(kind)
        | Point::EntityTab { kind, .. }
        | Point::EntityTabCount { kind, .. }
        | Point::EntityListColumn { kind, .. } => Some(kind),
        Point::Page(_)
        | Point::DashboardTile
        | Point::DashboardAttention
        | Point::DashboardPanel
        | Point::NavCounter => None,
    }
}

fn action_kind(point: &ActionPoint) -> Option<&str> {
    match point {
        ActionPoint::RowAction(kind) | ActionPoint::HeaderAction(kind) => Some(kind),
        ActionPoint::PageHead(_) | ActionPoint::PanelTool(_) => None,
    }
}

fn entity_kinds(manifests: &[Manifest], out: &mut Vec<String>) {
    let declared: BTreeSet<&str> = manifests
        .iter()
        .flat_map(|m| m.entity_kinds.iter().map(|k| k.kind.as_str()))
        .collect();
    for m in manifests {
        for c in &m.contributions {
            if let Some(kind) = point_kind(&c.point).filter(|k| !declared.contains(k)) {
                out.push(format!(
                    "{}: contribution {:?} targets entity kind {kind:?}, which no manifest declares",
                    m.module, c.id
                ));
            }
        }
        for a in &m.actions {
            if let Some(kind) = action_kind(&a.point).filter(|k| !declared.contains(k)) {
                out.push(format!(
                    "{}: action {:?} targets entity kind {kind:?}, which no manifest declares",
                    m.module, a.id
                ));
            }
        }
    }
}

fn layouts(m: &Manifest, out: &mut Vec<String>) {
    for page in &m.pages {
        let ids: Vec<&String> = match &page.layout {
            Layout::Stack(ids) | Layout::Tiles(ids) => ids.iter().collect(),
            Layout::Grid21(a, b) | Layout::Grid2(a, b) => a.iter().chain(b).collect(),
        };
        for id in ids {
            let on_page = m
                .contributions
                .iter()
                .any(|c| &c.id == id && matches!(&c.point, Point::Page(p) if *p == page.id));
            if !on_page {
                out.push(format!(
                    "{}: page {:?} lays out {id:?}, which is not a contribution with Point::Page({:?})",
                    m.module, page.id, page.id
                ));
            }
        }
    }
}

fn head_actions(m: &Manifest, out: &mut Vec<String>) {
    for page in &m.pages {
        for id in &page.head_actions {
            if !m.actions.iter().any(|a| &a.id == id) {
                out.push(format!(
                    "{}: page {:?} names head action {id:?}, which is not an action of this module",
                    m.module, page.id
                ));
            }
        }
    }
}

fn nav_entries(m: &Manifest, out: &mut Vec<String>) {
    for entry in &m.entries {
        if !m.pages.iter().any(|p| p.id == entry.page) {
            out.push(format!(
                "{}: nav entry {:?} opens page {:?}, which this module does not declare",
                m.module, entry.id, entry.page
            ));
        }
        if let Some(counter) = &entry.counter {
            let is_counter = m
                .contributions
                .iter()
                .any(|c| &c.id == counter && c.point == Point::NavCounter);
            if !is_counter {
                out.push(format!(
                    "{}: nav entry {:?} names counter {counter:?}, which is not a Point::NavCounter contribution",
                    m.module, entry.id
                ));
            }
        }
    }
}

fn nav_sections(manifests: &[Manifest], out: &mut Vec<String>) {
    let known: BTreeSet<&str> = SECTIONS
        .iter()
        .map(|(id, _, _)| *id)
        .chain(manifests.iter().flat_map(|m| m.sections.iter().map(|s| s.id.as_str())))
        .collect();
    for m in manifests {
        for entry in &m.entries {
            if !known.contains(entry.section.as_str()) {
                out.push(format!(
                    "{}: nav entry {:?} names section {:?}, which is neither fixed nor declared",
                    m.module, entry.id, entry.section
                ));
            }
        }
    }
}

/// Mirrors the portal's merge: a fixed section wins, then the declaration of the
/// lexicographically smallest module (its first, on a repeat within that module).
fn section_merge(manifests: &[Manifest], out: &mut Vec<String>) {
    let mut declared: Vec<(&str, &controlplane::NavSection)> = manifests
        .iter()
        .flat_map(|m| m.sections.iter().map(move |s| (m.module.as_str(), s)))
        .collect();
    declared.sort_by(|a, b| a.0.cmp(b.0));
    let mut winners: BTreeMap<&str, (&str, &str, i32)> = SECTIONS
        .iter()
        .map(|(id, label, order)| (*id, ("controlplane::SECTIONS", *label, *order)))
        .collect();
    for (module, s) in declared {
        let winner = *winners
            .entry(s.id.as_str())
            .or_insert((module, s.label.as_str(), s.order));
        let (by, label, order) = winner;
        if (label, order) != (s.label.as_str(), s.order) {
            out.push(format!(
                "{module}: section {:?} is declared as ({:?}, {}) but {by} declares it as ({label:?}, {order})",
                s.id, s.label, s.order
            ));
        }
    }
}
