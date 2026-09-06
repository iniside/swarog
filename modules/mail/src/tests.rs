use super::*;

use std::time::Duration;

use sqlx::PgPool;

use crate::address::parse_address;
use crate::admin::{
    apply_submit, build_content, Rejection, ACTION_CANCEL, ACTION_FIELD, ACTION_REQUEUE,
    ACTION_REQUEUE_ALL, ACTION_SEND_TEST, IDEM_TEST_FIELD, MAIL_ID_FIELD, PARAM_STATE,
    TEST_TO_FIELD,
};
use crate::providers::{Provider, ProviderKind, KNOWN_PROVIDERS};
use crate::service::{
    is_test_key, validate_new, NewMail, TEST_KEY_HEX, TEST_KEY_PREFIX,
};
use crate::store::{
    cap_from_db_error, classify_existing, truncate_error, Enqueued, ExistingMail,
    COLUMN_CAPS, LAST_ERROR_MAX_BYTES, STATE_CANCELLED, STATE_PARKED, STATE_PENDING, STATE_SENT,
};
use mailevents::MAX_ADDRESS_BYTES;

/// Fallback DSN for the live tests (which otherwise read `DATABASE_URL`).
pub(crate) const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

/// ONE lock for every test that touches the live DB. The delivery tests share this
/// module's two durable subscriptions (and reset them), the prune probe takes a
/// statement-level trigger on `mail.outbox`, and the claim tests race two connections for
/// one due row — so these serialize here rather than depending on the caller having passed
/// `--test-threads=1`.
pub(crate) static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Opens the local Postgres; returns `None` (printing a skip line) when unreachable, so the
/// suite RUNS but SKIPs cleanly with no DB.
pub(crate) async fn test_pool() -> Option<PgPool> {
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let pool = match tokio::time::timeout(Duration::from_secs(3), PgPool::connect(&dsn)).await {
        Ok(Ok(p)) => p,
        _ => {
            eprintln!("SKIP: postgres unreachable at {dsn} — mail DB tests skipped");
            return None;
        }
    };
    Some(pool)
}

/// Migrates BOTH the durable plane and this module's schema EXACTLY ONCE per test binary —
/// concurrent idempotent DDL can deadlock on catalog locks.
static SCHEMA_READY: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

pub(crate) async fn ensure_schema(pool: &PgPool) {
    SCHEMA_READY
        .get_or_init(|| async {
            let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
            asyncevents::Plane::new(pool.clone(), dsn)
                .unwrap()
                .migrate()
                .await
                .unwrap();
            let ctx = Context::with_db(pool.clone());
            let m = MailModule::new();
            m.register(&ctx).unwrap();
            m.migrate(&ctx).await.unwrap();
        })
        .await;
}

/// `register` only — the pool-path fixture for the store and the operator page. No
/// subscription is recorded, so these tests never touch the shared checkpoints.
pub(crate) async fn wired(pool: &PgPool) -> Arc<Service> {
    ensure_schema(pool).await;
    let ctx = Context::with_db(pool.clone());
    let m = MailModule::new();
    m.register(&ctx).unwrap();
    m.svc()
}

/// A key nothing else in the shared database can hold, carrying the tag so a leaked row
/// names the test that leaked it.
pub(crate) async fn unique_key(pool: &PgPool, tag: &str) -> String {
    let (hex,): (String,) = sqlx::query_as("SELECT replace(gen_random_uuid()::text, '-', '')")
        .fetch_one(pool)
        .await
        .unwrap();
    format!("mailtest-{tag}-{hex}")
}

/// An operator test-send key in the exact shape `service::is_test_key` admits, built by
/// hand rather than by the private minter — so the test pins the SHAPE, not the minter.
pub(crate) async fn minted_test_key(pool: &PgPool) -> String {
    let (hex,): (String,) = sqlx::query_as("SELECT replace(gen_random_uuid()::text, '-', '')")
        .fetch_one(pool)
        .await
        .unwrap();
    format!("{TEST_KEY_PREFIX}{hex}")
}

