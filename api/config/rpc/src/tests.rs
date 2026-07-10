//! Unit tests for the [`CachedConfig`] revision gate. No transport — a fake
//! [`configapi::ConfigSnapshot`] returns scripted [`configapi::Snapshot`]s so the test
//! drives `refresh` deterministically.

use std::sync::Mutex;

use super::*;
use async_trait::async_trait;
use configapi::{Setting, Snapshot};

/// A snapshot source whose next reply the test controls.
struct FakeSnapshot {
    next: Mutex<Snapshot>,
}

impl FakeSnapshot {
    fn new() -> Arc<FakeSnapshot> {
        Arc::new(FakeSnapshot {
            next: Mutex::new(Snapshot {
                revision: 0,
                settings: Vec::new(),
            }),
        })
    }
    fn set(&self, revision: i64, settings: Vec<Setting>) {
        *self.next.lock().unwrap() = Snapshot { revision, settings };
    }
}

#[async_trait]
impl configapi::ConfigSnapshot for FakeSnapshot {
    async fn snapshot(&self) -> Result<Snapshot, Error> {
        Ok(self.next.lock().unwrap().clone())
    }
}

fn setting(ns: &str, key: &str, value: &str) -> Setting {
    Setting {
        namespace: ns.to_string(),
        key: key.to_string(),
        value: value.to_string(),
    }
}

/// `refresh` applies a snapshot only when its revision is strictly newer than the one
/// held — a stale or duplicate revision is a no-op, so an out-of-order invalidation
/// NOTIFY never rolls the cache backwards.
#[tokio::test]
async fn refresh_applies_only_newer_revisions() {
    let source = FakeSnapshot::new();
    let cached = CachedConfig::new(source.clone() as Arc<dyn configapi::ConfigSnapshot>);

    // First refresh (revision 5) applies over the empty (-1) cache.
    source.set(5, vec![setting("game", "name", "v5")]);
    cached.refresh().await.unwrap();
    assert_eq!(cached.get("game", "name").as_deref(), Some("v5"));

    // A STALE snapshot (revision 3) is ignored despite different contents.
    source.set(3, vec![setting("game", "name", "v3")]);
    cached.refresh().await.unwrap();
    assert_eq!(cached.get("game", "name").as_deref(), Some("v5"), "stale revision must not apply");

    // A DUPLICATE (revision 5) is also a no-op.
    source.set(5, vec![setting("game", "name", "dup")]);
    cached.refresh().await.unwrap();
    assert_eq!(cached.get("game", "name").as_deref(), Some("v5"), "duplicate revision must not apply");

    // A newer revision applies and swaps the whole map (removed keys disappear).
    source.set(6, vec![setting("game", "region", "eu")]);
    cached.refresh().await.unwrap();
    assert_eq!(cached.get("game", "region").as_deref(), Some("eu"));
    assert_eq!(cached.get("game", "name"), None, "full-map swap drops removed keys");
}
