//! Deterministic stand-ins for mockup data the backend does not store. Every value is a
//! constant from `UILayout/redesign/assets/data.js` / `registry.js`, or a pure function of
//! an id; nothing reads a clock or a random source.

use controlplane::{fake_hash, AttentionItem, Link, ServiceItem, ServiceState, Severity};

// FAKE: the portal has no deployment name.
pub(crate) const DEPLOYMENT: &str = "emberline-prod";

// FAKE: no version or process-count source; the portal cannot see the fleet.
pub(crate) const DASHBOARD_SUBTITLE: &str = "emberline-prod · Swarog 0.42.1 · 20 processes (split topology)";

// FAKE: no health probe exists until Weles exposes its API.
const HEALTH_PROBE_S: u32 = 10;

/// The top-bar health popover's note.
pub(crate) fn health_note() -> String {
    format!("Probed every {HEALTH_PROBE_S} s by the portal · last probe 2 s ago")
}

/// The dashboard Service health panel's probe interval.
pub(crate) fn health_probe() -> String {
    format!("probed every {HEALTH_PROBE_S} s")
}

/// The number and width of a service's health history buckets.
const HISTORY_BUCKETS: u64 = 48;
const HISTORY_BUCKET_MIN: u64 = 5;

// FAKE: no health probe exists until Weles exposes its API.
pub(crate) fn services() -> Vec<ServiceItem> {
    use ServiceState::{Degraded, Down, Running};
    const RUNNING: &str = "6 d 4 h";
    let rows = [
        ("admin-svc", Running, "0.42.1", Some(RUNNING), "29 ms", None),
        ("gateway-svc", Running, "0.42.1", Some(RUNNING), "38 ms", None),
        ("accounts-svc", Running, "0.42.1", Some(RUNNING), "21 ms", None),
        ("characters-svc", Running, "0.42.1", Some(RUNNING), "17 ms", None),
        ("inventory-svc", Running, "0.42.1", Some(RUNNING), "24 ms", None),
        ("wallet-svc", Down, "0.42.1", None, "—", Some("connection refused · last ok 3 min ago")),
        ("friends-svc", Running, "0.42.1", Some(RUNNING), "12 ms", None),
        ("groups-svc", Running, "0.42.1", Some(RUNNING), "15 ms", None),
        ("party-svc", Running, "0.42.1", Some(RUNNING), "9 ms", None),
        ("notifications-svc", Running, "0.42.1", Some(RUNNING), "14 ms", None),
        ("mail-svc", Degraded, "0.42.1", Some(RUNNING), "880 ms", Some("SMTP relay throttling · 312 queued")),
        ("storage-svc", Running, "0.42.1", Some("2 d 1 h"), "61 ms", None),
        ("match-svc", Running, "0.42.1", Some(RUNNING), "19 ms", None),
        ("rating-svc", Running, "0.42.1", Some(RUNNING), "8 ms", None),
        ("leaderboard-svc", Running, "0.42.1", Some(RUNNING), "11 ms", None),
        ("config-svc", Running, "0.42.1", Some(RUNNING), "4 ms", None),
        ("scheduler-svc", Running, "0.42.1", Some(RUNNING), "6 ms", None),
        ("audit-svc", Running, "0.42.1", Some(RUNNING), "10 ms", None),
        ("apikeys-svc", Running, "0.42.1", Some(RUNNING), "5 ms", None),
        ("lore-svc", Running, "0.1.0", Some("1 d 2 h"), "33 ms", None),
    ];
    rows.into_iter()
        .map(|(name, state, version, uptime, value, note)| ServiceItem {
            name: name.to_string(),
            state,
            version: Some(version.to_string()),
            value: Some(value.to_string()),
            uptime: uptime.map(str::to_string),
            note: note.map(str::to_string),
            history: history(name, state),
        })
        .collect()
}

/// The span [`history`] covers: `"last 4 h"`.
pub(crate) fn history_window() -> String {
    format!("last {} h", HISTORY_BUCKETS * HISTORY_BUCKET_MIN / 60)
}

/// [`HISTORY_BUCKETS`] buckets of [`HISTORY_BUCKET_MIN`] minutes, oldest first, as the
/// mockup's `uptimeBar` draws them.
// FAKE: no health history is stored.
fn history(name: &str, now: ServiceState) -> Vec<(String, ServiceState)> {
    let seed = fake_hash("history", name);
    (0..HISTORY_BUCKETS)
        .map(|i| {
            let state = match now {
                ServiceState::Down if i > 41 => if i % 2 == 1 { ServiceState::Down } else { ServiceState::Degraded },
                ServiceState::Degraded if i > 43 => ServiceState::Degraded,
                _ if (seed.wrapping_mul(31).wrapping_add(i * 17)) % 97 == 3 => ServiceState::Degraded,
                _ => ServiceState::Running,
            };
            (format!("{} min ago", (HISTORY_BUCKETS - i) * HISTORY_BUCKET_MIN), state)
        })
        .collect()
}

fn page(module: &str, page: &str) -> Link {
    Link::Page { module: module.to_string(), page: page.to_string(), params: Default::default() }
}

// FAKE: no health probe exists until Weles exposes its API.
pub(crate) fn attention_services() -> Vec<AttentionItem> {
    vec![AttentionItem {
        severity: Severity::Err,
        title: "wallet-svc unavailable".to_string(),
        text: "7 restarts in 15 min; grants and ledger are offline.".to_string(),
        link: page("wallet", "economy"),
        action_label: "Open".to_string(),
        since: "3 min".to_string(),
    }]
}