pub(crate) fn mail_of<'a>(key: &'a str, body: &'a str) -> NewMail<'a> {
    NewMail {
        idempotency_key: key,
        recipient: "player@example.com",
        subject: "Verify your address",
        body,
        kind: "verification",
    }
}

/// Enqueues through the module's OWN authority on a pool connection — never a hand-rolled
/// INSERT, so every test reads what production writes.
pub(crate) async fn enqueue(svc: &Service, pool: &PgPool, m: &NewMail<'_>) -> Enqueued {
    let mut conn = pool.acquire().await.unwrap();
    svc.enqueue_on(&mut conn, m).await.unwrap()
}

pub(crate) async fn row_of(pool: &PgPool, key: &str) -> (String, String, i32, i32, String) {
    sqlx::query_as(
        "SELECT state, body, attempts, generation, COALESCE(last_error, '') \
           FROM mail.outbox WHERE idempotency_key = $1",
    )
    .bind(key)
    .fetch_one(pool)
    .await
    .unwrap()
}

pub(crate) async fn id_of(pool: &PgPool, key: &str) -> String {
    let (id,): (String,) =
        sqlx::query_as("SELECT id::text FROM mail.outbox WHERE idempotency_key = $1")
            .bind(key)
            .fetch_one(pool)
            .await
            .unwrap();
    id
}

/// Sets a row's `next_attempt_at` explicitly. Lease expiry is PERSISTED STATE, never a
/// wait on a real clock.
pub(crate) async fn expire_lease(pool: &PgPool, key: &str) {
    sqlx::query(
        "UPDATE mail.outbox SET next_attempt_at = now() - interval '1 hour' \
          WHERE idempotency_key = $1",
    )
    .bind(key)
    .execute(pool)
    .await
    .unwrap();
}

/// Deletes this test's outbox rows when the test ENDS — including through a panicking
/// assertion, which a trailing call never reaches. The database is shared and a stranded
/// `pending` row is claimable by any live drain, so a leak from a red test is a leak into
/// every fleet that boots afterwards.
#[must_use = "the rows are deleted when this guard drops — binding it to `_` drops it at once"]
pub(crate) fn cleanup(pool: &PgPool, keys: &[&str]) -> Cleanup {
    Cleanup {
        pool: pool.clone(),
        keys: keys.iter().map(|k| (*k).to_string()).collect(),
    }
}

pub(crate) struct Cleanup {
    pool: PgPool,
    keys: Vec<String>,
}

impl Drop for Cleanup {
    /// `Drop` cannot await, and a spawned task would be dropped with the test's runtime
    /// before it ran — so the delete blocks on the current runtime. Every test that holds
    /// a guard is `flavor = "multi_thread"`, which is what `block_in_place` requires.
    fn drop(&mut self) {
        let pool = self.pool.clone();
        let keys = std::mem::take(&mut self.keys);
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                for key in &keys {
                    let _ = sqlx::query("DELETE FROM mail.outbox WHERE idempotency_key = $1")
                        .bind(key)
                        .execute(&pool)
                        .await;
                }
            })
        });
    }
}

// ============================================================================
// 1. The duplicate-vs-conflict verdict — pure, zero I/O.
// ============================================================================

fn existing(state: &str, body: &str) -> ExistingMail {
    ExistingMail {
        recipient: "player@example.com".into(),
        subject: "Verify your address".into(),
        body: body.into(),
        kind: "verification".into(),
        state: state.into(),
    }
}

#[test]
fn an_identical_replay_under_a_live_state_is_a_duplicate() {
    for state in [STATE_PENDING, STATE_PARKED, STATE_CANCELLED, STATE_SENT] {
        assert_eq!(
            classify_existing(&mail_of("k", "hello"), &existing(state, "hello")),
            Enqueued::Duplicate,
            "an identical message under a {state} row is a replay"
        );
    }
}

