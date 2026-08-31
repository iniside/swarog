use super::*;

use std::collections::HashMap;
use std::time::Duration;

use base64::Engine as _;
use bus::{AnyTx, TxHandler};
use opsapi::{Identity, Status};
use sqlx::PgPool;

use crate::admin::{apply_submit, Rejection, ADMIN_LABEL, ADMIN_SLUG};
use crate::projection::{
    retention_days_from_env, PruneHandler, DEFAULT_RETENTION_DAYS, KIND_WALLET_CREDIT,
    MAX_RETENTION_DAYS, PLAYER_PROMOTED_SUB, PRUNE_BATCH, PRUNE_SCHEDULE_NAME, PRUNE_SUB,
    RETENTION_ENV, WALLET_CHANGED_SUB,
};
use crate::service::{
    decode_cursor, encode_cursor, is_operator_dedup_key, resolve_limit, validate_new,
    NewNotification, Sent, MAX_DEDUP_KEY_BYTES, OPERATOR_DEDUP_PREFIX,
};
use notificationsapi::{
    DEFAULT_PAGE_LIMIT, MAX_BODY_BYTES, MAX_CURSOR_BYTES, MAX_KIND_BYTES, MAX_PAGE_LIMIT,
    MAX_TITLE_BYTES,
};

/// Fallback DSN for the live tests (which otherwise read `DATABASE_URL`).
const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

/// ONE lock for every test that touches the live DB or the process environment. The delivery
/// tests share the module's three durable subscriptions (and reset two of them), the prune
/// probe takes a table-level lock on `notifications.messages`, and
/// `retention_days_from_env`'s cases mutate a process-global variable that
/// `NotificationsModule::init` reads — so these serialize here rather than depending on the
/// caller having passed `--test-threads=1`.
static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Opens the local Postgres; returns `None` (printing a skip line) when unreachable, so the
/// suite RUNS but SKIPs cleanly with no DB.
async fn test_pool() -> Option<PgPool> {
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let pool = match tokio::time::timeout(Duration::from_secs(3), PgPool::connect(&dsn)).await {
        Ok(Ok(p)) => p,
        _ => {
            eprintln!("SKIP: postgres unreachable at {dsn} — notifications DB tests skipped");
            return None;
        }
    };
    Some(pool)
}

/// Migrates BOTH the durable plane and this module's schema EXACTLY ONCE per test binary —
/// concurrent idempotent DDL can deadlock on catalog locks.
static SCHEMA_READY: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

async fn ensure_schema(pool: &PgPool) {
    SCHEMA_READY
        .get_or_init(|| async {
            let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
            asyncevents::Plane::new(pool.clone(), dsn)
                .unwrap()
                .migrate()
                .await
                .unwrap();
            let ctx = Context::with_db(pool.clone());
            let m = NotificationsModule::new();
            m.register(&ctx).unwrap();
            m.migrate(&ctx).await.unwrap();
        })
        .await;
}

/// `register` only — the pool-path fixture for the ops, the store and operator mail. No
/// subscription is recorded, so these tests never touch the shared checkpoints.
async fn wired(pool: &PgPool) -> (Context, Arc<Service>) {
    ensure_schema(pool).await;
    let ctx = Context::with_db(pool.clone());
    let m = NotificationsModule::new();
    m.register(&ctx).unwrap();
    (ctx, m.svc())
}

async fn reset_subscription(pool: &PgPool, id: &str) {
    sqlx::query("DELETE FROM asyncevents.subscriptions WHERE subscription_id = $1")
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
}

/// Wires the module the way `app::run` does — `register` THEN `init` — over a hand-driven
/// durable transport, and drains once BEFORE the test emits anything.
///
/// The trailing `deliver_all` is ORDERING-CRITICAL, not a warm-up: `AfterRegistration` stamps
/// the two content cursors with the RECONCILING transaction's xid, and reconcile happens
/// inside `deliver_all`. Reconciling only after the test's `emit_tx` would place the
/// checkpoint PAST the event under test and every delivery assertion would pass vacuously.
/// Its return value is NOT asserted: `PRUNE_SUB` starts at `Genesis`, so this pass also
/// drains whatever `scheduler.fired` backlog the shared log holds.
async fn wired_for_delivery(
    pool: &PgPool,
) -> (Context, Arc<Service>, asyncevents::testing::TestTransport) {
    ensure_schema(pool).await;
    reset_subscription(pool, WALLET_CHANGED_SUB.id).await;
    reset_subscription(pool, PLAYER_PROMOTED_SUB.id).await;
    let transport = asyncevents::testing::transport(pool.clone());
    let ctx = Context::with_db_and_transport(pool.clone(), transport.handle());
    let m = NotificationsModule::new();
    m.register(&ctx).unwrap();
    m.init(&ctx).unwrap();
    transport.deliver_all().await.unwrap();
    (ctx, m.svc(), transport)
}

async fn unique_player(pool: &PgPool) -> String {
    let (id,): (String,) = sqlx::query_as("SELECT gen_random_uuid()::text")
        .fetch_one(pool)
        .await
        .unwrap();
    id
}

/// A dedup key in the exact shape `admin::mint_idempotency_key` mints (its own minting is
/// private, and a hand-built key is what proves the SHAPE check rather than the minter).
async fn operator_key(pool: &PgPool) -> String {
    let (hex,): (String,) =
        sqlx::query_as("SELECT replace(gen_random_uuid()::text, '-', '')")
            .fetch_one(pool)
            .await
            .unwrap();
    format!("{OPERATOR_DEDUP_PREFIX}{hex}")
}

/// Writes one row through the module's OWN insert authority (`Service::deliver_on` on a pool
/// connection) — never a hand-rolled INSERT, so the ops tests read what production writes.
async fn seed_row(svc: &Service, pool: &PgPool, player_id: &str, title: &str) -> String {
    let key = operator_key(pool).await;
    let mut conn = pool.acquire().await.unwrap();
    let appended = svc
        .deliver_on(
            &mut conn,
            &NewNotification {
                player_id,
                kind: "operator.mail",
                title,
                body: "seeded",
                source_event_id: &key,
            },
        )
        .await
        .unwrap();
    assert!(appended, "the seed row must be appended, not deduped");
    let (id,): (String,) = sqlx::query_as(
        "SELECT id::text FROM notifications.messages WHERE source_event_id = $1",
    )
    .bind(&key)
    .fetch_one(pool)
    .await
    .unwrap();
    id
}

