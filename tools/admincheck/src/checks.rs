use std::collections::{BTreeMap, BTreeSet};

use checkmodules::ProcessControlPlane;
use controlplane::{ActionPoint, Layout, Manifest, Point, Then, SECTIONS};

/// The longest action or contribution id: a form instance id `<id>:<43-byte token>` then
/// stays within the 128-byte idempotency keys modules accept.
pub const MAX_ID_BYTES: usize = 64;

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
        panel_actions(m, &mut out);
        id_lengths(m, &mut out);
        form_specs(m, &mut out);
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
        ActionPoint::PageHead(_) | ActionPoint::PanelTool(_) | ActionPoint::PanelForm(_) => None,
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
        let primary: Vec<&str> = page
            .head_actions
            .iter()
            .filter(|id| m.actions.iter().any(|a| &a.id == *id && a.primary))
            .map(String::as_str)
            .collect();
        if primary.len() > 1 {
            out.push(format!(
                "{}: page {:?} has more than one primary head action: {}",
                m.module,
                page.id,
                primary.join(", ")
            ));
        }
    }
}

fn panel_actions(m: &Manifest, out: &mut Vec<String>) {
    let mut forms: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for a in &m.actions {
        let (point, contribution) = match &a.point {
            ActionPoint::PanelTool(c) => ("PanelTool", c),
            ActionPoint::PanelForm(c) => ("PanelForm", c),
            ActionPoint::RowAction(_) | ActionPoint::HeaderAction(_) | ActionPoint::PageHead(_) => continue,
        };
        if !m.contributions.iter().any(|c| &c.id == contribution) {
            out.push(format!(
                "{}: action {:?} is {point}({contribution:?}), which is not a contribution of this module",
                m.module, a.id
            ));
        }
        if point == "PanelForm" {
            if !matches!(a.then, Then::Invoke) {
                out.push(format!("{}: PanelForm action {:?} is not Then::Invoke", m.module, a.id));
            }
            forms.entry(contribution.as_str()).or_default().push(a.id.as_str());
        }
    }
    for (contribution, actions) in forms.iter().filter(|(_, actions)| actions.len() > 1) {
        out.push(format!(
            "{}: contribution {contribution:?} has more than one PanelForm action: {}",
            m.module,
            actions.join(", ")
        ));
    }
}

fn id_lengths(m: &Manifest, out: &mut Vec<String>) {
    let ids = m
        .actions
        .iter()
        .map(|a| ("action", &a.id))
        .chain(m.contributions.iter().map(|c| ("contribution", &c.id)));
    for (what, id) in ids.filter(|(_, id)| id.len() > MAX_ID_BYTES) {
        out.push(format!("{}: {what} id {id:?} is longer than {MAX_ID_BYTES} bytes", m.module));
    }
}

fn form_specs(m: &Manifest, out: &mut Vec<String>) {
    for a in &m.actions {
        if let Then::Form(spec) = &a.then {
            for error in controlplane::form_spec_errors(spec) {
                out.push(format!("{}: action {:?} form: {error}", m.module, a.id));
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

fn section_merge(manifests: &[Manifest], out: &mut Vec<String>) {
    let merge = controlplane::merge_sections(manifests);
    let winners: BTreeMap<&str, &controlplane::MergedSection> =
        merge.sections.iter().map(|m| (m.section.id.as_str(), m)).collect();
    for lost in &merge.lost {
        let s = lost.declared;
        let Some(winner) = winners.get(s.id.as_str()) else { continue };
        let (label, order) = (&winner.section.label, winner.section.order);
        if (label, order) != (&s.label, s.order) {
            let by = winner.owner.unwrap_or("controlplane::SECTIONS");
            out.push(format!(
                "{}: section {:?} is declared as ({:?}, {}) but {by} declares it as ({label:?}, {order})",
                lost.module, s.id, s.label, s.order
            ));
        }
    }
}

/// The split process that dials every peer's control-plane face.
pub const PORTAL_PROCESS: &str = "admin-svc";

/// `characters-svc` → `characters`, the peer name admin-svc dials it by.
fn peer_of(process: &str) -> &str {
    process.strip_suffix("-svc").unwrap_or(process)
}

/// Every finding about where providers are hosted and dialled across the topologies.
pub fn peer_findings(monolith: &[Manifest], split: &[ProcessControlPlane]) -> Vec<String> {
    let mut out = Vec::new();
    match split.iter().find(|p| p.process == PORTAL_PROCESS) {
        Some(portal) => dialled_peers(portal, split, &mut out),
        None => out.push(format!("no split process is named {PORTAL_PROCESS}, so no dial list is checked")),
    }

    let mut hosts: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for p in split {
        for m in &p.manifests {
            hosts.entry(m.module.as_str()).or_default().insert(p.process);
        }
    }
    for (module, processes) in hosts.iter().filter(|(_, processes)| processes.len() > 1) {
        let processes: Vec<&str> = processes.iter().copied().collect();
        out.push(format!("{module}: hosted by more than one split process: {}", processes.join(", ")));
    }

    let in_monolith: BTreeSet<&str> = monolith.iter().map(|m| m.module.as_str()).collect();
    for module in in_monolith.iter().filter(|m| !hosts.contains_key(*m)) {
        out.push(format!("{module}: the monolith hosts this provider but no split process does"));
    }
    for module in hosts.keys().filter(|m| !in_monolith.contains(*m)) {
        out.push(format!("{module}: a split process hosts this provider but the monolith does not"));
    }
    out
}

fn dialled_peers(portal: &ProcessControlPlane, split: &[ProcessControlPlane], out: &mut Vec<String>) {
    // admin-svc's dial list is hand-written, so it is checked against the hosts the split builds.
    let dialled: BTreeSet<&str> = portal.dialled.iter().map(String::as_str).collect();
    for p in split.iter().filter(|p| p.process != PORTAL_PROCESS && !p.manifests.is_empty()) {
        if !dialled.contains(peer_of(p.process)) {
            let mut modules: Vec<&str> = p.manifests.iter().map(|m| m.module.as_str()).collect();
            modules.sort_unstable();
            out.push(format!(
                "{}: hosts provider(s) {} but {PORTAL_PROCESS} does not dial peer {:?}",
                p.process,
                modules.join(", "),
                peer_of(p.process)
            ));
        }
    }
    for peer in dialled {
        if !split.iter().any(|p| peer_of(p.process) == peer) {
            out.push(format!("{PORTAL_PROCESS} dials peer {peer:?}, but no split process is {peer}-svc"));
        }
    }
}