/// THE `sent` carve-out. A delivered row's `body` is BLANKED, so a durable replay of that
/// request compares `"" != body` — without [`body_is_comparable`] excluding `sent` it
/// answers `Conflict`, drops the message, and reports the operator's own row as a producer
/// bug (`store.rs`'s own doc warns about exactly this).
#[test]
fn a_sent_row_whose_body_was_blanked_still_reads_as_a_duplicate() {
    assert_eq!(
        classify_existing(&mail_of("k", "hello"), &existing(STATE_SENT, "")),
        Enqueued::Duplicate,
        "a blanked `sent` body must not be compared — the replay is a duplicate"
    );
}

/// The OTHER half of the same carve-out, and what pins it to `sent` ALONE: every state
/// whose body is still authoritative must answer `Conflict` on a differing body. A writer
/// that blanks a second state without extending the predicate turns these into silent
/// drops.
#[test]
fn a_differing_body_under_any_unblanked_state_is_a_conflict() {
    for state in [STATE_PENDING, STATE_PARKED, STATE_CANCELLED] {
        assert_eq!(
            classify_existing(&mail_of("k", "hello"), &existing(state, "goodbye")),
            Enqueued::Conflict,
            "a {state} row's body is authoritative — a differing body is a key reuse"
        );
    }
}

/// The carve-out drops `body` from the comparison and NOTHING else: a `sent` row whose
/// recipient, subject or kind differs is still a reused key.
#[test]
fn the_sent_carve_out_covers_the_body_only() {
    for (field, existing) in [
        (
            "recipient",
            ExistingMail {
                recipient: "other@example.com".into(),
                ..existing(STATE_SENT, "")
            },
        ),
        (
            "subject",
            ExistingMail {
                subject: "Something else".into(),
                ..existing(STATE_SENT, "")
            },
        ),
        (
            "kind",
            ExistingMail {
                kind: "password-reset".into(),
                ..existing(STATE_SENT, "")
            },
        ),
    ] {
        assert_eq!(
            classify_existing(&mail_of("k", "hello"), &existing),
            Enqueued::Conflict,
            "a `sent` row with a differing {field} is still a reused key"
        );
    }
}

// ============================================================================
// 2. The enqueue authority against the live unique index.
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_key_inserts_and_an_identical_replay_deduplicates() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let svc = wired(&pool).await;
    let key = unique_key(&pool, "dedup").await;
    let _cleanup = cleanup(&pool, &[&key]);

    let first = enqueue(&svc, &pool, &mail_of(&key, "hello")).await;
    assert!(matches!(first, Enqueued::Inserted(_)), "got {first:?}");
    assert_eq!(
        enqueue(&svc, &pool, &mail_of(&key, "hello")).await,
        Enqueued::Duplicate
    );
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM mail.outbox WHERE idempotency_key = $1")
        .bind(&key)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 1, "the unique key must collapse the replay onto one row");
}

/// A bare `ON CONFLICT DO NOTHING` would discard the corrected message and report success;
/// the counter is the only place the producer bug becomes visible, because the durable
/// handler answers `Ok(())`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_same_key_holding_a_different_message_is_a_conflict() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let svc = wired(&pool).await;
    let key = unique_key(&pool, "conflict").await;
    let _cleanup = cleanup(&pool, &[&key]);

    enqueue(&svc, &pool, &mail_of(&key, "hello")).await;
    assert_eq!(
        enqueue(&svc, &pool, &mail_of(&key, "a different body")).await,
        Enqueued::Conflict
    );
    let (state, body, _, _, _) = row_of(&pool, &key).await;
    assert_eq!(state, STATE_PENDING);
    assert_eq!(body, "hello", "the first message must survive untouched");
}

// ============================================================================
// 3. The Rust caps and their column CHECK twins.
// ============================================================================