async fn rows_of(pool: &PgPool, player_id: &str) -> Vec<(String, String, String)> {
    sqlx::query_as(
        "SELECT id::text, kind, COALESCE(read_at::text, '') FROM notifications.messages \
          WHERE player_id = $1::uuid ORDER BY created_at DESC, id DESC",
    )
    .bind(player_id)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn cleanup(pool: &PgPool, players: &[&str]) {
    for pid in players {
        let _ = sqlx::query("DELETE FROM notifications.messages WHERE player_id = $1::uuid")
            .bind(pid)
            .execute(pool)
            .await;
        let _ = asyncevents::testing::cleanup_events(pool, "player_id", pid).await;
    }
}

/// THE non-poisoning proof: a handler that returned `Err` leaves `consecutive_failures = 1`
/// plus a backoff here (`worker::record_failure`), and at 20 it flips `state` to `paused` —
/// taking EVERY player's inbox offline. "No row was written" alone is satisfied by a
/// poisoned handler; this is not.
async fn subscription_health(pool: &PgPool, id: &str) -> (String, i32, Option<String>) {
    sqlx::query_as(
        "SELECT state, consecutive_failures, last_error FROM asyncevents.subscriptions \
          WHERE subscription_id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn assert_subscription_unpoisoned(pool: &PgPool, id: &str) {
    let (state, failures, last_error) = subscription_health(pool, id).await;
    assert_eq!(state, "active", "{id} must still be active");
    assert_eq!(
        failures, 0,
        "a data-quality verdict must return Ok(()), never Err; last_error = {last_error:?}"
    );
}

/// Clears a subscription's recorded failure AND its backoff, so a follow-up `deliver_all`
/// retries immediately. Explicit persisted state, never a wait on `next_attempt_at`'s real
/// clock.
async fn clear_backoff(pool: &PgPool, id: &str) {
    sqlx::query(
        "UPDATE asyncevents.subscriptions \
            SET consecutive_failures = 0, last_error = NULL, next_attempt_at = NULL \
          WHERE subscription_id = $1",
    )
    .bind(id)
    .execute(pool)
    .await
    .unwrap();
}

/// A `CHECK (false) NOT VALID` that makes EVERY new row fail with 23514 — an
/// infrastructure-class failure (not the 22P02 the insert authority maps to
/// `Status::Invalid`), raised INSIDE the plane's delivery transaction where the handler's
/// error class decides between a retry and a lost event.
const INSERT_BARRIER_ADD: &str = "ALTER TABLE notifications.messages \
     ADD CONSTRAINT notifications_test_insert_barrier CHECK (false) NOT VALID";
const INSERT_BARRIER_DROP: &str = "ALTER TABLE notifications.messages \
     DROP CONSTRAINT IF EXISTS notifications_test_insert_barrier";

/// Adds/removes the barrier under a bounded lock wait: `ALTER TABLE` takes ACCESS EXCLUSIVE,
/// so without `lock_timeout` a concurrent long transaction would turn this into a HANG
/// instead of a failure (`core/asyncevents/src/worker.rs` bounds the same class).
async fn set_insert_barrier(pool: &PgPool, on: bool) {
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql("SET LOCAL lock_timeout = '5s'; SET LOCAL statement_timeout = '30s';")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(if on { INSERT_BARRIER_ADD } else { INSERT_BARRIER_DROP })
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn emit_changed(ctx: &Context, pool: &PgPool, player_id: &str, delta: i64) {
    let mut tx = pool.begin().await.unwrap();
    let changed = walletevents::Changed {
        player_id: player_id.into(),
        currency: "gold".into(),
        delta,
        balance_after: 100,
        reason: "test".into(),
        ledger_id: "ledger-1".into(),
    };
    ctx.bus()
        .emit_tx(AnyTx::new(&mut *tx), &walletevents::CHANGED, &changed)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn emit_fired(ctx: &Context, pool: &PgPool, name: &str) {
    let mut tx = pool.begin().await.unwrap();
    let fired = schedulerevents::Fired { name: name.into() };
    ctx.bus()
        .emit_tx(AnyTx::new(&mut *tx), &schedulerevents::FIRED, &fired)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

// ============================================================================
// 1. The cursor codec — pure, no I/O. Every reject arm answers `Status::Invalid`
//    and NEVER `Ok(None)`, which would be a silent reset to page 1.
// ============================================================================

fn rejects_cursor(cursor: &str, why: &str) {
    match decode_cursor(cursor) {
        Ok(None) => panic!(
            "{why}: the codec answered `first page` — a silent reset makes a paging bug look \
             like an inbox that repeats its newest page forever"
        ),
        Ok(Some(parts)) => panic!("{why}: the codec ACCEPTED the cursor as {parts:?}"),
        Err(e) => assert_eq!(
            e.status,
            Status::Invalid,
            "{why}: a malformed cursor is a 400, got {:?} ({})",
            e.status,
            e.msg
        ),
    }
}

#[test]
fn a_minted_cursor_round_trips_through_the_codec() {
    let at = "2026-08-31T04:05:06.123456Z";
    let id = "3f2504e0-4f89-11d3-9a0c-0305e82c3301";
    let decoded = decode_cursor(&encode_cursor(at, id)).unwrap();
    assert_eq!(decoded, Some((at.to_string(), id.to_string())));
}

#[test]
fn the_empty_cursor_is_the_first_page() {
    assert_eq!(decode_cursor("").unwrap(), None);
}

#[test]
fn a_malformed_base64_cursor_is_rejected_not_reset() {
    rejects_cursor("!!!not base64!!!", "non-base64");
    // Valid base64 whose bytes are not UTF-8.
    rejects_cursor(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([0xff, 0xfe, 0xfd]),
        "base64 over non-utf8 bytes",
    );
}

#[test]
fn a_cursor_over_the_byte_cap_is_rejected() {
    let at = "2026-08-31T04:05:06.123456Z";
    let id = "3f2504e0-4f89-11d3-9a0c-0305e82c3301";
    let padded = format!("{}{}", encode_cursor(at, id), "A".repeat(MAX_CURSOR_BYTES));
    assert!(padded.len() > MAX_CURSOR_BYTES);
    rejects_cursor(&padded, "over MAX_CURSOR_BYTES");
    let e = decode_cursor(&padded).unwrap_err();
    assert!(
        e.msg.contains(&MAX_CURSOR_BYTES.to_string()),
        "the cap rejection must name the cap, got {:?}",
        e.msg
    );
}

#[test]
fn a_cursor_without_the_separator_is_rejected() {
    let joined = "2026-08-31T04:05:06.123456Z3f2504e0-4f89-11d3-9a0c-0305e82c3301";
    rejects_cursor(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(joined),
        "no '|' separator",
    );
}

/// The arm that keeps `$2::timestamptz` from ever raising 22008 (a 500) on a digit-SHAPED
/// but impossible instant. Deleting the calendar half of `is_cursor_time` turns every one of
/// these into an accepted cursor.
#[test]
fn digit_shaped_but_impossible_cursor_times_are_rejected() {
    let id = "3f2504e0-4f89-11d3-9a0c-0305e82c3301";
    for (at, why) in [
        ("2026-13-01T00:00:00.000000Z", "month 13"),
        ("2026-00-01T00:00:00.000000Z", "month 0"),
        ("2026-01-45T00:00:00.000000Z", "day 45"),
        ("2026-01-00T00:00:00.000000Z", "day 0"),
        ("2026-02-30T00:00:00.000000Z", "february 30"),
        ("2026-02-29T00:00:00.000000Z", "february 29 of a common year"),
        ("2026-08-31T25:00:00.000000Z", "hour 25"),
        ("2026-08-31T00:60:00.000000Z", "minute 60"),
        ("2026-08-31T00:00:60.000000Z", "second 60"),
        ("0000-01-01T00:00:00.000000Z", "year 0000"),
        ("2026-08-31T00:00:00Z", "wrong shape (no microseconds)"),
    ] {
        rejects_cursor(&encode_cursor(at, id), why);
    }
    // The positive control for the same predicate: a real leap day IS accepted, so the
    // rejections above are not "everything is refused".
    assert!(decode_cursor(&encode_cursor("2024-02-29T23:59:59.999999Z", id))
        .unwrap()
        .is_some());
}

#[test]
fn a_cursor_whose_id_half_is_not_a_uuid_is_rejected() {
    let at = "2026-08-31T04:05:06.123456Z";
    rejects_cursor(&encode_cursor(at, "not-a-uuid"), "non-uuid id half");
    rejects_cursor(
        &encode_cursor(at, "3f2504e04f8911d39a0c0305e82c3301"),
        "unhyphenated uuid",
    );
}

#[test]
fn the_limit_defaults_clamps_and_refuses_a_negative() {
    assert_eq!(resolve_limit(0).unwrap(), DEFAULT_PAGE_LIMIT);
    assert_eq!(resolve_limit(5).unwrap(), 5);
    assert_eq!(resolve_limit(MAX_PAGE_LIMIT + 1).unwrap(), MAX_PAGE_LIMIT);
    assert_eq!(resolve_limit(-1).unwrap_err().status, Status::Invalid);
}

// ============================================================================
// 2. The operator dedup key and the shared input policy.
// ============================================================================

/// The shape check is what keeps the two halves of the ONE dedup column disjoint: anything
/// looser lets an operator send claim a durable `event_id`'s row and silently suppress that
/// event's notification.
#[test]
fn only_the_exact_minted_shape_is_an_operator_dedup_key() {
    let hex = "0123456789abcdef0123456789abcdef";
    assert!(is_operator_dedup_key(&format!("{OPERATOR_DEDUP_PREFIX}{hex}")));
    assert!(
        !is_operator_dedup_key(OPERATOR_DEDUP_PREFIX),
        "prefix only — no random half at all"
    );
    assert!(
        !is_operator_dedup_key(&format!("{OPERATOR_DEDUP_PREFIX}0123456789abcdef")),
        "short random half"
    );
    assert!(
        !is_operator_dedup_key(&format!("{OPERATOR_DEDUP_PREFIX}{hex}00")),
        "over-long random half"
    );
    assert!(
        !is_operator_dedup_key(&format!("{OPERATOR_DEDUP_PREFIX}0123456789abcdef0123456789abcdeg")),
        "non-hex random half"
    );
    assert!(
        !is_operator_dedup_key("3f2504e0-4f89-11d3-9a0c-0305e82c3301"),
        "a durable event_id must never pass as an operator key"
    );
    assert!(!is_operator_dedup_key(""), "empty");
}

/// `source_event_id` is a btree INDEX key with no column CHECK under it: an over-long value
/// is 54000 — an unmappable 500 — and an empty one is stored NULL, which the PARTIAL unique
/// index skips, so the row would dedup nothing at all.
#[test]
fn the_input_policy_requires_a_bounded_dedup_key() {
    let base = NewNotification {
        player_id: "3f2504e0-4f89-11d3-9a0c-0305e82c3301",
        kind: "operator.mail",
        title: "t",
        body: "b",
        source_event_id: "admin-send-mail-0123456789abcdef0123456789abcdef",
    };
    validate_new(&base).expect("the canonical shape must validate");

    let long = "x".repeat(5000);
    let e = validate_new(&NewNotification {
        source_event_id: &long,
        ..base
    })
    .unwrap_err();
    assert_eq!(e.status, Status::Invalid);
    assert!(
        e.msg.contains(&MAX_DEDUP_KEY_BYTES.to_string()),
        "the rejection must name the cap, got {:?}",
        e.msg
    );

    assert_eq!(
        validate_new(&NewNotification {
            source_event_id: "",
            ..base
        })
        .unwrap_err()
        .status,
        Status::Invalid,
        "a keyless row deduplicates nothing — it must be refused, not written NULL"
    );
    assert_eq!(
        validate_new(&NewNotification {
            source_event_id: "   ",
            ..base
        })
        .unwrap_err()
        .status,
        Status::Invalid,
        "whitespace is empty"
    );
}

#[test]
fn the_input_policy_caps_every_column_the_ddl_checks() {
    let key = "admin-send-mail-0123456789abcdef0123456789abcdef";
    let base = NewNotification {
        player_id: "3f2504e0-4f89-11d3-9a0c-0305e82c3301",
        kind: "operator.mail",
        title: "t",
        body: "b",
        source_event_id: key,
    };
    // DRIVEN BY `DDL_CAPS`, not by a hand-written trio: a cap added to that list (and to the
    // DDL) with no matching `*_within_cap` line in `validate_new` fails HERE, where a
    // list-to-list anti-drift test alone would stay green while the column CHECK became the
    // only enforcement — i.e. a 500 instead of a 400.
    for (constraint, column, max_bytes) in DDL_CAPS {
        let over = "x".repeat(max_bytes + 1);
        let n = over_cap_notification(column, &over, &base);
        assert_eq!(
            validate_new(&n).unwrap_err().status,
            Status::Invalid,
            "{constraint}: `validate_new` must refuse a {column} of {} bytes itself — left to \
             the column CHECK it is a 23514 nothing maps (a 500)",
            max_bytes + 1
        );
        let at_cap = "x".repeat(*max_bytes);
        validate_new(&over_cap_notification(column, &at_cap, &base)).unwrap_or_else(|e| {
            panic!("{constraint}: exactly {max_bytes} bytes must be ACCEPTED, got {e:?}")
        });
    }
    for (n, why) in [
        (
            NewNotification {
                player_id: "  ",
                ..base
            },
            "player_id",
        ),
        (NewNotification { kind: "", ..base }, "empty kind"),
    ] {
        assert_eq!(
            validate_new(&n).unwrap_err().status,
            Status::Invalid,
            "{why} must be a 400 from the Rust policy, not a column CHECK's 23514 (a 500)"
        );
    }
}

// ============================================================================
// 3. Cap ↔ DDL anti-drift, BOTH directions.
// ============================================================================

/// The contract cap and the column CHECK that backstops it, stated in the two languages that
/// must agree. Bump one without the other and the Rust validator accepts a value the CHECK
/// rejects as an unmapped 23514 — the operator gets a 500 where a 400 belongs.
const DDL_CAPS: &[(&str, &str, usize)] = &[
    ("notifications_title_len_check", "title", MAX_TITLE_BYTES),
    ("notifications_body_len_check", "body", MAX_BODY_BYTES),
    ("notifications_kind_len_check", "kind", MAX_KIND_BYTES),
];

/// Every `octet_length(<column>) <= <bytes>` the DDL declares, as `(column, bytes)`. The
/// inventory is derived from the CHECK EXPRESSION, not from a constraint-name suffix: a
/// column capped by a constraint someone named `notifications_subject_bound` would be
/// invisible to a `_len_check` filter, ship with no Rust twin, and answer 23514 (a 500)
/// where a 400 belongs.
fn ddl_octet_caps() -> Vec<(String, usize)> {
    let flat = SCHEMA_DDL.split_whitespace().collect::<Vec<_>>().join(" ");
    let marker = "octet_length(";
    flat.match_indices(marker)
        .map(|(index, _)| {
            let rest = &flat[index + marker.len()..];
            let (column, tail) = rest.split_once(')').expect("octet_length( with no close");
            let bytes = tail
                .trim_start()
                .strip_prefix("<= ")
                .and_then(|t| t.split(')').next())
                .and_then(|n| n.trim().parse::<usize>().ok())
                .unwrap_or_else(|| {
                    panic!("SCHEMA_DDL caps {column} with an expression this test cannot read: {tail}")
                });
            (column.to_string(), bytes)
        })
        .collect()
}

/// The over-cap value for one capped column, as a whole [`NewNotification`]. The `match` is
/// TOTAL by panic: a column added to [`DDL_CAPS`] with no arm here — and therefore possibly
/// no `*_within_cap` line in `validate_new` — fails instead of silently testing nothing.
fn over_cap_notification<'a>(
    column: &str,
    over: &'a str,
    base: &NewNotification<'a>,
) -> NewNotification<'a> {
    match column {
        "title" => NewNotification { title: over, ..*base },
        "body" => NewNotification { body: over, ..*base },
        "kind" => NewNotification { kind: over, ..*base },
        other => panic!(
            "DDL_CAPS names column {other:?} with no arm here — add one, and check \
             `validate_new` refuses it at all"
        ),
    }
}

fn ddl_clause(constraint: &str) -> String {
    let flat = SCHEMA_DDL.split_whitespace().collect::<Vec<_>>().join(" ");
    let needle = format!("CONSTRAINT {constraint} ");
    let start = flat.find(&needle).unwrap_or_else(|| {
        panic!(
            "SCHEMA_DDL declares no `CONSTRAINT {constraint}` — DDL_CAPS names a constraint the \
             schema never creates, so nothing backstops that column"
        )
    });
    let rest = &flat[start..];
    let end = ["),", ");"]
        .iter()
        .filter_map(|terminator| rest.find(terminator))
        .min()
        .map(|index| index + 1)
        .unwrap_or(rest.len());
    rest[..end].to_owned()
}

#[test]
fn every_contract_cap_matches_its_check_constraint_in_the_ddl() {
    for (constraint, column, max_bytes) in DDL_CAPS {
        let clause = ddl_clause(constraint);
        // OCTETS, not characters: the Rust twin is `str::len`, so a 200-character multibyte
        // title is more than 200 octets and only one of the two levels would refuse it.
        assert!(
            clause.contains(&format!("octet_length({column}) <= {max_bytes})")),
            "{constraint}: notificationsapi caps {column} at {max_bytes} OCTETS, but SCHEMA_DDL \
             says `{clause}`"
        );
    }
}

/// The reverse leg: a column the DDL caps that no contract cap names has no Rust twin, so
/// its 23514 reaches the caller as an unmapped `Internal` — an operator sees a 500 where a
/// 400 belongs. Derived from the CHECK expression, so a constraint whose NAME breaks the
/// house `_len_check` convention is caught too.
#[test]
fn every_octet_cap_in_the_ddl_is_mapped_by_a_contract_cap() {
    let declared = ddl_octet_caps();
    assert!(!declared.is_empty(), "SCHEMA_DDL declares no octet_length CHECK");
    for (column, bytes) in &declared {
        let mapped = DDL_CAPS
            .iter()
            .find(|(_, capped, _)| *capped == column.as_str())
            .unwrap_or_else(|| {
                panic!(
                    "SCHEMA_DDL caps `{column}` at {bytes} octets but no notificationsapi cap \
                     names it — its 23514 stays an unmapped Internal error instead of a 400"
                )
            });
        assert_eq!(
            mapped.2, *bytes,
            "the DDL caps `{column}` at {bytes} octets, the contract at {}",
            mapped.2
        );
    }
    for (constraint, column, _) in DDL_CAPS {
        assert!(
            declared.iter().any(|(capped, _)| capped.as_str() == *column),
            "{constraint}: the contract caps `{column}`, but SCHEMA_DDL declares no \
             `octet_length({column})` CHECK under it"
        );
    }
}

/// The portal resolves a page's URL from `slugify(label)`, NOT from the item id, so every
/// self-link this module builds must come from `ADMIN_SLUG`. `slugify` is PRIVATE to
/// `modules/admin` and importing that module would be a module→module edge `archcheck`
/// rejects, so this is the strongest in-crate pin available: it catches a label edit that
/// leaves the slug behind. The LIVE proof — a real `GET /admin/inbox?player=<uuid>` — is
/// Step 9's split-proof assertion; a second admin page ever labelled "Inbox" would slug to
/// `inbox-2` and no in-crate test can see it.
#[test]
fn the_admin_slug_is_the_label_lowercased() {
    assert_eq!(ADMIN_SLUG, ADMIN_LABEL.to_lowercase());
    assert!(!ADMIN_LABEL.contains(' '), "a multi-word label would slugify with a dash");
}

// ============================================================================
// 4. `retention_days_from_env` — ONLY an unset variable takes the default.
// ============================================================================

/// Restores the variable to whatever the process had, so one case cannot leak into another
/// (or into a `NotificationsModule::init` running in a later test).
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
    assert_eq!(
        retention_days_from_env().unwrap(),
        7,
        "surrounding whitespace is trimmed, not a failure"
    );
}

/// A PRESENT-but-unusable value fails startup instead of silently pruning a 90-day inbox
/// two-thirds early. `NOTIFICATIONS_RETENTION_DAYS=${RETENTION_DAYS}` with the outer
/// variable unset expands to exactly the blank case.
#[tokio::test]
async fn a_present_but_unusable_retention_value_fails_startup() {
    let _serialized = DB_LOCK.lock().await;
    let guard = EnvGuard::take();
    // NOTE: `set_var(k, "")` REMOVES the variable on Windows, which would silently assert
    // the default instead of the bail — so the empty case is spelled as whitespace.
    for raw in ["   ", "\t", "0", "-1", "3651", "not-a-number", "30.5", "30 days"] {
        guard.set(raw);
        let err = retention_days_from_env()
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(RETENTION_ENV),
            "{raw:?} must fail startup naming the variable, got {err}"
        );
    }
    guard.set(MAX_RETENTION_DAYS.to_string());
    assert_eq!(
        retention_days_from_env().unwrap(),
        MAX_RETENTION_DAYS,
        "the ceiling itself is usable — the range check must not be off by one"
    );
}

/// The `VarError::NotUnicode` arm: without it `std::env::var` returns an `Err` that a naive
/// `unwrap_or(default)` would swallow into the compiled default.
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
        // A lone high surrogate: valid UTF-16 storage, not valid UTF-8.
        std::ffi::OsString::from_wide(&[0x0066, 0xD800, 0x006F])
    };
    guard.set(&bad);
    let err = retention_days_from_env().unwrap_err().to_string();
    assert!(
        err.contains("not valid unicode"),
        "a non-unicode value must bail, got {err}"
    );
}

