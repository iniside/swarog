//! The `EntityLabel` pass: one per slot response, after its body and its columns are built.

use std::collections::{BTreeMap, BTreeSet};

use controlplane::{Cell, ContributionDecl, EntityRef, Link, Point, Primitive};

use super::columns;
use super::manifests::Snapshot;
use super::roles::{self, Operator};

/// Each labelled ref's text by `(kind, id)`; a drawn ref without one draws its plain id.
pub(crate) type Labels = BTreeMap<(String, String), String>;

/// The ref `c` draws as its raw id: its link or its label names an entity whose id is its
/// text.
pub(crate) fn drawn_ref(c: &Cell) -> Option<&EntityRef> {
    let e = match (&c.link, &c.label) {
        (Some(Link::Entity(e)), _) | (None, Some(e)) => e,
        _ => return None,
    };
    (c.value == e.id).then_some(e)
}

/// One thing `body` draws, in drawn order: a cell (without flattening its `more`) or a bare
/// entity ref.
enum Drawn<'a> {
    Cell(&'a Cell),
    Ref(&'a EntityRef),
}

/// Every cell and bare entity ref `body` draws, in drawn order; a column answer is no body.
fn drawn(body: &Primitive) -> Vec<Drawn<'_>> {
    let mut out = Vec::new();
    match body {
        Primitive::DataTable(table) => out.extend(table.rows.iter().flat_map(|r| &r.cells).map(Drawn::Cell)),
        Primitive::PropertyList(list) => out.extend(list.items.iter().map(|(_, c)| Drawn::Cell(c))),
        Primitive::Timeline(timeline) => {
            for entry in &timeline.entries {
                out.extend(entry.actor.iter().chain(&entry.badges).map(Drawn::Cell));
                out.extend(entry.target.iter().map(Drawn::Ref));
            }
        }
        Primitive::WorkQueue(queue) => {
            for item in &queue.items {
                out.extend(item.cells.iter().map(Drawn::Cell));
                out.extend(item.entity.iter().map(Drawn::Ref));
            }
        }
        Primitive::Cell(c) => out.push(Drawn::Cell(c)),
        Primitive::EntityHeader(header) => out.extend(header.status.iter().chain(&header.badges).map(Drawn::Cell)),
        Primitive::KpiTile(_)
        | Primitive::KpiTiles(_)
        | Primitive::Form(_)
        | Primitive::ServiceStatus(_)
        | Primitive::TimeSeriesChart(_)
        | Primitive::AttentionItems(_)
        | Primitive::ColumnCells(_)
        | Primitive::Empty { .. } => {}
    }
    out
}

/// Whether `c`, or a cell at any depth of its `more`, holds both a `link` and a `label`.
pub(crate) fn links_and_labels(c: &Cell) -> bool {
    (c.link.is_some() && c.label.is_some()) || c.more.iter().any(links_and_labels)
}

/// Why a provider's answer `body`, a panel's or a list column's, cannot be drawn: a cell
/// holding both a `link` and a `label`.
pub(crate) fn body_error(body: &Primitive) -> Option<String> {
    let refused = match body {
        Primitive::ColumnCells(cells) => cells.values().any(links_and_labels),
        _ => drawn(body).into_iter().any(|d| matches!(d, Drawn::Cell(c) if links_and_labels(c))),
    };
    refused.then(|| "answered a cell with both a link and a label".to_string())}

/// The distinct refs `body` draws, by kind, in first-drawn order: each cell, at any depth
/// of its `more`, that draws an entity as its raw id, each timeline target and each queue
/// item's entity.
pub(crate) fn refs(body: &Primitive) -> BTreeMap<String, Vec<String>> {
    let mut refs: Vec<&EntityRef> = Vec::new();
    for d in drawn(body) {
        match d {
            Drawn::Cell(c) => cell_refs(c, &mut refs),
            Drawn::Ref(e) => refs.push(e),
        }
    }
    let mut seen = BTreeSet::new();
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for e in refs.into_iter().filter(|e| seen.insert((e.kind.as_str(), e.id.as_str()))) {
        out.entry(e.kind.clone()).or_default().push(e.id.clone());
    }
    out
}

fn cell_refs<'a>(c: &'a Cell, out: &mut Vec<&'a EntityRef>) {
    out.extend(drawn_ref(c));
    c.more.iter().for_each(|m| cell_refs(m, out));
}

/// The labels of the refs `body` draws: per kind, every chunk of one call to the kind's
/// owner's `EntityLabel`, as a list column is asked. A kind without one, a label above the
/// operator's role, and a call that fails, times out or answers other than `ColumnCells`
/// label nothing of that kind.
pub(crate) async fn pass(snapshot: &Snapshot, operator: &Operator, body: &Primitive) -> Labels {
    let calls = refs(body).into_iter().filter_map(|(kind, ids)| {
        let (module, decl) = owned_label(snapshot, &kind).filter(|(_, d)| roles::allows(operator, d.min_role))?;
        Some(async move {
            match columns::cells(snapshot, module, decl, &ids).await {
                Ok(cells) => cells.into_iter().map(|(id, cell)| ((kind.clone(), id), cell.value)).collect(),
                Err(why) => {
                    tracing::warn!(module = %module, contribution = %decl.id, %why, "admin: an entity label failed");
                    Vec::new()
                }
            }
        })
    });
    futures::future::join_all(calls).await.into_iter().flatten().collect()
}

/// The `EntityLabel` of `kind`, first by `(order, id)`, of the module whose manifest declares
/// `kind`; one any other module declares is never asked.
fn owned_label<'a>(snapshot: &'a Snapshot, kind: &str) -> Option<(&'a str, &'a ContributionDecl)> {
    let (module, _) = snapshot.kind_owner(kind)?;
    let (manifest, _) = snapshot.route(module)?;
    let decl = manifest
        .contributions
        .iter()
        .filter(|c| matches!(&c.point, Point::EntityLabel { kind: k } if k == kind))
        .min_by(|a, b| (a.order, &a.id).cmp(&(b.order, &b.id)))?;
    Some((module, decl))
}
