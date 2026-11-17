//! Panel resolution: one provider call under its deadline, mapped to a panel state.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use controlplane::{Banner, ContributionDecl, PanelBody, PanelQuery, Point, Primitive, TimeRange, DEFAULT_PANEL_DEADLINE_MS};

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

/// One resolved panel; `body` is set for ok, readonly, stale and a provider's empty
/// payload.
#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct PanelView {
    pub(crate) module: String,
    pub(crate) contribution: String,
    pub(crate) title: String,
    pub(crate) state: PanelState,
    pub(crate) body: Option<PanelBody>,
    pub(crate) error: Option<String>,
    /// Seconds since a stale panel's cached copy was fetched.
    pub(crate) age_s: Option<u64>,
}

/// The module's banner, shown only in `ok` and `readonly`; every other state shows the
/// portal's own banner.
pub(crate) fn module_banner(state: PanelState, banner: Option<&Banner>) -> Option<&Banner> {
    banner.filter(|_| matches!(state, PanelState::Ok | PanelState::Readonly))
}

/// Whether `point` is on the dashboard, whose requests carry the time range.
pub(crate) fn is_dashboard(point: &Point) -> bool {
    match point {
        Point::DashboardTile | Point::DashboardAttention | Point::DashboardPanel | Point::DashboardWide => true,
        Point::Page(_)
        | Point::EntityHeader(_)
        | Point::EntityTab { .. }
        | Point::EntityTabCount { .. }
        | Point::EntitySummary(_)
        | Point::EntityListColumn { .. }
        | Point::NavCounter => false,
    }
}

/// The dashboard range switch's query key.
pub(crate) const QUERY_RANGE: &str = "range";
/// The range a dashboard request without `range` asks for.
pub(crate) const DEFAULT_RANGE: TimeRange = TimeRange::H24;
/// Every range in switch order, with its query value and label.
pub(crate) const RANGES: [(TimeRange, &str, &str); 3] =
    [(TimeRange::H1, "1h", "1 h"), (TimeRange::H24, "24h", "24 h"), (TimeRange::D7, "7d", "7 d")];

pub(crate) fn range_key(range: TimeRange) -> &'static str {
    RANGES.iter().find(|(r, _, _)| *r == range).map_or("", |(_, key, _)| key)
}

/// A request's range: absent is [`DEFAULT_RANGE`], an unknown value is `Err` (a 400).
pub(crate) fn parse_range(raw: Option<&str>) -> Result<TimeRange, String> {
    match raw {
        None => Ok(DEFAULT_RANGE),
        Some(raw) => RANGES
            .iter()
            .find(|(_, key, _)| *key == raw)
            .map(|(range, _, _)| *range)
            .ok_or_else(|| format!("unknown {QUERY_RANGE} {raw:?}")),
    }
}

/// Last-good panels for contributions that declare `cache_ttl_s`.
#[derive(Default)]
pub(crate) struct PanelCache {
    entries: HashMap<String, (PanelBody, Instant, Instant)>,
}

impl PanelCache {
    /// The cached copy and its age.
    fn get(&self, key: &str) -> Option<(PanelBody, Duration)> {
        let (body, fetched, expires) = self.entries.get(key)?;
        let now = Instant::now();
        (now < *expires).then(|| (body.clone(), now.saturating_duration_since(*fetched)))
    }

    fn put(&mut self, key: String, body: PanelBody, ttl: Duration) {
        let now = Instant::now();
        if self.entries.len() >= PANEL_CACHE_CAP && !self.entries.contains_key(&key) {
            self.entries.retain(|_, (_, _, expires)| *expires > now);
            if self.entries.len() >= PANEL_CACHE_CAP {
                if let Some(oldest) =
                    self.entries.iter().min_by_key(|(_, (_, _, expires))| *expires).map(|(k, _)| k.clone())
                {
                    self.entries.remove(&oldest);
                }
            }
        }
        self.entries.insert(key, (body, now, now + ttl));
    }
}

/// Every part is length-prefixed and every optional field tagged, so distinct queries
/// (an absent field and `Some("")` included) never share a key.
fn cache_key(req: &PanelQuery) -> String {
    let mut parts: Vec<String> = vec![req.module.clone(), req.contribution.clone()];
    match &req.entity {
        Some(entity) => parts.extend(["e".to_string(), entity.kind.clone(), entity.id.clone()]),
        None => parts.push("-".to_string()),
    }
    for field in [&req.search, &req.cursor, &req.before, &req.queue_state] {
        parts.push(match field {
            Some(value) => format!("+{value}"),
            None => "-".to_string(),
        });
    }
    parts.push(match req.range {
        Some(range) => format!("+{}", range_key(range)),
        None => "-".to_string(),
    });
    for (k, v) in &req.params {
        parts.extend([k.clone(), v.clone()]);
    }
    let mut key = String::new();
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
    req: PanelQuery,
) -> PanelView {
    let mut view = PanelView {
        module: module.to_string(),
        contribution: decl.id.clone(),
        title: decl.title.clone(),
        state: PanelState::Error,
        body: None,
        error: None,
        age_s: None,
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
        Ok(Ok(body)) => {
            if is_empty(&body.primitive) {
                view.state = PanelState::Empty;
                view.body = Some(body);
                return view;
            }
            if let Some(ttl) = decl.cache_ttl_s {
                engine
                    .panel_cache
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .put(key, body.clone(), Duration::from_secs(u64::from(ttl)));
            }
            view.state = if writes { PanelState::Ok } else { PanelState::Readonly };
            view.body = Some(body);
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
    view.age_s = cached.as_ref().map(|(_, age)| age.as_secs());
    view.body = cached.map(|(body, _)| body);
    view.error = Some(failure);
    view
}