// ============================================================================
// 5. Keyset paging — the `id` tiebreak on rows sharing one `created_at`.
// ============================================================================

/// Five rows, ONE `created_at`, paged three at a time. A bare `created_at <` predicate
/// (no `id` tiebreak) either loops on the same page forever or skips the rest of the tied
/// block — this walks the whole inbox and asserts every id exactly once.
#[tokio::test]
async fn keyset_paging_walks_rows_sharing_one_created_at_exactly_once() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (_ctx, svc) = wired(&pool).await;
    let pid = unique_player(&pool).await;

    let key_prefix = operator_key(&pool).await;
    sqlx::query(
        "INSERT INTO notifications.messages \
             (id, player_id, kind, title, body, created_at, source_event_id) \
         SELECT gen_random_uuid(), $1::uuid, 'operator.mail', 'tied ' || n, 'b', \
                timestamptz '2026-01-02 03:04:05.678901+00', $2 || '-' || n \
           FROM generate_series(1, 5) AS n",
    )
    .bind(&pid)
    .bind(&key_prefix)
    .execute(&pool)
    .await
    .unwrap();

    let identity = Identity::player(pid.as_str());
    let mut seen: Vec<String> = Vec::new();
    let mut cursor = String::new();
    let mut pages = 0;
    let exhausted = loop {
        pages += 1;
        assert!(pages <= 6, "the walk never exhausted the inbox: saw {seen:?}");
        let page = svc.list(identity.clone(), cursor.clone(), 2).await.unwrap();
        seen.extend(page.items.iter().map(|n| n.id.clone()));
        if page.next_cursor.is_empty() {
            break true;
        }
        assert_eq!(
            page.items.len(),
            2,
            "a non-final page must be full — a short page carrying a cursor means the \
             surplus-row probe is wrong"
        );
        cursor = page.next_cursor;
    };
    assert!(exhausted);
    assert_eq!(pages, 3, "5 rows at 2 per page is three pages, the last one short");

    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        5,
        "every row must be seen EXACTLY once across the walk; saw {seen:?}"
    );

    cleanup(&pool, &[&pid]).await;
}

