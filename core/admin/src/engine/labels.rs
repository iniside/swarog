//! The `EntityLabel` pass: one per slot response, after its body and its columns are built.

use std::collections::{BTreeMap, BTreeSet};

use controlplane::{ContributionDecl, EntityRef, Link, Point, Primitive};

use super::columns;
use super::manifests::Snapshot;
use super::roles::{self, Operator};

/// Each labelled ref's text by `(kind, id)`; a drawn ref without one draws its plain id.
pub(crate) type Labels = BTreeMap<(String, String), String>;

/// The distinct refs `body` draws, by kind, in first-drawn order: each table row's entity,
/// and each timeline entry's actor where its link names an entity, then its target.
pub(crate) fn refs(body: &Primitive) -> BTreeMap<String, Vec<String>> {
    let drawn: Vec<&EntityRef> = match body {
        Primitive::DataTable(table) => table.rows.iter().filter_map(|r| r.entity.as_ref()).collect(),
        Primitive::Timeline(timeline) => timeline
            .entries
            .iter()
            .flat_map(|entry| {
                let actor = match entry.actor.as_ref().and_then(|a| a.link.as_ref()) {
                    Some(Link::Entity(e)) => Some(e),
                    _ => None,
                };
                actor.into_iter().chain(entry.target.as_ref())
            })
            .collect(),
        Primitive::PropertyList(_)
        | Primitive::KpiTile(_)
        | Primitive::KpiTiles(_)
        | Primitive::Form(_)
        | Primitive::WorkQueue(_)
        | Primitive::ServiceStatus(_)
        | Primitive::TimeSeriesChart(_)
        | Primitive::AttentionItems(_)
        | Primitive::ColumnCells(_)
        | Primitive::Cell(_)
        | Primitive::EntityHeader(_)
        | Primitive::Empty { .. } => Vec::new(),
    };
    let mut seen = BTreeSet::new();
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for e in drawn.into_iter().filter(|e| seen.insert((e.kind.as_str(), e.id.as_str()))) {
        out.entry(e.kind.clone()).or_default().push(e.id.clone());
    }
    out
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
    let (module, routed) =
        snapshot.modules.iter().find(|(_, r)| r.manifest.entity_kinds.iter().any(|k| k.kind == kind))?;
    let decl = routed
        .manifest
        .contributions
        .iter()
        .filter(|c| matches!(&c.point, Point::EntityLabel { kind: k } if k == kind))
        .min_by(|a, b| (a.order, &a.id).cmp(&(b.order, &b.id)))?;
    Some((module.as_str(), decl))
}