/// The pairing `store::COLUMN_CAPS` asserts, read off the DDL itself: a cap naming a
/// constraint the table does not carry makes `cap_from_db_error` blind, and an over-long
/// value then reaches the durable handler as an unmapped 23514 that PAUSES the ingress.
#[test]
fn every_column_cap_names_the_check_the_ddl_actually_declares() {
    let ddl: String = SCHEMA_DDL.split_whitespace().collect::<Vec<_>>().join(" ");
    for cap in COLUMN_CAPS {
        let expected = format!(
            "CONSTRAINT {} CHECK (octet_length({}) <= {})",
            cap.constraint, cap.what, cap.max_bytes
        );
        assert!(
            ddl.contains(&expected),
            "the DDL must declare `{expected}` — the Rust cap and its class fail-safe are \
             one limit, not two"
        );
    }
}

/// The 23514 → cap lookup, driven by a value that skips `validate_new` entirely — which is
/// exactly the shape of the defect it exists to name.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_column_check_violation_resolves_to_the_cap_that_should_have_refused_it() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    ensure_schema(&pool).await;
    let key = unique_key(&pool, "cap").await;
    let _cleanup = cleanup(&pool, &[&key]);

    for cap in COLUMN_CAPS {
        let over = "a".repeat(cap.max_bytes + 1);
        let (k, recipient, subject, body, kind) = match cap.what {
            "idempotency_key" => (over.as_str(), "p@example.com", "s", "b", "k"),
            "recipient" => (key.as_str(), over.as_str(), "s", "b", "k"),
            "subject" => (key.as_str(), "p@example.com", over.as_str(), "b", "k"),
            "body" => (key.as_str(), "p@example.com", "s", over.as_str(), "k"),
            "kind" => (key.as_str(), "p@example.com", "s", "b", over.as_str()),
            other => panic!("COLUMN_CAPS grew a {other:?} entry this probe cannot drive"),
        };
        let e = sqlx::query(
            "INSERT INTO mail.outbox (idempotency_key, recipient, subject, body, kind) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(k)
        .bind(recipient)
        .bind(subject)
        .bind(body)
        .bind(kind)
        .execute(&pool)
        .await
        .expect_err("the column CHECK must refuse the over-long value");
        let resolved = cap_from_db_error(&e)
            .unwrap_or_else(|| panic!("23514 on {} resolved to no cap: {e}", cap.what));
        assert_eq!(resolved.what, cap.what);
        assert_eq!(resolved.constraint, cap.constraint);
    }
}

#[test]
fn an_unrelated_database_error_resolves_to_no_cap() {
    let e = sqlx::Error::RowNotFound;
    assert!(cap_from_db_error(&e).is_none());
}

#[test]
fn a_last_error_is_truncated_on_a_char_boundary() {
    assert_eq!(truncate_error("short"), "short");
    let at_cap = "a".repeat(LAST_ERROR_MAX_BYTES);
    assert_eq!(truncate_error(&at_cap), at_cap);
    let over = "a".repeat(LAST_ERROR_MAX_BYTES + 10);
    let cut = truncate_error(&over);
    assert!(cut.ends_with('…'));
    assert_eq!(cut.chars().count(), LAST_ERROR_MAX_BYTES + 1);
    // A multibyte code point straddling the cap: slicing mid-UTF-8 would PANIC.
    let multibyte = "ł".repeat(LAST_ERROR_MAX_BYTES);
    let cut = truncate_error(&multibyte);
    assert!(cut.ends_with('…'));
    assert!(cut.len() <= LAST_ERROR_MAX_BYTES + '…'.len_utf8());
}

// ============================================================================
// 4. `validate_new`'s NON-LENGTH rules — the ones the conformance gate does not run.
// ============================================================================

fn rejects(m: &NewMail<'_>, why: &str) -> String {
    let e = validate_new(m).expect_err(&format!("{why}: the request was ACCEPTED"));
    assert_eq!(e.status, opsapi::Status::Invalid, "{why}: {}", e.msg);
    e.msg
}

