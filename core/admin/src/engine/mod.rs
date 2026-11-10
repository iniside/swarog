//! The control-plane portal engine: sources, the manifest cache, panel states, actions,
//! search and roles. Routes (`crate::routes`) render what it resolves.

pub(crate) mod actions;
pub(crate) mod manifests;
pub(crate) mod panels;
pub(crate) mod roles;
pub(crate) mod search;
pub(crate) mod sources;

use std::sync::{Arc, Mutex, RwLock};

/// Engine state shared by the router and the manifest refresh task.
pub(crate) struct Engine {
    snapshot: RwLock<Arc<manifests::Snapshot>>,
    panel_cache: Mutex<panels::PanelCache>,
}

impl Engine {
    pub(crate) fn new() -> Engine {
        Engine {
            snapshot: RwLock::new(Arc::new(manifests::Snapshot::empty())),
            panel_cache: Mutex::new(panels::PanelCache::default()),
        }
    }

    pub(crate) fn snapshot(&self) -> Arc<manifests::Snapshot> {
        self.snapshot.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub(crate) fn install(&self, snapshot: manifests::Snapshot) {
        *self.snapshot.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(snapshot);
    }
}
