//! Panel resolution: one provider call under its deadline, mapped to a panel state.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use controlplane::{ContributionDecl, PanelRequest, Primitive, DEFAULT_PANEL_DEADLINE_MS};

use super::manifests::Snapshot;
use super::roles::{self, Operator};
use super::Engine;

/// Upper bound on cached last-good panels; the key space spans entities and params.
const PANEL_CACHE_CAP: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum PanelState {
    Ok,
    Empty,
    Stale,
    Error,
    Readonly,
    Denied,
}

impl PanelState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            PanelState::Ok => "ok",
            PanelState::Empty => "empty",
            PanelState::Stale => "stale",
            PanelState::Error => "error",
            PanelState::Readonly => "readonly",
            PanelState::Denied => "denied",
        }
    }
}

/// One resolved panel; `primitive` is set for ok, readonly and stale.
#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct PanelView {
    pub(crate) module: String,
    pub(crate) contribution: String,
    pub(crate) title: String,
    pub(crate) state: PanelState,
    pub(crate) primitive: Option<Primitive>,
    pub(crate) error: Option<String>,
}

/// Last-good panels for contributions that declare `cache_ttl_s`.
#[derive(Default)]
pub(crate) struct PanelCache {
    entries: HashMap<String, (Primitive, Instant)>,
}

impl PanelCache {
    fn get(&self, key: &str) -> Option<Primitive> {
        let (primitive, expires) = self.entries.get(key)?;
        (Instant::now() < *expires).then(|| primitive.clone())
    }

    fn put(&mut self, key: String, primitive: Primitive, ttl: Duration) {
        let now = Instant::now();
        if self.entries.len() >= PANEL_CACHE_CAP && !self.entries.contains_key(&key) {
            self.entries.retain(|_, (_, expires)| *expires > now);
            if self.entries.len() >= PANEL_CACHE_CAP {
                if let Some(oldest) =
                    self.entries.iter().min_by_key(|(_, (_, expires))| *expires).map(|(k, _)| k.clone())
                {
                    self.entries.remove(&oldest);
                }
            }
        }
        self.entries.insert(key, (primitive, now + ttl));
    }
}

fn cache_key(req: &PanelRequest) -> String {
    let mut parts: Vec<&str> = vec![&req.module, &req.contribution];
    if let Some(entity) = &req.entity {
        parts.extend([entity.kind.as_str(), entity.id.as_str()]);
    }
    let mut key = format!("{}/", parts.len());
    for (k, v) in &req.params {
        parts.extend([k.as_str(), v.as_str()]);
    }
    for part in parts {
        key.push_str(&format!("{}:{part}", part.len()));
    }
    key
}

pub(crate) fn deadline(decl: &ContributionDecl) -> Duration {
    let ms = if decl.deadline_ms == 0 { DEFAULT_PANEL_DEADLINE_MS } else { decl.deadline_ms };
    Duration::from_millis(u64::from(ms))
}

/// Whether a payload has zero items to show.
pub(crate) fn is_empty(primitive: &Primitive) -> bool {
    match primitive {
        Primitive::DataTable(t) => t.rows.is_empty(),
        Primitive::PropertyList(p) => p.items.is_empty(),
        Primitive::Timeline(t) => t.entries.is_empty(),
        Primitive::WorkQueue(q) => q.items.is_empty(),
        Primitive::ServiceStatus(s) => s.items.is_empty(),
        Primitive::TimeSeriesChart(c) => c.series.is_empty() || c.buckets.is_empty(),
        Primitive::AttentionItems(items) => items.is_empty(),
        Primitive::ColumnCells(cells) => cells.is_empty(),
        Primitive::Empty { .. } => true,
        Primitive::KpiTile(_) | Primitive::Form(_) | Primitive::Cell(_) | Primitive::EntityHeader(_) => false,
    }
}

/// Resolves `decl` of `module` for `operator`: denied without a call below `min_role`,
/// otherwise one call bounded by the contribution's deadline.
pub(crate) async fn resolve(
    engine: &Engine,
    snapshot: &Snapshot,
    operator: &Operator,
    module: &str,
    decl: &ContributionDecl,
    req: PanelRequest,
) -> PanelView {
    let mut view = PanelView {
        module: module.to_string(),
        contribution: decl.id.clone(),
        title: decl.title.clone(),
        state: PanelState::Error,
        primitive: None,
        error: None,
    };
    if !roles::allows(operator, decl.min_role) {
        view.state = PanelState::Denied;
        return view;
    }
    let Some((manifest, face)) = snapshot.route(module) else {
        view.error = Some(format!("no source serves module {module:?}"));
        return view;
    };
    let writes = manifest.writes;
    let key = cache_key(&req);
    let failure = match tokio::time::timeout(deadline(decl), face.panel(req)).await {
        Ok(Ok(primitive)) => {
            if is_empty(&primitive) {
                view.state = PanelState::Empty;
                return view;
            }
            if let Some(ttl) = decl.cache_ttl_s {
                engine
                    .panel_cache
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .put(key, primitive.clone(), Duration::from_secs(u64::from(ttl)));
            }
            view.state = if writes { PanelState::Ok } else { PanelState::Readonly };
            view.primitive = Some(primitive);
            return view;
        }
        Ok(Err(error)) if error.status == opsapi::Status::NotFound => {
            view.state = PanelState::Empty;
            return view;
        }
        Ok(Err(error)) => error.to_string(),
        Err(_) => format!("deadline of {}ms elapsed", deadline(decl).as_millis()),
    };
    let cached = decl
        .cache_ttl_s
        .and_then(|_| engine.panel_cache.lock().unwrap_or_else(|e| e.into_inner()).get(&key));
    view.state = if cached.is_some() { PanelState::Stale } else { PanelState::Error };
    view.primitive = cached;
    view.error = Some(failure);
    view
}