// ============================================================================
// 6. Ownership authz — 404, never 403, and A's row untouched.
// ============================================================================

/// The enumeration oracle. A 403 would confirm the id exists; ownership is a PREDICATE in
/// the statement, so B's call must be indistinguishable from a call on a row that never
/// existed — and it must leave A's `read_at` alone.
#[tokio::test]
async fn another_players_mark_read_is_not_found_and_changes_nothing() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (_ctx, svc) = wired(&pool).await;
    let owner = unique_player(&pool).await;
    let stranger = unique_player(&pool).await;
    let id = seed_row(&svc, &pool, &owner, "owner's mail").await;

    let e = svc
        .mark_read(Identity::player(stranger.as_str()), id.clone())
        .await
        .expect_err("a stranger must not mark another player's row read");
    assert_eq!(
        e.status,
        Status::NotFound,
        "403 would confirm the id exists — the answer must be 404"
    );
    assert_eq!(
        rows_of(&pool, &owner).await,
        vec![(id.clone(), "operator.mail".to_string(), String::new())],
        "the owner's row must be UNREAD and still present after the stranger's call"
    );

    svc.mark_read(Identity::player(owner.as_str()), id.clone())
        .await
        .expect("the owner's own mark_read must succeed — the 404 above is not a broken query");

    cleanup(&pool, &[&owner, &stranger]).await;
}

