//! `eventctl seed-mockup`: the admin mockup's designed Event plane state on a dev
//! fleet. A fresh fleet has no paused and no lagging subscription, so this upserts two
//! subscription rows that no process registers: the pull worker claims only the ids
//! its own process registered (`asyncevents::worker::deliver_one_on` selects
//! `WHERE subscription_id = $1` for each entry of the `Plane::start` snapshot), so
//! nothing ever delivers, pauses or advances these rows. Running `resume`, `skip` or
//! `retire` on them is harmless, and the next `seed-mockup` resets them again.
//!
//! Both rows read the topic with the most events in the log from its first event (the
//! genesis cursor), so the paused row's poison panel shows a real event and the
//! lagging row's lag is the topic's whole history. Like any live subscription they
//! hold the topic's retention floor until retired or wiped.
//!
//! The SQL lives here, not in `asyncevents::admin`: these are demo fixtures, not
//! operator operations.

use anyhow::{bail, Context, Result};
use sqlx::PgPool;

/// The paused row: the mockup's `inventory.on_purchase.v1` row and Paused panel.
pub const PAUSED_ID: &str = "seed.mockup-paused.v1";

/// The lagging row: the mockup's `wallet.on_match.v1` row.
pub const LAGGING_ID: &str = "seed.mockup-lagging.v1";

/// "Consumer failed 5× on the same event" (`UILayout/redesign/assets/pages-platform.js:146`).
const PAUSED_FAILURES: i32 = 5;

/// The paused row's `poison.err` (`UILayout/redesign/assets/data.js:145`).
const PAUSED_ERROR: &str = r#"decode: unknown currency code "TOK2" at $.amount.currency"#;

/// Any failure count above zero turns an active row's state badge warn, as the
/// mockup's `lagging` badge is. The mockup does not show the count.
const LAGGING_FAILURES: i32 = 3;

/// Why the mockup's lagging row lags: "wallet down" (`pages-platform.js:136`), worded
/// as the economy page's stale banner words it (`pages-platform.js:167`). A worker never
/// leaves a failing row without an error, so neither does the seed.
const LAGGING_ERROR: &str = "wallet-svc is unavailable";

/// Sets every column, so a run resets a row from any state, retired included. The
/// cursor is the genesis position, which sorts before every real event, and
/// `spec_hash` / `start_kind` are what `asyncevents` writes for a genesis
/// subscription (`SubEntry::spec_hash` in `core/asyncevents/src/transport.rs`).
const UPSERT: &str = "\
INSERT INTO asyncevents.subscriptions \
    (subscription_id, topic, contract_version, state, \
     cursor_generation, cursor_xid, cursor_tie, next_attempt_at, consecutive_failures, \
     last_error, spec_hash, start_kind, updated_at) \
VALUES ($1, $2, $3, $4, 0, '0'::xid8, 0, NULL, $5, $6, $7, 'genesis', now()) \
ON CONFLICT (subscription_id) DO UPDATE SET \
    topic = EXCLUDED.topic, contract_version = EXCLUDED.contract_version, \
    state = EXCLUDED.state, cursor_generation = EXCLUDED.cursor_generation, \
    cursor_xid = EXCLUDED.cursor_xid, cursor_tie = EXCLUDED.cursor_tie, \
    next_attempt_at = EXCLUDED.next_attempt_at, \
    consecutive_failures = EXCLUDED.consecutive_failures, \
    last_error = EXCLUDED.last_error, spec_hash = EXCLUDED.spec_hash, \
    start_kind = EXCLUDED.start_kind, updated_at = EXCLUDED.updated_at";

/// The stream both seeded rows read.
pub struct Seeded {
    pub topic: String,
    pub version: i32,
    pub events: i64,
}

/// Upserts both rows in one transaction onto the `(topic, contract_version)` stream
/// with the most events (ties broken by name). Lag and the poison event match on the
/// version too, so the version is chosen with the topic. Errors on an empty log.
pub async fn seed_mockup(pool: &PgPool) -> Result<Seeded> {
    let mut tx = pool.begin().await.context("eventctl: begin seed-mockup tx")?;
    let busiest: Option<(String, i32, i64)> = sqlx::query_as(
        "SELECT topic, contract_version, count(*) FROM asyncevents.events \
         GROUP BY topic, contract_version \
         ORDER BY count(*) DESC, topic, contract_version \
         LIMIT 1",
    )
    .fetch_optional(&mut *tx)
    .await
    .context("eventctl: find the busiest topic")?;
    let Some((topic, version, events)) = busiest else {
        bail!("eventctl: seed-mockup: the event log is empty; run demoseed first");
    };
    let spec_hash = format!("{topic}|v{version}|genesis");
    let rows = [
        (PAUSED_ID, "paused", PAUSED_FAILURES, PAUSED_ERROR),
        (LAGGING_ID, "active", LAGGING_FAILURES, LAGGING_ERROR),
    ];
    for (id, state, failures, error) in rows {
        sqlx::query(UPSERT)
            .bind(id)
            .bind(&topic)
            .bind(version)
            .bind(state)
            .bind(failures)
            .bind(error)
            .bind(&spec_hash)
            .execute(&mut *tx)
            .await
            .with_context(|| format!("eventctl: upsert {id}"))?;
    }
    tx.commit().await.context("eventctl: commit seed-mockup tx")?;
    Ok(Seeded { topic, version, events })
}
