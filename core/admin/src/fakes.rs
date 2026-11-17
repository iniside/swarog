//! Deterministic stand-ins for mockup data the backend does not store. Every value is a
//! constant from `UILayout/redesign/assets/data.js` / `registry.js`, or a pure function of
//! an id; nothing reads a clock or a random source.

use controlplane::{AttentionItem, Delta, KpiTile, Link, Palette, ServiceItem, ServiceState, Severity, Trend};

/// FNV-1a, 64-bit.
pub(crate) fn hash(id: &str) -> u64 {
    id.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3))
}

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
        ("admin-svc", Running, "0.42.1", Some(RUNNING), Some(29), None),
        ("gateway-svc", Running, "0.42.1", Some(RUNNING), Some(38), None),
        ("accounts-svc", Running, "0.42.1", Some(RUNNING), Some(21), None),
        ("characters-svc", Running, "0.42.1", Some(RUNNING), Some(17), None),
        ("inventory-svc", Running, "0.42.1", Some(RUNNING), Some(24), None),
        ("wallet-svc", Down, "0.42.1", None, None, Some("connection refused · last ok 3 min ago")),
        ("friends-svc", Running, "0.42.1", Some(RUNNING), Some(12), None),
        ("groups-svc", Running, "0.42.1", Some(RUNNING), Some(15), None),
        ("party-svc", Running, "0.42.1", Some(RUNNING), Some(9), None),
        ("notifications-svc", Running, "0.42.1", Some(RUNNING), Some(14), None),
        ("mail-svc", Degraded, "0.42.1", Some(RUNNING), Some(880), Some("SMTP relay throttling · 312 queued")),
        ("storage-svc", Running, "0.42.1", Some("2 d 1 h"), Some(61), None),
        ("match-svc", Running, "0.42.1", Some(RUNNING), Some(19), None),
        ("rating-svc", Running, "0.42.1", Some(RUNNING), Some(8), None),
        ("leaderboard-svc", Running, "0.42.1", Some(RUNNING), Some(11), None),
        ("config-svc", Running, "0.42.1", Some(RUNNING), Some(4), None),
        ("scheduler-svc", Running, "0.42.1", Some(RUNNING), Some(6), None),
        ("audit-svc", Running, "0.42.1", Some(RUNNING), Some(10), None),
        ("apikeys-svc", Running, "0.42.1", Some(RUNNING), Some(5), None),
        ("lore-svc", Running, "0.1.0", Some("1 d 2 h"), Some(33), None),
    ];
    rows.into_iter()
        .map(|(name, state, version, uptime, p99_ms, note)| ServiceItem {
            name: name.to_string(),
            state,
            version: Some(version.to_string()),
            p99_ms,
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
    let seed = hash(name);
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

/// The notification bell as `(tone, title, text, when)`.
// FAKE: no operator feed exists.
pub(crate) fn notifications() -> Vec<(&'static str, &'static str, &'static str, &'static str)> {
    vec![
        ("err", "wallet-svc is down", "Health probe failed 7× in 15 min.", "3 min"),
        ("err", "inventory.on_purchase.v1 paused", "Poison event at seq 12,905.", "4 h"),
        ("warn", "Lore store volume 66% full", "13.29 of 20 TB on lore-01.", "5 h"),
        ("warn", "SMTP relay throttling", "Retry backoff active.", "2 h"),
        ("muted", "Key ak_c4a8 created", "by marta.k · role ci-build", "6 h"),
    ]
}

fn delta(value: &str, period: &str, good: Trend) -> Option<Delta> {
    Some(Delta { value: value.to_string(), period: period.to_string(), good })
}

// FAKE: there is no Lore module.
pub(crate) fn lore_seats() -> KpiTile {
    KpiTile {
        label: "Lore seats".to_string(),
        value: "8".to_string(),
        unit: None,
        delta: delta("", "users with Lore access · 1 suspended", Trend::None),
        series: vec![5.0, 6.0, 6.0, 7.0, 8.0, 8.0],
        spark: Palette::Chart1,
        capacity: None,
        link: None,
    }
}

// FAKE: there is no Lore module.
pub(crate) fn lore_store() -> KpiTile {
    KpiTile {
        label: "Lore store used".to_string(),
        value: "13.29".to_string(),
        unit: Some("TB".to_string()),
        delta: delta("▲ 1.19 TB", "this month", Trend::Down),
        series: vec![6.2, 7.4, 8.9, 10.6, 12.1, 13.29],
        spark: Palette::Chart2,
        capacity: None,
        link: None,
    }
}

// FAKE: `match.matches` holds finished matches only.
pub(crate) fn matches_in_progress() -> KpiTile {
    KpiTile {
        label: "Matches in progress".to_string(),
        value: "1,204".to_string(),
        unit: None,
        delta: delta("— steady", "", Trend::None),
        series: vec![900.0, 1010.0, 1100.0, 1180.0, 1220.0, 1190.0, 1204.0],
        spark: Palette::Chart1,
        capacity: None,
        link: None,
    }
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

// FAKE: there is no Lore module.
pub(crate) fn attention_lore() -> Vec<AttentionItem> {
    vec![AttentionItem {
        severity: Severity::Warn,
        title: "2 stale Lore locks".to_string(),
        text: "Held > 14 days by sam.o (suspended) in ashfall-game, ashfall-art.".to_string(),
        link: page("lore", "locks"),
        action_label: "Review".to_string(),
        since: "14 d".to_string(),
    }]
}

// FAKE: the scheduler stores no run failures.
pub(crate) fn attention_scheduler() -> Vec<AttentionItem> {
    vec![AttentionItem {
        severity: Severity::Warn,
        title: "storage.gc failed at 03:30".to_string(),
        text: "timeout: storage-svc deadline exceeded.".to_string(),
        link: page("scheduler", "scheduler"),
        action_label: "Logs".to_string(),
        since: "03:30".to_string(),
    }]
}

// FAKE: the event log has an (generation, xid, tie) position, not a sequence number.
pub(crate) const HEAD_SEQUENCE: &str = "991,210";

// FAKE: no append rate is measured.
pub(crate) const HEAD_RATE: &str = "1.8 k events/min";

// FAKE: skipped events are not kept.
pub(crate) const DEAD_LETTERED: &str = "3";

/// A subscription checkpoint as one integer; a head is this plus the real lag.
// FAKE: the real position is (generation, xid, tie).
pub(crate) fn checkpoint(subscription: &str) -> u64 {
    hash(subscription) % 250_000
}

/// The subscription id's first dotted segment.
// FAKE: the consumer module is not stored.
pub(crate) fn consumer(subscription: &str) -> String {
    subscription.split('.').next().unwrap_or(subscription).to_string()
}

// FAKE: the handler name is not stored.
pub(crate) fn handler(subscription: &str) -> String {
    const HANDLERS: [&str; 4] = ["on_event", "apply", "project", "handle"];
    HANDLERS[(hash(subscription) % HANDLERS.len() as u64) as usize].to_string()
}