#[tokio::test]
async fn another_players_delete_is_not_found_and_changes_nothing() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (_ctx, svc) = wired(&pool).await;
    let owner = unique_player(&pool).await;
    let stranger = unique_player(&pool).await;
    let id = seed_row(&svc, &pool, &owner, "owner's mail").await;

    let e = svc
        .delete(Identity::player(stranger.as_str()), id.clone())
        .await
        .expect_err("a stranger must not delete another player's row");
    assert_eq!(e.status, Status::NotFound);
    assert_eq!(
        rows_of(&pool, &owner).await.len(),
        1,
        "the owner's row must survive the stranger's delete"
    );

    svc.delete(Identity::player(owner.as_str()), id.clone())
        .await
        .expect("the owner's own delete must succeed");
    assert!(rows_of(&pool, &owner).await.is_empty());
    assert_eq!(
        svc.delete(Identity::player(owner.as_str()), id)
            .await
            .unwrap_err()
            .status,
        Status::NotFound,
        "delete is not idempotent — a replay is a 404, which is why it is not #[retry_safe]"
    );

    cleanup(&pool, &[&owner, &stranger]).await;
}

/// `COALESCE(read_at, now())` is what licenses `#[retry_safe]` on `mark_read`: a replay must
/// answer with the SAME state. A bare `read_at = now()` passes "it returned Ok" but moves the
/// timestamp on every retry.
#[tokio::test]
async fn mark_read_twice_keeps_the_first_read_at() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (_ctx, svc) = wired(&pool).await;
    let pid = unique_player(&pool).await;
    let id = seed_row(&svc, &pool, &pid, "read me").await;
    let identity = Identity::player(pid.as_str());

    svc.mark_read(identity.clone(), id.clone()).await.unwrap();
    let first = rows_of(&pool, &pid).await[0].2.clone();
    assert!(!first.is_empty(), "the first mark_read must stamp read_at");

    svc.mark_read(identity.clone(), id.clone()).await.unwrap();
    assert_eq!(
        rows_of(&pool, &pid).await[0].2,
        first,
        "a replay must keep the FIRST read timestamp"
    );

    let page = svc.list(identity, String::new(), 0).await.unwrap();
    assert_eq!(page.items.len(), 1);
    assert!(
        !page.items[0].read_at.is_empty(),
        "the contract's read state is the non-empty read_at"
    );

    cleanup(&pool, &[&pid]).await;
}

// ============================================================================
// 7. Operator mail — the three outcomes of ONE dedup key (finding B3/19).
// ============================================================================

fn send_params(player_id: &str, title: &str, body: &str, key: &str) -> adminapi::Params {
    HashMap::from([
        ("_action".to_string(), "send-mail".to_string()),
        ("player_id".to_string(), player_id.to_string()),
        ("title".to_string(), title.to_string()),
        ("body".to_string(), body.to_string()),
        ("_idem_send".to_string(), key.to_string()),
    ])
}

/// `ON CONFLICT DO NOTHING` collapses "the same form submitted twice" and "an edited form
/// resubmitted under its old key" into ONE SQL outcome. Only the first may read as success:
/// before the re-read, an operator's correction was silently discarded and the portal
/// rendered a success card.
#[tokio::test]
async fn one_dedup_key_appends_dedups_and_refuses_an_edited_resubmit() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (_ctx, svc) = wired(&pool).await;
    let pid = unique_player(&pool).await;
    let key = operator_key(&pool).await;
    let first = NewNotification {
        player_id: &pid,
        kind: "operator.mail",
        title: "Hello",
        body: "first",
        source_event_id: &key,
    };
    let edited = NewNotification {
        body: "edited",
        ..first
    };

    assert!(
        matches!(svc.send_operator_mail(&first).await.unwrap(), Sent::Appended),
        "the first send under a fresh key appends"
    );
    assert!(
        matches!(svc.send_operator_mail(&first).await.unwrap(), Sent::Duplicate),
        "the IDENTICAL payload under the same key is the double-submit the key exists for"
    );
    assert!(
        matches!(svc.send_operator_mail(&edited).await.unwrap(), Sent::KeyReused),
        "a DIFFERENT body under the same key must not read as sent"
    );
    assert_eq!(rows_of(&pool, &pid).await.len(), 1, "exactly one row for one key");
    let (body,): (String,) =
        sqlx::query_as("SELECT body FROM notifications.messages WHERE source_event_id = $1")
            .bind(&key)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(body, "first", "the edited resubmit must NOT have overwritten the row");

    // The same three outcomes as the operator sees them, through the submit authority both
    // topologies run.
    assert!(
        apply_submit(&svc, send_params(&pid, "Hello", "first", &key))
            .await
            .is_ok(),
        "an identical resubmit is a success no-op"
    );
    let stale = apply_submit(&svc, send_params(&pid, "Hello", "edited", &key))
        .await
        .expect_err("an edited resubmit must be refused");
    assert!(
        matches!(stale, Rejection::Stale),
        "an edited resubmit under a spent key is `reload`, never a silent success"
    );

    cleanup(&pool, &[&pid]).await;
}

/// The operator entry is the ONLY caller of the insert authority that a portal reaches, so
/// it is where a key from the durable half of the shared column has to be refused.
#[tokio::test]
async fn operator_mail_refuses_a_key_that_is_not_an_operator_key() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (_ctx, svc) = wired(&pool).await;
    let pid = unique_player(&pool).await;
    let sent = svc
        .send_operator_mail(&NewNotification {
            player_id: &pid,
            kind: "operator.mail",
            title: "t",
            body: "b",
            source_event_id: "3f2504e0-4f89-11d3-9a0c-0305e82c3301",
        })
        .await;
    let Err(e) = sent else {
        panic!("a bare uuid is the shape a durable event_id has — it must be refused")
    };
    assert_eq!(e.status, Status::Invalid);
    assert!(rows_of(&pool, &pid).await.is_empty());

    let stale = apply_submit(&svc, send_params(&pid, "t", "b", "not-a-minted-key"))
        .await
        .expect_err("a form that did not come from a render of this page is stale");
    assert!(matches!(stale, Rejection::Stale));

    cleanup(&pool, &[&pid]).await;
}

