//! Subscription reconciliation: at [`crate::Plane::start`], every locally-registered
//! [`bus::SubscriptionSpec`] is materialized into an `asyncevents.subscriptions` row.
//!
//! Cursor discipline (the plan's normative rule): the cursor is NOT NULL from the
//! moment the row exists — `StartPosition` is materialized into an initial cursor at
//! reconcile time, and there are no separate floor columns; the initial cursor IS
//! the floor.
//! - `Genesis` → `(0, xid8 '0', 0)` — sorts before every real position (real
//!   generations are ≥ 1).
//! - `AfterRegistration` → `(current_generation, pg_current_xact_id(), i64::MAX)`,
//!   captured IN THE INSERTING TX: an exclusive floor that excludes every event of
//!   the registering transaction by construction.
//! - `Explicit(p)` → `p`.
//!
//! `start` applies only when the row does not exist yet; an existing checkpoint
//! always wins. An existing row whose immutable `spec_hash` (topic + version +
//! start) differs from the registering code's fails startup loudly — the stored
//! checkpoint would otherwise silently mean something else.

use sqlx::PgPool;

use crate::transport::SubEntry;

pub(crate) async fn reconcile(pool: &PgPool, subs: &[SubEntry]) -> anyhow::Result<()> {
    for entry in subs {
        reconcile_one(pool, entry).await?;
    }
    Ok(())
}

async fn reconcile_one(pool: &PgPool, entry: &SubEntry) -> anyhow::Result<()> {
    let hash = entry.spec_hash();
    let version = i32::try_from(entry.version)
        .map_err(|_| anyhow::anyhow!("asyncevents: contract version {} overflows i32", entry.version))?;

    // Insert-if-missing and the hash check share ONE tx so AfterRegistration's
    // pg_current_xact_id() is the xid of the tx that actually created the row.
    let mut tx = pool.begin().await?;
    let insert = match entry.spec.start {
        bus::StartPosition::Genesis => sqlx::query(
            "INSERT INTO asyncevents.subscriptions \
             (subscription_id, topic, contract_version, state, \
              cursor_generation, cursor_xid, cursor_tie, spec_hash, start_kind, updated_at) \
             VALUES ($1, $2, $3, 'active', 0, '0'::xid8, 0, $4, $5, now()) \
             ON CONFLICT (subscription_id) DO NOTHING",
        ),
        bus::StartPosition::AfterRegistration => sqlx::query(
            "INSERT INTO asyncevents.subscriptions \
             (subscription_id, topic, contract_version, state, \
              cursor_generation, cursor_xid, cursor_tie, spec_hash, start_kind, updated_at) \
             VALUES ($1, $2, $3, 'active', \
                     (SELECT generation FROM asyncevents.plane_meta WHERE singleton), \
                     pg_current_xact_id(), 9223372036854775807, $4, $5, now()) \
             ON CONFLICT (subscription_id) DO NOTHING",
        ),
        bus::StartPosition::Explicit(_) => sqlx::query(
            "INSERT INTO asyncevents.subscriptions \
             (subscription_id, topic, contract_version, state, \
              cursor_generation, cursor_xid, cursor_tie, spec_hash, start_kind, updated_at) \
             VALUES ($1, $2, $3, 'active', $6, $7::xid8, $8, $4, $5, now()) \
             ON CONFLICT (subscription_id) DO NOTHING",
        ),
    };
    let insert = insert
        .bind(entry.spec.id)
        .bind(&entry.topic)
        .bind(version)
        .bind(&hash)
        .bind(entry.start_kind());
    // The explicit-position params ride after the shared five; xid8 crosses the
    // boundary as text (the crate-wide codec convention — see `store`).
    let insert = match entry.spec.start {
        bus::StartPosition::Explicit(p) => {
            insert.bind(p.generation).bind(p.xid.to_string()).bind(p.tie)
        }
        _ => insert,
    };
    insert.execute(&mut *tx).await?;

    let stored: (String,) = sqlx::query_as(
        "SELECT spec_hash FROM asyncevents.subscriptions WHERE subscription_id = $1",
    )
    .bind(entry.spec.id)
    .fetch_one(&mut *tx)
    .await?;
    if stored.0 != hash {
        anyhow::bail!(
            "asyncevents: subscription {:?} is registered with spec {:?} but the stored \
             checkpoint was created for spec {:?} — a subscription id names exactly one \
             immutable (topic, version, start); pick a NEW id (new checkpoint) or retire \
             the old row via eventctl",
            entry.spec.id,
            hash,
            stored.0,
        );
    }
    tx.commit().await?;
    Ok(())
}
