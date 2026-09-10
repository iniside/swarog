//! Live-Postgres projection tests: `retention_days_from_env`'s fail-closed parsing
//! (a bad value fails BOOT, never first delivery), and the retention sweep itself —
//! driven by direct `PruneHandler::call` invocations (mail's own precedent), never
//! through a hand-rolled proxy — proving it deletes `invited`/`requested` rows past
//! retention while leaving `member` rows untouched, that its batch loop actually
//! terminates over a tie spanning the 256-row boundary without skipping or
//! double-deleting, and that a foreign schedule name is a true no-op.

use super::*;

use bus::{AnyTx, Delivery, TxHandler};
use groupsapi::{ROLE_MEMBER, STATE_INVITED, STATE_MEMBER, STATE_REQUESTED};
use groupsevents::REASON_EXPIRED;
use sqlx::PgPool;

use crate::projection::{
    retention_days_from_env, PruneHandler, DEFAULT_RETENTION_DAYS, MAX_RETENTION_DAYS,
    PRUNE_BATCH, PRUNE_SCHEDULE_NAME, RETENTION_ENV,
};
use crate::tests::{cleanup_groups, ensure_schema, test_pool, unique_uuid, DB_LOCK};

async fn seed_aged(pool: &PgPool, group_id: &str, player_id: &str, state: &str, role: &str, age_days: i32) {
    sqlx::query(
        "INSERT INTO groups.memberships (group_id, player_id, state, role, created_at) \
         VALUES ($1::uuid, $2::uuid, $3, $4, now() - make_interval(days => $5))",
    )
    .bind(group_id)
    .bind(player_id)
    .bind(state)
    .bind(role)
    .bind(age_days)
    .execute(pool)
    .await
    .unwrap();
}

async fn row_states(pool: &PgPool, group_id: &str) -> Vec<String> {
    sqlx::query_scalar("SELECT state FROM groups.memberships WHERE group_id = $1::uuid")
        .bind(group_id)
        .fetch_all(pool)
        .await
        .unwrap()
}