/// The `22P02 ⇒ Status::Invalid` arm of the insert authority and the TOLERANT cast under it —
/// neither reachable from the durable path, which never gets near the cast
/// (`deliverable_player_id` is why), so the operator form is the only caller that executes
/// them. Without the first, a mistyped id renders a 500 card instead of the rejection that
/// tells the operator what to change; without the second, a pasted Windows-style `{ABC…}` id
/// would write a row its owner can never read.
#[tokio::test]
async fn an_operator_id_is_cast_tolerantly_and_a_typo_is_a_rejection() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (_ctx, svc) = wired(&pool).await;
    let pid = unique_player(&pool).await;

    let typo_key = operator_key(&pool).await;
    let typo = apply_submit(&svc, send_params("oops", "Hello", "b", &typo_key))
        .await
        .expect_err("a player_id the DB cannot parse must be refused");
    match typo {
        Rejection::Rejected(msg) => assert!(
            msg.contains("uuid"),
            "the operator must be told WHAT to change, got {msg:?}"
        ),
        Rejection::Stale => panic!("a typo is not a stale form"),
        Rejection::Internal(msg) => {
            panic!("a mistyped id is operator input (400), not a server fault: {msg}")
        }
    }

    // The tolerant half: a braced, uppercase spelling of the SAME id folds onto one player.
    let key = operator_key(&pool).await;
    let braced = format!("{{{}}}", pid.to_uppercase());
    assert!(
        apply_submit(&svc, send_params(&braced, "Hello", "b", &key))
            .await
            .is_ok(),
        "a braced/uppercase id is one spelling of a real player, not a rejection"
    );
    assert_eq!(
        rows_of(&pool, &pid).await.len(),
        1,
        "the row must land on the CANONICAL player, who is the one who reads the inbox"
    );

    // And the conflict re-read compares THROUGH the same cast: a resubmit spelling the id
    // differently is the same message, not an edited one.
    assert!(
        apply_submit(&svc, send_params(&pid, "Hello", "b", &key))
            .await
            .is_ok(),
        "a resubmit spelling the id differently must read as the duplicate it is, never Stale"
    );
    assert_eq!(rows_of(&pool, &pid).await.len(), 1);

    cleanup(&pool, &[&pid]).await;
}

/// `admin_render` bridges the async store reads into the synchronous `RenderFn` with
/// `block_in_place`, which PANICS on a current-thread runtime — so the local page is only
/// exercised on the multi-thread flavour the monolith actually runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_local_inbox_page_renders_a_drill_down_and_carries_its_submit() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (_ctx, svc) = wired(&pool).await;
    let pid = unique_player(&pool).await;
    seed_row(&svc, &pool, &pid, "rendered mail").await;

    let params = adminapi::Params::from([("player".to_string(), pid.clone())]);
    let content = admin_render_for_test(&svc, &params);
    let table = content.table.as_ref().expect("the drill-down renders a table");
    assert_eq!(table.rows.len(), 1);
    let cells: Vec<&str> = table.rows[0].iter().map(|c| c.text.as_str()).collect();
    assert!(
        cells.contains(&"rendered mail"),
        "the rendered row must carry the seeded title, got {cells:?}"
    );
    assert!(
        content.form.as_ref().is_some_and(|f| f.submit.is_some()),
        "the LOCAL render must carry the in-process submit closure"
    );

    // A malformed drill-down renders an error CARD, never an Err: in a split, an Err raised
    // by another page's param would degrade this item to an error card in every sidebar.
    let bad = adminapi::Params::from([("player".to_string(), "not-a-uuid".to_string())]);
    let content = admin_render_for_test(&svc, &bad);
    assert!(content.table.is_none());
    assert_eq!(content.kpis.first().map(|k| k.label.as_str()), Some("Error"));

    cleanup(&pool, &[&pid]).await;
}

fn admin_render_for_test(svc: &Arc<Service>, params: &adminapi::Params) -> adminapi::Content {
    crate::admin::admin_render(svc, params).unwrap()
}

// ============================================================================
// 8. Durable fan-in — reached through a REAL delivery, never by calling the guard.
// ============================================================================

/// The positive control for every skip assertion below: the same fixture, the same emit, one
/// delivery — and a row. Without it a green "no row" proves nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wallet_credit_becomes_one_inbox_row() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (ctx, _svc, transport) = wired_for_delivery(&pool).await;
    let pid = unique_player(&pool).await;

    emit_changed(&ctx, &pool, &pid, 120).await;
    assert_eq!(transport.deliver_all().await.unwrap(), 1);

    let rows = rows_of(&pool, &pid).await;
    assert_eq!(rows.len(), 1, "a credit is news — it must produce a row");
    assert_eq!(rows[0].1, KIND_WALLET_CREDIT);
    assert_subscription_unpoisoned(&pool, WALLET_CHANGED_SUB.id).await;

    cleanup(&pool, &[&pid]).await;
}

/// The `delta <= 0` guard, reached through a REAL delivery. `delivered == 1` proves the
/// handler RAN and answered `Ok(())`; the unpoisoned checkpoint proves the skip did not take
/// the subscription — every player's inbox — offline.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wallet_debit_is_skipped_without_pausing_the_subscription() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (ctx, _svc, transport) = wired_for_delivery(&pool).await;
    let pid = unique_player(&pool).await;

    emit_changed(&ctx, &pool, &pid, -50).await;
    assert_eq!(
        transport.deliver_all().await.unwrap(),
        1,
        "the debit must be DELIVERED and answered Ok — an Err handler is not counted"
    );
    assert!(rows_of(&pool, &pid).await.is_empty(), "a debit is not news");
    assert_subscription_unpoisoned(&pool, WALLET_CHANGED_SUB.id).await;

    emit_changed(&ctx, &pool, &pid, 0).await;
    assert_eq!(transport.deliver_all().await.unwrap(), 1);
    assert!(rows_of(&pool, &pid).await.is_empty(), "a zero delta is not news either");
    assert_subscription_unpoisoned(&pool, WALLET_CHANGED_SUB.id).await;

    cleanup(&pool, &[&pid]).await;
}

/// The `Status::Invalid` arm of `deliver_or_skip`, reached through a REAL delivery: a
/// producer's over-long `reason` renders a body past `MAX_BODY_BYTES`, the shared input
/// policy refuses it, and the handler must answer `Ok(())`. Propagating that `Err` would
/// back the subscription off and, after 20 tries, PAUSE it — every player's inbox offline
/// over one bad payload. The row-count assertion alone passes against that broken shape;
/// the unpoisoned checkpoint is the half that does not.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_over_long_payload_is_rejected_without_pausing_the_subscription() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (ctx, _svc, transport) = wired_for_delivery(&pool).await;
    let pid = unique_player(&pool).await;

    let mut tx = pool.begin().await.unwrap();
    let changed = walletevents::Changed {
        player_id: pid.clone(),
        currency: "gold".into(),
        delta: 120,
        balance_after: 100,
        reason: "r".repeat(MAX_BODY_BYTES + 1),
        ledger_id: "ledger-1".into(),
    };
    ctx.bus()
        .emit_tx(AnyTx::new(&mut *tx), &walletevents::CHANGED, &changed)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(
        transport.deliver_all().await.unwrap(),
        1,
        "the rejected payload must be DELIVERED and answered Ok — an Err is not counted"
    );
    assert!(
        rows_of(&pool, &pid).await.is_empty(),
        "a body past the cap must not be written — the column CHECK would answer 23514"
    );
    assert_subscription_unpoisoned(&pool, WALLET_CHANGED_SUB.id).await;

    cleanup(&pool, &[&pid]).await;
}

/// The pre-check that avoids the 25P02 hazard: a non-canonical `player_id` must cost NO
/// statement, because a 22P02 inside the plane's delivery transaction aborts it and the
/// checkpoint `UPDATE` that follows an `Ok(())` then fails with 25P02 — which pauses the
/// subscription instead of skipping one payload.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_non_canonical_player_id_is_skipped_without_pausing_the_subscription() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (ctx, _svc, transport) = wired_for_delivery(&pool).await;
    let bogus = "not-a-uuid";

    emit_changed(&ctx, &pool, bogus, 120).await;
    assert_eq!(
        transport.deliver_all().await.unwrap(),
        1,
        "the payload must be DELIVERED and answered Ok, not faulted"
    );
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM notifications.messages WHERE kind = $1")
        .bind(KIND_WALLET_CREDIT)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_subscription_unpoisoned(&pool, WALLET_CHANGED_SUB.id).await;

    // The braced/uppercase spelling the tolerant operator cast folds is DELIVERED-strict:
    // it too must be skipped rather than cost a statement in the delivery transaction.
    let pid = unique_player(&pool).await;
    emit_changed(&ctx, &pool, &format!("{{{}}}", pid.to_uppercase()), 120).await;
    assert_eq!(transport.deliver_all().await.unwrap(), 1);
    assert!(rows_of(&pool, &pid).await.is_empty());
    assert_subscription_unpoisoned(&pool, WALLET_CHANGED_SUB.id).await;
    let (m,): (i64,) = sqlx::query_as("SELECT count(*) FROM notifications.messages WHERE kind = $1")
        .bind(KIND_WALLET_CREDIT)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, m, "neither malformed payload may have written a row");

    let _ = asyncevents::testing::cleanup_events(&pool, "player_id", bogus).await;
    cleanup(&pool, &[&pid]).await;
}