#[test]
fn the_base_probe_request_is_accepted() {
    validate_new(&mail_of("k", "hello")).expect("the positive control must pass");
}

#[test]
fn a_blank_idempotency_key_is_refused() {
    for key in ["", "   ", "\t\n"] {
        let msg = rejects(&mail_of(key, "hello"), &format!("key {key:?}"));
        assert!(msg.contains("idempotency_key"), "got {msg:?}");
    }
}

#[test]
fn a_blank_kind_is_refused() {
    for kind in ["", "   "] {
        let m = NewMail {
            kind,
            ..mail_of("k", "hello")
        };
        let msg = rejects(&m, &format!("kind {kind:?}"));
        assert!(msg.contains("kind"), "got {msg:?}");
    }
}

/// The header-injection guard. A subject goes into a message HEADER, where a newline
/// starts a header nobody wrote.
#[test]
fn control_characters_in_a_subject_are_refused() {
    for subject in ["a\r\nBcc: attacker@example.com", "a\nb", "a\rb", "a\0b"] {
        let m = NewMail {
            subject,
            ..mail_of("k", "hello")
        };
        let msg = rejects(&m, &format!("subject {subject:?}"));
        assert!(msg.contains("control characters"), "got {msg:?}");
    }
}

/// Every `parse_address` branch, through the recipient. The `a@` case is the reason a shape
/// check looking for an `@` was not enough: the row is enqueued, claimed, and permanently
/// parked on its first attempt, where a refusal here costs one counter and no row.
#[test]
fn every_address_branch_is_refused_with_its_own_reason() {
    for (value, needle, why) in [
        ("", "is required", "empty"),
        ("   ", "is required", "whitespace only"),
        ("a\r\nb@example.com", "control characters", "CRLF injection"),
        ("a@", "not a routable address", "empty domain"),
        ("@example.com", "not a routable address", "empty local part"),
        ("no-at-sign", "not a routable address", "no @"),
    ] {
        let reason = parse_address(value).expect_err(&format!("{why}: ACCEPTED {value:?}"));
        assert!(reason.contains(needle), "{why}: got {reason:?}");
        let m = NewMail {
            recipient: value,
            ..mail_of("k", "hello")
        };
        let msg = rejects(&m, why);
        assert!(msg.contains("recipient"), "{why}: got {msg:?}");
    }
    // The order matters: an over-cap value must be reported as the CAP, not as a parse
    // failure, so an operator reads the limit rather than a lettre message.
    let over = format!("{}@example.com", "a".repeat(MAX_ADDRESS_BYTES));
    let reason = parse_address(&over).unwrap_err();
    assert!(reason.contains(&MAX_ADDRESS_BYTES.to_string()), "got {reason:?}");
    // The positive control: the parser accepts a real address, so the rejections above are
    // not "everything is refused".
    parse_address("player@example.com").expect("a routable address must parse");
}

// ============================================================================
// 5. The operator test-send key shape and its AUTHORITY-level re-check.
// ============================================================================

#[test]
fn only_the_exact_minted_shape_is_a_test_send_key() {
    let hex = "0123456789abcdef0123456789abcdef";
    assert_eq!(hex.len(), TEST_KEY_HEX);
    assert!(is_test_key(&format!("{TEST_KEY_PREFIX}{hex}")));
    assert!(!is_test_key(hex), "the prefix is required");
    assert!(!is_test_key(&format!("{TEST_KEY_PREFIX}{}", &hex[..31])), "short hex");
    assert!(!is_test_key(&format!("{TEST_KEY_PREFIX}{hex}0")), "long hex");
    assert!(
        !is_test_key(&format!("{TEST_KEY_PREFIX}{}g", &hex[..31])),
        "a non-hex digit"
    );
    assert!(!is_test_key(&format!("x{TEST_KEY_PREFIX}{hex}")), "a leading byte");
}

