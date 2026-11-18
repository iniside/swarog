//! The `EntityLabel` pass: one per slot response, after its body and its columns are built.

use std::collections::{BTreeMap, BTreeSet};

use controlplane::{Cell, ContributionDecl, EntityRef, Link, Point, Primitive};

use super::columns;
use super::manifests::Snapshot;
use super::roles::{self, Operator};

/// Each labelled ref's text by `(kind, id)`; a drawn ref without one draws its plain id.
pub(crate) type Labels = BTreeMap<(String, String), String>;

/// The ref `c` draws as its raw id: its link names an entity whose id is its text.
pub(crate) fn drawn_ref(c: &Cell) -> Option<&EntityRef> {
    match &c.link {
        Some(Link::Entity(e)) if c.value == e.id => Some(e),
        _ => None,
    }
}

/// The distinct refs `body` draws, by kind, in first-drawn order: each cell, at any depth
/// of its `more`, that draws an entity as its raw id, each timeline target and each queue
/// item's entity.
pub(crate) fn refs(body: &Primitive) -> BTreeMap<String, Vec<String>> {
    let mut drawn: Vec<&EntityRef> = Vec::new();
    match body {
        Primitive::DataTable(table) => {
            table.rows.iter().flat_map(|r| &r.cells).for_each(|c| cell_refs(c, &mut drawn));
        }
        Primitive::PropertyList(list) => list.items.iter().for_each(|(_, c)| cell_refs(c, &mut drawn)),
        Primitive::Timeline(timeline) => {
            for entry in &timeline.entries {
                entry.actor.iter().chain(&entry.badges).for_each(|c| cell_refs(c, &mut drawn));
                drawn.extend(entry.target.as_ref());
            }
        }
        Primitive::WorkQueue(queue) => {
            for item in &queue.items {
                item.cells.iter().for_each(|c| cell_refs(c, &mut drawn));
                drawn.extend(item.entity.as_ref());
            }
        }
        Primitive::Cell(c) => cell_refs(c, &mut drawn),
        Primitive::EntityHeader(header) => {
            header.status.iter().chain(&header.badges).for_each(|c| cell_refs(c, &mut drawn));
        }
        Primitive::KpiTile(_)
        | Primitive::KpiTiles(_)
        | Primitive::Form(_)
        | Primitive::ServiceStatus(_)
        | Primitive::TimeSeriesChart(_)
        | Primitive::AttentionItems(_)
        | Primitive::ColumnCells(_)
        | Primitive::Empty { .. } => {}
    }
    let mut seen = BTreeSet::new();
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for e in drawn.into_iter().filter(|e| seen.insert((e.kind.as_str(), e.id.as_str()))) {
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
