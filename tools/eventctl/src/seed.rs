//! `eventctl seed-mockup`: the admin mockup's designed Event plane state on a dev
//! fleet. A fresh fleet has no paused and no lagging subscription, so this upserts two
//! subscription rows that no process registers: the pull worker claims only the ids
//! its own process registered (`asyncevents::worker::deliver_one_on` selects
//! `WHERE subscription_id = $1` for each entry of the `Plane::start` snapshot), so
//! nothing ever delivers, pauses or advances these rows. Running `resume`, `skip` or
//! `retire` on them is harmless, and the next `seed-mockup` resets them again.
//!
//! Both rows read the `(topic, contract_version)` stream with the most events in the
//! log. The lagging row reads it from its first event (the genesis cursor), so its lag
//! counts every event of that stream still retained at seed time. The paused row's
//! cursor sits on the event just before the stream's middle event, so the poison panel
//! shows that middle event and the paused row's lag is strictly below the lagging
//! row's, as in the mockup (lag 118 against 273, `UILayout/redesign/assets/data.js`),
//! which makes the Max lag tile name the lagging row. That needs at least two events
//! on the stream; fewer is an error. Like any live subscription both rows hold the
//! topic's retention floor until retired or wiped.
//!
//! The SQL lives here, not in `asyncevents::admin`: these are demo fixtures, not
//! operator operations.

use anyhow::{bail, Context, Result};
use sqlx::{PgConnection, PgPool};

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

/// Sets every column, so a run resets a row from any state, retired included. `$7..$9`
/// bind the cursor `(generation, xid, tie)`. `spec_hash` / `start_kind` are what
/// `asyncevents` writes for a genesis subscription (`SubEntry::spec_hash` in
/// `core/asyncevents/src/transport.rs`); they describe where the subscription started,
/// not where its cursor is, so the paused row keeps them too.
const UPSERT: &str = "\
INSERT INTO asyncevents.subscriptions \
    (subscription_id, topic, contract_version, state, \
     cursor_generation, cursor_xid, cursor_tie, next_attempt_at, consecutive_failures, \
     last_error, spec_hash, start_kind, updated_at) \
VALUES ($1, $2, $3, $4, $7, $8::xid8, $9, NULL, $5, $6, $10, 'genesis', now()) \
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
/// version too, so the version is chosen with the topic. Errors on an empty log and on
/// a busiest stream of one event, where no cursor gives the paused row a lag strictly
/// below the lagging row's.
pub async fn seed_mockup(pool: &PgPool) -> Result<Seeded> {
    let mut tx = pool.begin().await.context("eventctl: begin seed-mockup tx")?;
    let seeded = seed_mockup_on(&mut tx).await?;
    tx.commit().await.context("eventctl: commit seed-mockup tx")?;
    Ok(seeded)
}

/// [`seed_mockup`]'s work on a caller-owned connection, which commits nothing: inside a
/// transaction both upserts land or neither does when the caller commits or rolls back.
pub async fn seed_mockup_on(conn: &mut PgConnection) -> Result<Seeded> {
    let busiest: Option<(String, i32, i64)> = sqlx::query_as(
        "SELECT topic, contract_version, count(*) FROM asyncevents.events \
         GROUP BY topic, contract_version \
         ORDER BY count(*) DESC, topic, contract_version \
         LIMIT 1",
    )
    .fetch_optional(&mut *conn)
    .await
    .context("eventctl: find the busiest topic")?;
    let Some((topic, version, events)) = busiest else {
        bail!("eventctl: seed-mockup: the event log is empty; run demoseed first");
    };
    if events < 2 {
        bail!(
            "eventctl: seed-mockup: the busiest stream {topic} v{version} has {events} event; \
             the paused row needs one event before its poison event, so run demoseed first"
        );
    }

    // The paused row's poison event is the stream's middle event (0-based index
    // `events / 2`, at least 1). Its cursor is the position of the event just before
    // it, in the plane's own `(generation, producer_xid, tie_breaker)` order, so
    // `poison_event`'s `>` comparison returns the middle event and the lag counts it
    // and every event after it. The alias must not equal the column name: a bare
    // ORDER BY prefers the output alias (text sort) over the xid8 column.
    let (pg, px, pt): (i64, String, i64) = sqlx::query_as(
        "SELECT generation, producer_xid::text AS producer_xid_text, tie_breaker \
         FROM asyncevents.events \
         WHERE topic = $1 AND contract_version = $2 \
         ORDER BY generation, producer_xid, tie_breaker \
         OFFSET $3 LIMIT 1",
    )
    .bind(&topic)
    .bind(version)
    .bind(events / 2 - 1)
    .fetch_one(&mut *conn)
    .await
    .context("eventctl: find the paused row's cursor")?;

    let spec_hash = format!("{topic}|v{version}|genesis");
    let rows = [
        (PAUSED_ID, "paused", PAUSED_FAILURES, PAUSED_ERROR, (pg, px, pt)),
        // The genesis position, which sorts before every real event.
        (LAGGING_ID, "active", LAGGING_FAILURES, LAGGING_ERROR, (0, "0".to_string(), 0)),
    ];
    for (id, state, failures, error, (cg, cx, ct)) in rows {
        sqlx::query(UPSERT)
            .bind(id)
            .bind(&topic)
            .bind(version)
            .bind(state)
            .bind(failures)
            .bind(error)
            .bind(cg)
            .bind(&cx)
            .bind(ct)
            .bind(&spec_hash)
            .execute(&mut *conn)
            .await
            .with_context(|| format!("eventctl: upsert {id}"))?;
    }
    Ok(Seeded { topic, version, events })
}
