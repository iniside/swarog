//! The `EntityListColumn` fan-out: one call per column for one rendered table page.

use std::collections::BTreeMap;

use controlplane::{Align, Cell, DataTable, PanelQuery, Point, Primitive, MAX_PANEL_REFS};

use super::manifests::Snapshot;
use super::panels;
use super::roles::{self, Operator};

/// One contributed column of a table page: its cells by entity id, or why it has none.
#[derive(Clone, Debug)]
pub(crate) struct ListColumn {
    pub(crate) module: String,
    pub(crate) label: String,
    pub(crate) align: Align,
    pub(crate) outcome: ColumnOutcome,
}

#[derive(Clone, Debug)]
pub(crate) enum ColumnOutcome {
    Cells(BTreeMap<String, Cell>),
    /// An error, a deadline or an answer that is not `ColumnCells`; drawn as a marker in
    /// every cell and one warn banner.
    Failed(String),
}

/// The distinct ids of `table`'s rows whose entity is of `kind`, in first-row order.
pub(crate) fn refs(table: &DataTable, kind: &str) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    let ids = table.rows.iter().filter_map(|r| r.entity.as_ref()).filter(|e| e.kind == kind);
    ids.filter(|e| seen.insert(e.id.as_str())).map(|e| e.id.clone()).collect()
}

/// Calls every `EntityListColumn` of `kind` concurrently, each column under its own deadline, with
/// `refs` split into `PanelQuery::refs` chunks of at most `MAX_PANEL_REFS`. Every chunk of
/// every column runs concurrently; a chunk that fails fails its whole column. A column below
/// the operator's role makes no call and is absent from the result; no refs make no call at
/// all.
pub(crate) async fn fan_out(
    snapshot: &Snapshot,
    operator: &Operator,
    kind: &str,
    refs: Vec<String>,
) -> Vec<ListColumn> {
    if refs.is_empty() {
        return Vec::new();
    }
    let decls = snapshot.contributions(|pt| matches!(pt, Point::EntityListColumn { kind: k, .. } if k == kind));
    let allowed = decls.into_iter().filter(|(_, decl)| roles::allows(operator, decl.min_role));
    let calls = allowed.filter_map(|(module, decl)| {
        let Point::EntityListColumn { label, align, .. } = &decl.point else {
            return None;
        };
        let column = |outcome| ListColumn { module: module.to_string(), label: label.clone(), align: *align, outcome };
        let route = snapshot.route(module);
        let bound = panels::deadline(decl);
        let refs = &refs;
        Some(async move {
            let Some((_, face)) = route else {
                return column(ColumnOutcome::Failed(format!("no source serves module {module:?}")));
            };
            let chunks = refs.chunks(MAX_PANEL_REFS).map(|chunk| {
                let req = PanelQuery {
                    module: module.to_string(),
                    contribution: decl.id.clone(),
                    entity: None,
                    search: None,
                    cursor: None,
                    before: None,
                    queue_state: None,
                    range: None,
                    params: BTreeMap::new(),
                    refs: chunk.to_vec(),
                };
                let face = face.clone();
                async move {
                    match face.panel(req).await {
                        Ok(body) => match body.primitive {
                            Primitive::ColumnCells(cells) => Ok(cells),
                            _ => Err("answered a primitive other than ColumnCells".to_string()),
                        },
                        Err(error) => Err(error.to_string()),
                    }
                }
            });
            // One deadline over all chunks: the column is drawn only whole, so it is late
            // once any chunk is, and splitting must not stretch the decl's budget.
            let outcome = match tokio::time::timeout(bound, futures::future::try_join_all(chunks)).await {
                Ok(Ok(answers)) => ColumnOutcome::Cells(answers.into_iter().flatten().collect()),
                Ok(Err(why)) => ColumnOutcome::Failed(why),
                Err(_) => ColumnOutcome::Failed(format!("deadline of {}ms elapsed", bound.as_millis())),
            };
            if let ColumnOutcome::Failed(why) = &outcome {
                tracing::warn!(module = %module, contribution = %decl.id, %why, "admin: a list column failed");
            }
            column(outcome)
        })
    });
    futures::future::join_all(calls).await
}