/// The OTHER half of `deliver_or_skip`'s verdict, and the only one no `Ok(())` proves:
/// anything that is NOT `Status::Invalid` is infrastructure and MUST propagate. Swallowed
/// into `Ok(())` it advances the checkpoint over an event that was never applied — the event
/// is LOST FOREVER, with no backoff and no `last_error` to find it by. `delivered == 0` plus
/// a recorded failure is what a retry looks like, and the decoy test proves both can move.
///
/// The failure is a 23514 raised INSIDE the delivery transaction by a `CHECK (false)` added
/// for the duration — 23514 is not the 22P02 the insert authority maps to `Invalid`, so it
/// takes exactly the arm under test. Nothing asserts between the two barrier calls: a panic
/// there would leave the shared table refusing every insert.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_infrastructure_failure_faults_the_delivery_and_keeps_the_event() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (ctx, _svc, transport) = wired_for_delivery(&pool).await;
    let pid = unique_player(&pool).await;
    emit_changed(&ctx, &pool, &pid, 120).await;

    set_insert_barrier(&pool, false).await;
    set_insert_barrier(&pool, true).await;
    let delivered = transport.deliver_all().await;
    let health = subscription_health(&pool, WALLET_CHANGED_SUB.id).await;
    let rows = rows_of(&pool, &pid).await.len();
    set_insert_barrier(&pool, false).await;

    assert_eq!(
        delivered.unwrap(),
        0,
        "an infrastructure failure must NOT be counted as a delivery — a counted one means \
         the checkpoint moved past an event that was never applied"
    );
    assert_eq!(rows, 0, "the barrier refused the insert");
    assert_eq!(
        health.1, 1,
        "the failure must be RECORDED so the plane retries and an operator can see it; \
         state = {:?}, last_error = {:?}",
        health.0, health.2
    );
    assert!(health.2.is_some(), "the failure must carry its error, not be swallowed");
    assert_eq!(health.0, "active", "one failure backs off, it does not pause yet");

    // The event was RETAINED, not skipped. The backoff is cleared as explicit state rather
    // than waited out on a real clock.
    clear_backoff(&pool, WALLET_CHANGED_SUB.id).await;
    assert_eq!(
        transport.deliver_all().await.unwrap(),
        1,
        "with the barrier gone the SAME event must still be there to deliver"
    );
    assert_eq!(
        rows_of(&pool, &pid).await.len(),
        1,
        "the retry lands the row the faulted attempt could not — nothing was lost"
    );
    assert_subscription_unpoisoned(&pool, WALLET_CHANGED_SUB.id).await;

    cleanup(&pool, &[&pid]).await;
}

/// Reads the subscription's checkpoint so a test can rewind it and have the plane deliver
/// the SAME `event_id` a second time — the operator re-drive the partial unique index exists
/// for.
async fn cursor_of(pool: &PgPool, id: &str) -> (i64, String, i64) {
    sqlx::query_as(
        "SELECT cursor_generation, cursor_xid::text, cursor_tie FROM asyncevents.subscriptions \
          WHERE subscription_id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn rewind_cursor(pool: &PgPool, id: &str, cursor: &(i64, String, i64)) {
    sqlx::query(
        "UPDATE asyncevents.subscriptions \
            SET cursor_generation = $2, cursor_xid = $3::xid8, cursor_tie = $4 \
          WHERE subscription_id = $1",
    )
    .bind(id)
    .bind(cursor.0)
    .bind(&cursor.1)
    .bind(cursor.2)
    .execute(pool)
    .await
    .unwrap();
}

/// The dedup belt, driven END TO END: the checkpoint is rewound so the plane hands the
/// handler the SAME `event_id` again. Both halves matter — ONE row (the partial unique index
/// inferred by the repeated `WHERE`), and the second delivery answering `Ok(())` with the
/// subscription still active. The row count alone passes against the broken `Err`-on-23505
/// shape that takes every player's inbox offline.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_same_event_id_delivered_twice_yields_one_row_and_no_backoff() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (ctx, _svc, transport) = wired_for_delivery(&pool).await;
    let pid = unique_player(&pool).await;

    let before = cursor_of(&pool, WALLET_CHANGED_SUB.id).await;
    emit_changed(&ctx, &pool, &pid, 120).await;
    assert_eq!(transport.deliver_all().await.unwrap(), 1);
    assert_eq!(rows_of(&pool, &pid).await.len(), 1);

    rewind_cursor(&pool, WALLET_CHANGED_SUB.id, &before).await;
    assert_eq!(
        transport.deliver_all().await.unwrap(),
        1,
        "the re-drive must be delivered AND answered Ok — an Err would not be counted"
    );
    assert_eq!(
        rows_of(&pool, &pid).await.len(),
        1,
        "the partial unique index must collapse the re-drive onto the one row"
    );
    assert_subscription_unpoisoned(&pool, WALLET_CHANGED_SUB.id).await;

    cleanup(&pool, &[&pid]).await;
}

/// The instrument itself, proven by construction: a decoy subscription on the SAME topic
/// whose handler always fails, delivered in the same pass. Without it, `delivered == 1` and
/// `consecutive_failures == 0` are assertions nobody has shown can fail — every skip test
/// above would be "green by absence of errors".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_faulting_handler_is_uncounted_and_backed_off() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    const DECOY_SUB: &str = "notifications.tests.poison-decoy.v1";
    ensure_schema(&pool).await;
    reset_subscription(&pool, WALLET_CHANGED_SUB.id).await;
    reset_subscription(&pool, PLAYER_PROMOTED_SUB.id).await;
    reset_subscription(&pool, DECOY_SUB).await;

    let transport = asyncevents::testing::transport(pool.clone());
    let ctx = Context::with_db_and_transport(pool.clone(), transport.handle());
    let m = NotificationsModule::new();
    m.register(&ctx).unwrap();
    m.init(&ctx).unwrap();
    ctx.bus().on_tx(
        bus::SubscriptionSpec {
            id: DECOY_SUB,
            start: bus::StartPosition::AfterRegistration,
        },
        &walletevents::CHANGED,
        |_delivery, _e: walletevents::Changed| {
            Box::pin(async move {
                Err(bus::Error::transport(std::io::Error::other(
                    "decoy handler: always fails",
                )))
            })
        },
    );
    transport.deliver_all().await.unwrap();

    let pid = unique_player(&pool).await;
    emit_changed(&ctx, &pool, &pid, 120).await;
    assert_eq!(
        transport.deliver_all().await.unwrap(),
        1,
        "one event, two subscriptions: only the Ok delivery is counted — which is exactly \
         what `delivered == 1` asserts in the skip tests"
    );
    assert_eq!(rows_of(&pool, &pid).await.len(), 1);
    assert_subscription_unpoisoned(&pool, WALLET_CHANGED_SUB.id).await;

    let (state, failures, last_error): (String, i32, Option<String>) = sqlx::query_as(
        "SELECT state, consecutive_failures, last_error FROM asyncevents.subscriptions \
          WHERE subscription_id = $1",
    )
    .bind(DECOY_SUB)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state, "active", "one failure backs off, it does not pause yet");
    assert_eq!(
        failures, 1,
        "a handler that returns Err DOES move consecutive_failures — so the `== 0` assertions \
         elsewhere are not vacuous"
    );
    assert!(last_error.is_some(), "the failure is recorded, not swallowed");

    reset_subscription(&pool, DECOY_SUB).await;
    cleanup(&pool, &[&pid]).await;
}

