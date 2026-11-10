//! The manifest cache: every source refreshed in one wave, last-good kept per source, and
//! the derived routing table, sidebar sections and nav entries.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use controlplane::{ControlPlane, Manifest, NavSection, Source};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::Engine;

pub(crate) const REFRESH_INTERVAL: Duration = Duration::from_secs(5);
pub(crate) const SOURCE_TIMEOUT: Duration = Duration::from_secs(6);
/// Grace for the refresh task to observe the stop signal before `Admin::stop` aborts it.
pub(crate) const STOP_GRACE: Duration = Duration::from_secs(2);

/// The section an entry naming no known section is placed in.
pub(crate) const FALLBACK_SECTION: &str = "platform";

/// Which source serves a module, and that module's manifest.
pub(crate) struct Routed {
    pub(crate) source: usize,
    pub(crate) manifest: Manifest,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct NavItem {
    pub(crate) module: String,
    pub(crate) section: String,
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) icon: String,
    pub(crate) order: i32,
    pub(crate) page: String,
    pub(crate) counter: Option<String>,
    /// The serving source answered the latest refresh wave.
    pub(crate) source_ok: bool,
}

/// One immutable view of every source's last-good manifests.
pub(crate) struct Snapshot {
    pub(crate) sources: Vec<Source>,
    last_good: Vec<Option<Vec<Manifest>>>,
    healthy: Vec<bool>,
    pub(crate) modules: BTreeMap<String, Routed>,
    pub(crate) sections: Vec<NavSection>,
    pub(crate) nav: Vec<NavItem>,
}

impl Snapshot {
    pub(crate) fn empty() -> Snapshot {
        Snapshot::build(Vec::new(), Vec::new(), Vec::new())
    }

    /// `sources` with only `sources[0]`, the local source, holding manifests (`local`);
    /// no peer manifest is fetched yet.
    pub(crate) fn seeded(sources: Vec<Source>, local: Option<Vec<Manifest>>) -> Snapshot {
        let n = sources.len();
        let mut last_good = vec![None; n];
        let mut healthy = vec![false; n];
        if n > 0 {
            healthy[0] = local.is_some();
            last_good[0] = local;
        }
        Snapshot::build(sources, last_good, healthy)
    }

    fn build(sources: Vec<Source>, last_good: Vec<Option<Vec<Manifest>>>, healthy: Vec<bool>) -> Snapshot {
        let mut modules: BTreeMap<String, Routed> = BTreeMap::new();
        for (idx, manifests) in last_good.iter().enumerate() {
            for manifest in manifests.iter().flatten() {
                if let Some(first) = modules.get(&manifest.module) {
                    tracing::warn!(
                        module = %manifest.module,
                        served_by = %sources[first.source].peer,
                        ignored = %sources[idx].peer,
                        "admin: two control-plane sources declare the same module; the first source serves it"
                    );
                    continue;
                }
                modules.insert(
                    manifest.module.clone(),
                    Routed { source: idx, manifest: manifest.clone() },
                );
            }
        }
        let sections: Vec<NavSection> = controlplane::merge_sections(modules.values().map(|r| &r.manifest))
            .sections
            .into_iter()
            .map(|merged| merged.section)
            .collect();
        let rank: HashMap<&str, usize> =
            sections.iter().enumerate().map(|(i, s)| (s.id.as_str(), i)).collect();
        let mut nav: Vec<NavItem> = Vec::new();
        for routed in modules.values() {
            for entry in &routed.manifest.entries {
                nav.push(NavItem {
                    module: routed.manifest.module.clone(),
                    section: resolve_section(&sections, &entry.section).to_string(),
                    id: entry.id.clone(),
                    label: entry.label.clone(),
                    icon: entry.icon.clone(),
                    order: entry.order,
                    page: entry.page.clone(),
                    counter: entry.counter.clone(),
                    source_ok: healthy[routed.source],
                });
            }
        }
        nav.sort_by(|a, b| {
            (rank[a.section.as_str()], a.order, &a.module, &a.id)
                .cmp(&(rank[b.section.as_str()], b.order, &b.module, &b.id))
        });
        Snapshot { sources, last_good, healthy, modules, sections, nav }
    }

    /// The module's manifest and the face that serves it.
    pub(crate) fn route(&self, module: &str) -> Option<(&Manifest, Arc<dyn ControlPlane>)> {
        let routed = self.modules.get(module)?;
        Some((&routed.manifest, self.sources[routed.source].face.clone()))
    }
}

pub(crate) fn resolve_section<'a>(sections: &[NavSection], id: &'a str) -> &'a str {
    if sections.iter().any(|s| s.id == id) {
        id
    } else {
        FALLBACK_SECTION
    }
}

/// One wave: every source fetched concurrently, each under [`SOURCE_TIMEOUT`].
pub(crate) async fn refresh_once(engine: &Engine) {
    let current = engine.snapshot();
    let mut fetched: Vec<Option<Vec<Manifest>>> = vec![None; current.sources.len()];
    let mut set: tokio::task::JoinSet<(usize, Option<Vec<Manifest>>)> = tokio::task::JoinSet::new();
    let mut ids: HashMap<tokio::task::Id, usize> = HashMap::new();
    for (idx, source) in current.sources.iter().enumerate() {
        let face = source.face.clone();
        let peer = source.peer.clone();
        let handle = set.spawn(async move {
            match tokio::time::timeout(SOURCE_TIMEOUT, face.manifests()).await {
                // An empty reply is a peer with no providers, which every edge process
                // answers uniformly; it is a successful fetch, never a failure.
                Ok(Ok(manifests)) => (idx, Some(manifests)),
                Ok(Err(error)) => {
                    tracing::debug!(%peer, %error, "admin: cp.manifests failed; keeping last-good");
                    (idx, None)
                }
                Err(_) => {
                    tracing::debug!(%peer, "admin: cp.manifests timed out; keeping last-good");
                    (idx, None)
                }
            }
        });
        ids.insert(handle.id(), idx);
    }
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((idx, manifests)) => fetched[idx] = manifests,
            Err(error) => {
                let peer = ids
                    .get(&error.id())
                    .map(|i| current.sources[*i].peer.as_str())
                    .unwrap_or("<unknown>");
                tracing::error!(%peer, %error, "admin: manifest fetch task failed; keeping last-good");
            }
        }
    }
    let healthy: Vec<bool> = fetched.iter().map(Option::is_some).collect();
    // A source that misses one wave keeps serving its last-good manifest, so a peer
    // restart or a slow wave does not blank its nav and pages.
    let last_good: Vec<Option<Vec<Manifest>>> = fetched
        .into_iter()
        .zip(current.last_good.iter())
        .map(|(new, prev)| new.or_else(|| prev.clone()))
        .collect();
    if last_good == current.last_good && healthy == current.healthy {
        return;
    }
    engine.install(Snapshot::build(current.sources.clone(), last_good, healthy));
}

/// Runs a wave immediately, then every [`REFRESH_INTERVAL`] until `stop_rx` fires.
pub(crate) fn spawn_refresh(engine: Arc<Engine>, mut stop_rx: watch::Receiver<bool>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(REFRESH_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                _ = stop_rx.changed() => break,
                _ = ticker.tick() => {}
            }
            refresh_once(&engine).await;
        }
    })
}
