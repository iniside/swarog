//! Global search: every matching `SearchDecl` queried concurrently under its own deadline.

use std::time::Duration;

use controlplane::{SearchDecl, SearchHit, SearchRequest, DEFAULT_SEARCH_DEADLINE_MS};

use super::manifests::Snapshot;

/// Hits requested from each search source.
pub(crate) const HITS_PER_SOURCE: u32 = 8;
/// Byte cap on the query string before it is sent to any provider.
pub(crate) const MAX_QUERY_BYTES: usize = 256;

#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct SearchGroup {
    pub(crate) module: String,
    pub(crate) kind: String,
    pub(crate) label: String,
    pub(crate) icon: String,
    pub(crate) hits: Vec<SearchHit>,
    pub(crate) error: Option<String>,
}

pub(crate) fn deadline(decl: &SearchDecl) -> Duration {
    let ms = if decl.deadline_ms == 0 { DEFAULT_SEARCH_DEADLINE_MS } else { decl.deadline_ms };
    Duration::from_millis(u64::from(ms))
}

/// Queries every declared search source, or only those of `kind` when it is non-empty.
/// An empty `q` makes no call.
pub(crate) async fn run(snapshot: &Snapshot, q: &str, kind: &str) -> Vec<SearchGroup> {
    if q.trim().is_empty() {
        return Vec::new();
    }
    let mut calls = Vec::new();
    for (module, routed) in &snapshot.modules {
        for decl in &routed.manifest.search {
            if !kind.is_empty() && decl.kind != kind {
                continue;
            }
            let face = snapshot.sources[routed.source].face.clone();
            let req = SearchRequest {
                module: module.clone(),
                kind: decl.kind.clone(),
                q: q.to_string(),
                limit: HITS_PER_SOURCE,
            };
            let module = module.clone();
            let decl = decl.clone();
            calls.push(async move {
                let bound = deadline(&decl);
                let (hits, error) = match tokio::time::timeout(bound, face.search(req)).await {
                    Ok(Ok(mut hits)) => {
                        hits.truncate(HITS_PER_SOURCE as usize);
                        (hits, None)
                    }
                    Ok(Err(e)) => (Vec::new(), Some(e.to_string())),
                    Err(_) => (Vec::new(), Some(format!("deadline of {}ms elapsed", bound.as_millis()))),
                };
                SearchGroup { module, kind: decl.kind, label: decl.label, icon: decl.icon, hits, error }
            });
        }
    }
    futures::future::join_all(calls).await
}