/// The re-check lives in the ENQUEUE AUTHORITY, not at the form: a hand-posted body or a
/// future second caller is refused here rather than by whichever caller remembered to look.
/// Deleting the check leaves every form-driven test green.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_operator_send_carrying_a_key_this_page_never_minted_is_refused() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let svc = wired(&pool).await;
    let forged = unique_key(&pool, "forged").await;
    let _forged_cleanup = cleanup(&pool, &[&forged]);

    let e = svc
        .enqueue_from_admin(&mail_of(&forged, "hello"))
        .await
        .expect_err("a producer-shaped key must not reach the outbox through the admin path");
    assert_eq!(e.status, opsapi::Status::Invalid, "{}", e.msg);
    assert!(e.msg.contains("minted by the Mail page"), "got {:?}", e.msg);
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM mail.outbox WHERE idempotency_key = $1")
        .bind(&forged)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 0, "the refusal must precede the INSERT");

    // The positive control on the SAME path: a minted key IS accepted, so the refusal above
    // is not "the admin path never enqueues".
    let minted = minted_test_key(&pool).await;
    let _cleanup = cleanup(&pool, &[&minted]);
    let ok = svc.enqueue_from_admin(&mail_of(&minted, "hello")).await.unwrap();
    assert!(matches!(ok, Enqueued::Inserted(_)), "got {ok:?}");
}

// ============================================================================
// 6. The operator page: every submit arm, and the malformed-filter card.
// ============================================================================