async fn member_left_events(pool: &PgPool, group_id: &str, player_id: &str) -> Vec<(String, String)> {
    let rows: Vec<(serde_json::Value,)> = sqlx::query_as(
        "SELECT payload FROM asyncevents.events \
          WHERE topic = $1 AND payload->>'group_id' = $2 AND payload->>'player_id' = $3",
    )
    .bind(groupsevents::MEMBER_LEFT.topic())
    .bind(group_id)
    .bind(player_id)
    .fetch_all(pool)
    .await
    .unwrap();
    rows.into_iter()
        .map(|(p,)| {
            (
                p["reason"].as_str().unwrap().to_string(),
                p["actor_id"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn fired_payload(name: &str) -> Vec<u8> {
    serde_json::to_vec(&schedulerevents::Fired { name: name.to_string() }).unwrap()
}

// ============================================================================
// The sweep: deletes pending states past retention, leaves `member` alone, and
// emits exactly one `group.member_left{reason: expired, actor_id: ""}` per swept row.
// ============================================================================

#[tokio::test]
async fn prune_deletes_pending_past_retention_and_leaves_member_untouched() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    ensure_schema(&pool).await;
    let transport = asyncevents::testing::transport(pool.clone());
    let ctx = Context::with_db_and_transport(pool.clone(), transport.handle());

    let group_id = unique_uuid(&pool).await;
    let invited = unique_uuid(&pool).await;
    let requested = unique_uuid(&pool).await;
    let member = unique_uuid(&pool).await;

    seed_aged(&pool, &group_id, &invited, STATE_INVITED, "", DEFAULT_RETENTION_DAYS + 5).await;
    seed_aged(&pool, &group_id, &requested, STATE_REQUESTED, "", DEFAULT_RETENTION_DAYS + 5).await;
    seed_aged(&pool, &group_id, &member, STATE_MEMBER, ROLE_MEMBER, DEFAULT_RETENTION_DAYS + 5).await;

    let handler = PruneHandler {
        retention_days: DEFAULT_RETENTION_DAYS,
        bus: ctx.bus().clone(),
    };
    let mut tx = pool.begin().await.unwrap();
    handler
        .call(
            Delivery { event_id: "groups-prune-basic", tx: AnyTx::new(&mut *tx) },
            fired_payload(PRUNE_SCHEDULE_NAME),
        )
        .await
        .expect("the sweep must answer Ok");
    tx.commit().await.unwrap();

    let remaining = row_states(&pool, &group_id).await;
    assert_eq!(remaining, vec![STATE_MEMBER.to_string()], "only the member row must survive");

    for pid in [&invited, &requested] {
        let events = member_left_events(&pool, &group_id, pid).await;
        assert_eq!(events.len(), 1, "exactly one member_left per swept row for {pid}");
        assert_eq!(events[0].0, REASON_EXPIRED);
        assert_eq!(events[0].1, "", "no party ended it — actor_id must be empty, never a stand-in id");
    }
    assert!(
        member_left_events(&pool, &group_id, &member).await.is_empty(),
        "the untouched member row must emit nothing"
    );

    cleanup_groups(&pool, &[group_id]).await;
}

// ============================================================================
// The batch loop: PRUNE_BATCH-per-statement, terminating even across a tie that
// spans the 256-row cut — `>=` on the watermark, never `>`.
// ============================================================================

/// 200 rows strictly older than a 100-row TIE group at the exact same timestamp.
/// `ORDER BY created_at ASC LIMIT 256` takes all 200 older rows plus 56 of the tied
/// ones in batch 1; the watermark then carries the tie's own timestamp into batch 2.
/// A `>` watermark (instead of `>=`) would find NOTHING at that exact timestamp and
/// strand the remaining 44 tied rows forever.
#[tokio::test]
async fn prune_batch_loop_terminates_across_a_tie_at_the_256_row_boundary() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    ensure_schema(&pool).await;
    let group_id = unique_uuid(&pool).await;

    const {
        assert!(
            200i64 < PRUNE_BATCH && 300i64 > PRUNE_BATCH,
            "fixture assumes PRUNE_BATCH == 256; update the seed counts if it changes"
        );
    };

    sqlx::query(
        "INSERT INTO groups.memberships (group_id, player_id, state, role, created_at) \
         SELECT $1::uuid, gen_random_uuid(), 'invited', '', now() - make_interval(days => 40) \
           FROM generate_series(1, 200)",
    )
    .bind(&group_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO groups.memberships (group_id, player_id, state, role, created_at) \
         SELECT $1::uuid, gen_random_uuid(), 'requested', '', now() - make_interval(days => 35) \
           FROM generate_series(1, 100)",
    )
    .bind(&group_id)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(row_states(&pool, &group_id).await.len(), 300);

    let transport = asyncevents::testing::transport(pool.clone());
    let ctx = Context::with_db_and_transport(pool.clone(), transport.handle());
    let handler = PruneHandler {
        retention_days: DEFAULT_RETENTION_DAYS,
        bus: ctx.bus().clone(),
    };
    let mut tx = pool.begin().await.unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        handler.call(
            Delivery { event_id: "groups-prune-tie", tx: AnyTx::new(&mut *tx) },
            fired_payload(PRUNE_SCHEDULE_NAME),
        ),
    )
    .await
    .expect("the sweep must not hang across multiple batches")
    .expect("the sweep must answer Ok");

    let (remaining,): (i64,) = sqlx::query_as("SELECT count(*) FROM groups.memberships WHERE group_id = $1::uuid")
        .bind(&group_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(
        remaining, 0,
        "every one of the 300 seeded rows must be gone — a `>` watermark would strand the \
         44 tied rows that missed batch 1"
    );
    // Discard everything (rows and the durable events the sweep emitted) — this probe
    // never needs to survive past its own assertions.
    tx.rollback().await.unwrap();
}

// ============================================================================
// A foreign `scheduler.fired{name}` is a true no-op: nothing deleted, nothing emitted.
// ============================================================================

#[tokio::test]
async fn prune_ignores_a_foreign_schedule_name() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    ensure_schema(&pool).await;
    let transport = asyncevents::testing::transport(pool.clone());
    let ctx = Context::with_db_and_transport(pool.clone(), transport.handle());

    let group_id = unique_uuid(&pool).await;
    let player_id = unique_uuid(&pool).await;
    seed_aged(&pool, &group_id, &player_id, STATE_INVITED, "", DEFAULT_RETENTION_DAYS + 5).await;

    let handler = PruneHandler {
        retention_days: DEFAULT_RETENTION_DAYS,
        bus: ctx.bus().clone(),
    };
    let mut tx = pool.begin().await.unwrap();
    handler
        .call(
            Delivery { event_id: "groups-prune-foreign", tx: AnyTx::new(&mut *tx) },
            fired_payload("some-other-schedule"),
        )
        .await
        .expect("a foreign schedule name must still answer Ok");
    tx.commit().await.unwrap();

    assert_eq!(row_states(&pool, &group_id).await, vec![STATE_INVITED.to_string()], "nothing deleted");
    assert!(
        member_left_events(&pool, &group_id, &player_id).await.is_empty(),
        "nothing emitted for a schedule this handler does not own"
    );

    cleanup_groups(&pool, &[group_id]).await;
}

// ============================================================================
// `retention_days_from_env` — ONLY an unset variable takes the compiled default,
// and a present-but-unusable one FAILS STARTUP.
// ============================================================================

struct EnvGuard(Option<String>);

impl EnvGuard {
    fn take() -> EnvGuard {
        EnvGuard(std::env::var(RETENTION_ENV).ok())
    }
    fn set(&self, value: impl AsRef<std::ffi::OsStr>) {
        std::env::set_var(RETENTION_ENV, value);
    }
    fn unset(&self) {
        std::env::remove_var(RETENTION_ENV);
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.0 {
            Some(v) => std::env::set_var(RETENTION_ENV, v),
            None => std::env::remove_var(RETENTION_ENV),
        }
    }
}

#[tokio::test]
async fn only_an_unset_retention_variable_takes_the_compiled_default() {
    let _serialized = DB_LOCK.lock().await;
    let guard = EnvGuard::take();
    guard.unset();
    assert_eq!(retention_days_from_env().unwrap(), DEFAULT_RETENTION_DAYS);
    guard.set("45");
    assert_eq!(retention_days_from_env().unwrap(), 45);
    guard.set("  7  ");
    assert_eq!(retention_days_from_env().unwrap(), 7, "surrounding whitespace is trimmed, not a failure");
}

#[tokio::test]
async fn a_present_but_unusable_retention_value_fails_startup() {
    let _serialized = DB_LOCK.lock().await;
    let guard = EnvGuard::take();
    for raw in ["   ", "\t", "0", "-1", "3651", "not-a-number", "30.5", "30 days"] {
        guard.set(raw);
        let err = retention_days_from_env().unwrap_err().to_string();
        assert!(err.contains(RETENTION_ENV), "{raw:?} must fail startup naming the variable, got {err}");
    }
    guard.set(MAX_RETENTION_DAYS.to_string());
    assert_eq!(
        retention_days_from_env().unwrap(),
        MAX_RETENTION_DAYS,
        "the ceiling itself is usable — the range check must not be off by one"
    );
}

/// The `VarError::NotUnicode` arm — without it a naive `unwrap_or(default)` would
/// swallow it into the compiled default instead of bailing.
#[tokio::test]
async fn a_non_unicode_retention_value_fails_startup() {
    let _serialized = DB_LOCK.lock().await;
    let guard = EnvGuard::take();
    #[cfg(unix)]
    let bad = {
        use std::os::unix::ffi::OsStrExt;
        std::ffi::OsStr::from_bytes(&[0x66, 0xff, 0x6f]).to_os_string()
    };
    #[cfg(windows)]
    let bad = {
        use std::os::windows::ffi::OsStringExt;
        std::ffi::OsString::from_wide(&[0x0066, 0xD800, 0x006F])
    };
    guard.set(&bad);
    let err = retention_days_from_env().unwrap_err().to_string();
    assert!(err.contains("not valid unicode"), "a non-unicode value must bail, got {err}");
}

/// The end-to-end proof that a bad value fails BOOT, not first delivery:
/// `Groups::init` calls `retention_days_from_env()` BEFORE it ever touches the bus or
/// the directory capability, so a bad env value must fail `init` itself — the process
/// never reaches a state where it could accept a `scheduler.fired` delivery at all.
#[tokio::test]
async fn a_bad_retention_value_fails_module_init_not_first_delivery() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let guard = EnvGuard::take();
    guard.set("not-a-number");

    let ctx = Context::with_db(pool.clone());
    let module = Groups::new();
    let err = module.init(&ctx).expect_err("init must fail fast on a bad retention value");
    assert!(
        err.to_string().contains(RETENTION_ENV),
        "the boot failure must name the offending variable, got {err}"
    );
}