// ============================================================================
// 9. Retention — the prune subscription and the watermarked batch loop.
// ============================================================================

/// Inserts one row at an explicit age, through plain SQL because no production path can
/// backdate `created_at`.
async fn seed_aged_row(pool: &PgPool, player_id: &str, key: &str, age_days: i32) {
    sqlx::query(
        "INSERT INTO notifications.messages \
             (id, player_id, kind, title, body, created_at, source_event_id) \
         VALUES (gen_random_uuid(), $1::uuid, 'operator.mail', 'aged', 'b', \
                 now() - make_interval(days => $2), $3)",
    )
    .bind(player_id)
    .bind(age_days)
    .bind(key)
    .execute(pool)
    .await
    .unwrap();
}

/// The prune reached through a REAL delivery of `scheduler.fired`: the subscription is
/// registered on the raw topic and its handler must match the schedule NAME.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_scheduler_fire_prunes_only_rows_past_retention() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (ctx, _svc, transport) = wired_for_delivery(&pool).await;
    let pid = unique_player(&pool).await;
    let old_key = operator_key(&pool).await;
    let fresh_key = operator_key(&pool).await;
    seed_aged_row(&pool, &pid, &old_key, DEFAULT_RETENTION_DAYS + 5).await;
    seed_aged_row(&pool, &pid, &fresh_key, 1).await;
    assert_eq!(rows_of(&pool, &pid).await.len(), 2);

    emit_fired(&ctx, &pool, PRUNE_SCHEDULE_NAME).await;
    assert_eq!(transport.deliver_all().await.unwrap(), 1);

    let keys: Vec<(String,)> = sqlx::query_as(
        "SELECT source_event_id FROM notifications.messages WHERE player_id = $1::uuid",
    )
    .bind(&pid)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        keys,
        vec![(fresh_key.clone(),)],
        "the row past retention must go and the fresh one must stay"
    );
    assert_subscription_unpoisoned(&pool, PRUNE_SUB.id).await;

    cleanup(&pool, &[&pid]).await;
}

/// The subscription receives EVERY schedule's fire (it is a raw sink on the whole topic), so
/// the name guard is the only thing standing between another module's daily fire and this
/// module's retention policy.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_foreign_schedule_name_prunes_nothing() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (ctx, _svc, transport) = wired_for_delivery(&pool).await;
    let pid = unique_player(&pool).await;
    let old_key = operator_key(&pool).await;
    seed_aged_row(&pool, &pid, &old_key, DEFAULT_RETENTION_DAYS + 5).await;

    emit_fired(&ctx, &pool, "audit-prune").await;
    assert_eq!(
        transport.deliver_all().await.unwrap(),
        1,
        "the foreign fire IS delivered to this subscription — the guard is in the handler"
    );
    assert_eq!(
        rows_of(&pool, &pid).await.len(),
        1,
        "another module's schedule must not prune this module's inbox"
    );

    // The positive control on the SAME row: this module's own name does prune it, so the
    // assertion above is not "nothing ever prunes".
    emit_fired(&ctx, &pool, PRUNE_SCHEDULE_NAME).await;
    assert_eq!(transport.deliver_all().await.unwrap(), 1);
    assert!(rows_of(&pool, &pid).await.is_empty());
    assert_subscription_unpoisoned(&pool, PRUNE_SUB.id).await;

    cleanup(&pool, &[&pid]).await;
}

/// The watermarked LOOP, with a statement-level probe as the instrument: every DELETE this
/// module issues is counted, so the test can assert that no single statement exceeds
/// `PRUNE_BATCH` (an unbounded DELETE would seq-scan and delete everything in one) and that
/// the sweep KEEPS GOING until a short batch (the per-fire cap this replaced left retention
/// permanently behind its inflow).
///
/// Everything — the probe, the rows and the deletes — lives in ONE transaction that is rolled
/// back, so the shared table is untouched and no cleanup can be skipped by a panic.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_fire_loops_batched_deletes_and_never_exceeds_the_batch_per_statement() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    ensure_schema(&pool).await;
    let pid = unique_player(&pool).await;
    let seeded = PRUNE_BATCH * 2 + 7;

    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql(
        // `CREATE TRIGGER` takes SHARE ROW EXCLUSIVE on `notifications.messages`: unbounded,
        // a concurrent long transaction turns this test into a HANG that blocks every other
        // writer instead of a failure (`core/asyncevents/src/worker.rs` bounds the same
        // class).
        "SET LOCAL lock_timeout = '5s'; SET LOCAL statement_timeout = '60s'; \
         CREATE TEMP TABLE prune_probe (seq serial, n bigint) ON COMMIT DROP; \
         CREATE FUNCTION pg_temp.prune_probe_log() RETURNS trigger LANGUAGE plpgsql AS $fn$ \
           BEGIN INSERT INTO prune_probe (n) SELECT count(*) FROM removed; RETURN NULL; END $fn$; \
         CREATE TRIGGER notifications_prune_probe AFTER DELETE ON notifications.messages \
           REFERENCING OLD TABLE AS removed FOR EACH STATEMENT \
           EXECUTE FUNCTION pg_temp.prune_probe_log();",
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO notifications.messages \
             (id, player_id, kind, title, body, created_at, source_event_id) \
         SELECT gen_random_uuid(), $1::uuid, 'operator.mail', 'aged', 'b', \
                now() - make_interval(days => 99), 'prune-probe-' || n \
           FROM generate_series(1, $2) AS n",
    )
    .bind(&pid)
    .bind(seeded)
    .execute(&mut *tx)
    .await
    .unwrap();

    let handler = PruneHandler {
        retention_days: DEFAULT_RETENTION_DAYS,
    };
    let payload = serde_json::to_vec(&schedulerevents::Fired {
        name: PRUNE_SCHEDULE_NAME.to_string(),
    })
    .unwrap();
    handler
        .call(
            bus::Delivery {
                event_id: "prune-probe-event",
                tx: AnyTx::new(&mut *tx),
            },
            payload,
        )
        .await
        .expect("the sweep must answer Ok");

    let counts: Vec<(i64,)> = sqlx::query_as("SELECT n FROM prune_probe ORDER BY seq")
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    let counts: Vec<i64> = counts.into_iter().map(|(n,)| n).collect();
    assert!(
        counts.len() >= 3,
        "{seeded} stale rows at {PRUNE_BATCH} per statement must take at least 3 statements — \
         a per-fire CAP would stop after one; got {counts:?}"
    );
    assert!(
        counts.iter().all(|n| *n <= PRUNE_BATCH),
        "no single statement may delete more than PRUNE_BATCH rows; got {counts:?}"
    );
    assert!(
        *counts.last().unwrap() < PRUNE_BATCH,
        "the loop must end on a SHORT batch, not on a full one; got {counts:?}"
    );
    assert!(
        counts.iter().sum::<i64>() >= seeded,
        "every stale row must be swept in the one fire; got {counts:?} for {seeded} rows"
    );
    let (left,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM notifications.messages WHERE player_id = $1::uuid",
    )
    .bind(&pid)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(left, 0, "the fire must leave no stale row behind");

    tx.rollback().await.unwrap();
    // The rows cannot show the rollback — they were inserted AND deleted inside it, so the
    // count is 0 either way. The TRIGGER can: it is the artifact that would hurt the shared
    // database if this probe ever leaked, and it exists only if the transaction committed.
    let (triggers,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM pg_trigger WHERE tgname = $1")
            .bind("notifications_prune_probe")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        triggers, 0,
        "the probe transaction rolled back — its trigger must not survive on the shared table"
    );
}