fn params(pairs: &[(&str, &str)]) -> adminapi::Params {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

async fn park(pool: &PgPool, key: &str) {
    sqlx::query(
        "UPDATE mail.outbox SET state = 'parked', attempts = 4, last_error = 'seeded' \
          WHERE idempotency_key = $1",
    )
    .bind(key)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn requeue_returns_one_parked_row_and_a_stale_selection_is_not_success() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let svc = wired(&pool).await;
    let key = unique_key(&pool, "requeue").await;
    let _cleanup = cleanup(&pool, &[&key]);
    enqueue(&svc, &pool, &mail_of(&key, "hello")).await;
    park(&pool, &key).await;
    let id = id_of(&pool, &key).await;

    applied(
        apply_submit(&svc, params(&[(ACTION_FIELD, ACTION_REQUEUE), (MAIL_ID_FIELD, &id)])).await,
        "a parked row must requeue",
    );
    let (state, _, attempts, generation, last_error) = row_of(&pool, &key).await;
    assert_eq!(state, STATE_PENDING);
    assert_eq!(attempts, 0, "the ladder restarts");
    assert!(generation >= 1, "the requeue must BUMP the ABA guard, got {generation}");
    assert_eq!(last_error, "seeded", "the only record of why it parked must survive");

    // The same submit again: the row is no longer parked, which is a stale form and never a
    // silent success.
    let again = apply_submit(&svc, params(&[(ACTION_FIELD, ACTION_REQUEUE), (MAIL_ID_FIELD, &id)]))
        .await;
    assert!(matches!(again, Err(Rejection::Stale)), "a zero-row requeue must be Stale");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn requeue_all_parked_reports_what_it_moved_and_what_it_left() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let svc = wired(&pool).await;
    let a = unique_key(&pool, "bulk-a").await;
    let b = unique_key(&pool, "bulk-b").await;
    let _cleanup = cleanup(&pool, &[&a, &b]);
    for key in [&a, &b] {
        enqueue(&svc, &pool, &mail_of(key, "hello")).await;
        park(&pool, key).await;
    }

    let outcome = applied(
        apply_submit(&svc, params(&[(ACTION_FIELD, ACTION_REQUEUE_ALL)])).await,
        "the bulk verb must apply",
    );
    let notice = outcome.notice.expect("the bulk verb reports a count");
    assert!(notice.contains("Requeued"), "got {notice:?}");
    assert!(
        outcome.reveal.is_empty(),
        "an operational count is not a show-once secret"
    );
    for key in [&a, &b] {
        assert_eq!(row_of(&pool, key).await.0, STATE_PENDING);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_takes_a_pending_row_out_of_the_drain_and_keeps_its_body() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let svc = wired(&pool).await;
    let key = unique_key(&pool, "cancel").await;
    let _cleanup = cleanup(&pool, &[&key]);
    enqueue(&svc, &pool, &mail_of(&key, "hello")).await;
    let id = id_of(&pool, &key).await;

    applied(
        apply_submit(&svc, params(&[(ACTION_FIELD, ACTION_CANCEL), (MAIL_ID_FIELD, &id)])).await,
        "a pending row must cancel",
    );
    let (state, body, _, _, _) = row_of(&pool, &key).await;
    assert_eq!(state, STATE_CANCELLED);
    assert_eq!(
        body, "hello",
        "a cancelled body must SURVIVE — blanking it would make a durable replay a Conflict"
    );

    let again = apply_submit(&svc, params(&[(ACTION_FIELD, ACTION_CANCEL), (MAIL_ID_FIELD, &id)]))
        .await;
    assert!(matches!(again, Err(Rejection::Stale)), "a zero-row cancel must be Stale");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unmade_choice_and_an_unknown_action_are_both_stated_rejections() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let svc = wired(&pool).await;

    match apply_submit(&svc, params(&[(ACTION_FIELD, "")])).await {
        Err(Rejection::Rejected(msg)) => {
            assert!(msg.contains("no action selected"), "got {msg:?}")
        }
        other => panic!("the blank option must be refused, not defaulted to a verb: {}", label(&other)),
    }
    match apply_submit(&svc, params(&[(ACTION_FIELD, "delete-everything")])).await {
        Err(Rejection::Rejected(msg)) => {
            assert!(msg.contains("delete-everything"), "the message must name it: {msg:?}")
        }
        other => panic!("an unknown action must be refused: {}", label(&other)),
    }
    // A verb whose row selector is empty is a stated rejection too, never a statement
    // against an empty id.
    match apply_submit(&svc, params(&[(ACTION_FIELD, ACTION_REQUEUE)])).await {
        Err(Rejection::Rejected(msg)) => assert!(msg.contains(MAIL_ID_FIELD), "got {msg:?}"),
        other => panic!("a missing selection must be refused: {}", label(&other)),
    }
    // A hand-edited id never reaches the `$1::uuid` cast.
    match apply_submit(
        &svc,
        params(&[(ACTION_FIELD, ACTION_CANCEL), (MAIL_ID_FIELD, "not-a-uuid")]),
    )
    .await
    {
        Err(Rejection::Rejected(msg)) => assert!(msg.contains("outbox id"), "got {msg:?}"),
        other => panic!("a malformed id must be refused: {}", label(&other)),
    }
    // A test send whose hidden key is not the minted shape did not come from a render of
    // this page.
    match apply_submit(
        &svc,
        params(&[
            (ACTION_FIELD, ACTION_SEND_TEST),
            (TEST_TO_FIELD, "player@example.com"),
            (IDEM_TEST_FIELD, "hand-written"),
        ]),
    )
    .await
    {
        Err(Rejection::Stale) => {}
        other => panic!("an unminted key must be Stale: {}", label(&other)),
    }
}

/// `Rejection` is deliberately not `Debug` (it carries operator-facing messages), so a
/// failing submit is reported through its own rendering rather than through `expect`.
fn applied(
    outcome: Result<adminapi::SubmitOutcome, Rejection>,
    why: &str,
) -> adminapi::SubmitOutcome {
    match outcome {
        Ok(o) => o,
        other => panic!("{why}: {}", label(&other)),
    }
}

fn label(outcome: &Result<adminapi::SubmitOutcome, Rejection>) -> String {
    match outcome {
        Ok(_) => "Ok".into(),
        Err(Rejection::Stale) => "Stale".into(),
        Err(Rejection::Rejected(msg)) => format!("Rejected({msg:?})"),
        Err(Rejection::Internal(msg)) => format!("Internal({msg:?})"),
    }
}

/// A malformed filter renders an error CARD, never an `Err`: the portal forwards every
/// page's params to every provider, so an `Err` raised on another page's param would
/// degrade this item to an error card in its sidebar.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_malformed_state_filter_renders_a_card_rather_than_failing_the_page() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let svc = wired(&pool).await;

    let content = build_content(&svc, &params(&[(PARAM_STATE, "sending")]))
        .await
        .expect("a bad filter must not be an Err");
    assert!(content.table.is_none(), "the card replaces the page");
    assert_eq!(content.kpis.len(), 1);
    assert_eq!(content.kpis[0].label, "Error");
    assert!(content.kpis[0].value.contains("sending"), "{:?}", content.kpis[0].value);

    // The positive controls: no filter, and a legal one, both render the real page.
    for filter in [vec![], vec![(PARAM_STATE, STATE_PARKED)]] {
        let content = build_content(&svc, &params(&filter)).await.unwrap();
        assert!(content.table.is_some(), "{filter:?} must render the table");
        assert_eq!(content.kpis.len(), 4, "{filter:?} must render the four KPIs");
    }
}

// ============================================================================
// 7. The provider naming authority, and the undrained channel's readiness verdict.
// ============================================================================

/// `KNOWN_PROVIDERS` is what an unknown `MAIL_PROVIDER` is reported against and
/// `from_name` is what resolves one: a name in one list and not the other is drift an
/// operator meets as "unknown provider log" at boot.
#[test]
fn every_known_provider_name_resolves_and_builds_a_sender() {
    for name in KNOWN_PROVIDERS {
        let kind = ProviderKind::from_name(name)
            .unwrap_or_else(|| panic!("{name} is in KNOWN_PROVIDERS but from_name refuses it"));
        assert_eq!(kind.name(), *name, "the round trip must be stable");
        let provider = match kind {
            ProviderKind::Log => Provider::Log,
            ProviderKind::Smtp => Provider::Smtp(crate::smtp::SmtpSettings {
                host: "relay.example.com".into(),
                port: 587,
                tls: crate::smtp::TlsMode::StartTls,
                credentials: None,
            }),
        };
        let sender = provider
            .sender("noreply@example.com", Duration::from_secs(10))
            .unwrap_or_else(|e| panic!("{name} must build a sender from validated config: {e}"));
        assert_eq!(sender.name(), *name);
    }
    assert!(ProviderKind::from_name("sendgrid").is_none());
    assert!(ProviderKind::from_name("").is_none());
}

/// A process that accepts mail it can never deliver reports RED. A boot warning that
/// scrolled past is not a signal, and the message must name the variable an operator has
/// to set.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_process_with_no_provider_contributes_a_permanently_failing_readiness_check() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    ensure_schema(&pool).await;
    let transport = asyncevents::testing::transport(pool.clone());
    let ctx = Context::with_db_and_transport(pool.clone(), transport.handle());
    let m = MailModule::new();
    m.register(&ctx).unwrap();
    assert!(
        m.cfg().provider.is_none(),
        "this test binary must run with MAIL_PROVIDER unset"
    );
    m.init(&ctx).unwrap();

    let checks = ctx.contributions(httpmw::READINESS_SLOT);
    let check = checks
        .iter()
        .find(|c| c.name() == "mail")
        .expect("mail must contribute a readiness check in BOTH arms");
    let reason = check
        .run()
        .await
        .expect_err("an undrained channel is never ready");
    assert!(reason.contains("MAIL_PROVIDER"), "got {reason:?}");
    assert_eq!(reason, NO_PROVIDER_READY);
}
