//! groups tests — shared fixtures for `service_tests`/`store_tests`/`projection_tests`,
//! plus the pure (no-DB) unit tests for the cursor codec and `resolve_limit`. Unit tests
//! need no DB; the rest drive the real `Service` against the local Postgres over a fake
//! `accountsapi::Directory` (this module never imports the `accounts` impl crate —
//! fortress rule), through a real durable plane so an emitted event is a real
//! `asyncevents.events` row. Live-Postgres tests get their pool from `testdb`, which
//! FAILS the run when the local DB is unreachable (`TESTDB_ALLOW_SKIP=1` is the sole
//! local opt-out, and `verifyctl`'s `test` stage refuses it). In-crate so sibling test
//! files can drive the private `Service`/`Store` directly via `crate::tests::*`.

use super::*;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use accountsapi::{Directory, PlayerSummary};
use async_trait::async_trait;
use opsapi::{Error, Status};
use sqlx::PgPool;

use crate::service::{decode_cursor, is_uuid_text, resolve_limit, MALFORMED_CURSOR};
use groupsapi::{DEFAULT_PAGE_LIMIT, MAX_CURSOR_BYTES, MAX_PAGE_LIMIT};

pub(crate) const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

pub(crate) use testdb::test_pool;

/// ONE lock for every test that touches the live DB or the process environment:
/// `retention_days_from_env`'s cases mutate a process-global variable, and the prune
/// tests share the shared plane — so these serialize here rather than depending on the
/// caller having passed `--test-threads=1`.
pub(crate) static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Migrates BOTH the asyncevents plane and the groups schema EXACTLY ONCE per test
/// binary — concurrent idempotent DDL across parallel tests can deadlock on catalog locks.
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
            let module = Groups::new();
            module.register(&ctx).unwrap();
            module.migrate(&ctx).await.unwrap();
        })
        .await;
}

/// A real durable plane over the live pool, with a fake `Directory` filling groups'
/// sync `accounts` dependency directly (never through the real accounts impl —
/// fortress rule).
pub(crate) async fn wired(pool: &PgPool, directory: Arc<dyn Directory>) -> (Context, Arc<Service>) {
    ensure_schema(pool).await;
    let transport = asyncevents::testing::transport(pool.clone());
    let ctx = Context::with_db_and_transport(pool.clone(), transport.handle());
    let svc = Arc::new(Service::new(pool.clone(), ctx.bus().clone()));
    svc.directory.set(directory).ok().expect("directory set once");
    (ctx, svc)
}

pub(crate) async fn unique_uuid(pool: &PgPool) -> String {
    let (id,): (String,) = sqlx::query_as("SELECT gen_random_uuid()::text")
        .fetch_one(pool)
        .await
        .unwrap();
    id
}

/// Deletes every row this suite could have written for `group_ids` — both
/// `groups.memberships`/`groups.groups` and every `asyncevents.events` row whose
/// payload names one of the ids under `group_id`.
pub(crate) async fn cleanup_groups(pool: &PgPool, group_ids: &[String]) {
    if group_ids.is_empty() {
        return;
    }
    let _ = sqlx::query("DELETE FROM groups.memberships WHERE group_id = ANY($1::uuid[])")
        .bind(group_ids)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM groups.groups WHERE id = ANY($1::uuid[])")
        .bind(group_ids)
        .execute(pool)
        .await;
    let _ = sqlx::query(
        "DELETE FROM asyncevents.events WHERE payload->>'group_id' = ANY($1)",
    )
    .bind(group_ids)
    .execute(pool)
    .await;
}

/// Runs `body` on its own task and cleans up even when it panicked, then re-raises the
/// panic (a `Drop` guard cannot `await`). A red run must never leak a row into the
/// shared `groups.groups`/`groups.memberships`/`asyncevents.events` tables and fail
/// every later run.
pub(crate) async fn with_cleanup<Fut>(pool: &PgPool, group_ids: Vec<String>, body: Fut)
where
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let outcome = tokio::spawn(body).await;
    cleanup_groups(pool, &group_ids).await;
    if let Err(e) = outcome {
        if e.is_panic() {
            std::panic::resume_unwind(e.into_panic());
        }
        panic!("groups test task ended without completing: {e}");
    }
}

static SUFFIX: AtomicU64 = AtomicU64::new(0);

fn unique_suffix() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    nanos.wrapping_add(SUFFIX.fetch_add(1, Ordering::SeqCst))
}

/// A canonical-uuid-shaped, per-call-unique id — deterministic scaffolding rather than
/// left to `gen_random_uuid()` chance, so a test needing a STABLE player id across
/// several calls (the self-kick guard) can reconstruct the same value.
pub(crate) fn fixed_uuid(tier: u8) -> String {
    format!("{tier:02x}000000-0000-4000-8000-{:012x}", unique_suffix() & 0xFFFF_FFFF_FFFF)
}

/// An in-memory `accountsapi::Directory` — never the real `accounts` impl crate
/// (fortress rule: a module never imports another module's impl).
pub(crate) struct FakeDirectory {
    players: Mutex<HashMap<String, PlayerSummary>>,
    fail_lookup: AtomicBool,
    fail_handle: AtomicBool,
}

impl FakeDirectory {
    pub(crate) fn new() -> Self {
        FakeDirectory {
            players: Mutex::new(HashMap::new()),
            fail_lookup: AtomicBool::new(false),
            fail_handle: AtomicBool::new(false),
        }
    }

    pub(crate) fn insert(&self, id: &str, handle: &str) {
        self.players.lock().unwrap().insert(
            id.to_string(),
            PlayerSummary {
                player_id: id.to_string(),
                display_name: handle.to_string(),
                handle: handle.to_string(),
                online_until: String::new(),
            },
        );
    }

}

#[async_trait]
impl Directory for FakeDirectory {
    async fn players_by_id(&self, ids: Vec<String>) -> Result<Vec<PlayerSummary>, Error> {
        if self.fail_lookup.load(Ordering::SeqCst) {
            return Err(Error::internal("fake directory down"));
        }
        let players = self.players.lock().unwrap();
        Ok(ids
            .iter()
            .filter_map(|id| {
                players.iter().find(|(k, _)| k.eq_ignore_ascii_case(id)).map(|(_, v)| v.clone())
            })
            .collect())
    }

    async fn find_by_handle(&self, handle: String) -> Result<Option<PlayerSummary>, Error> {
        if self.fail_handle.load(Ordering::SeqCst) {
            return Err(Error::internal("fake directory down"));
        }
        let players = self.players.lock().unwrap();
        Ok(players.values().find(|p| p.handle.eq_ignore_ascii_case(&handle)).cloned())
    }
}

// ============================================================================
// Cursor codec — pure, no DB. Every reject arm is testable without a database
// (`service::decode_cursor`'s own doc comment states this).
// ============================================================================

#[test]
fn resolve_limit_zero_is_the_default() {
    assert_eq!(resolve_limit(0).unwrap(), DEFAULT_PAGE_LIMIT);
}

#[test]
fn resolve_limit_above_max_is_clamped_not_rejected() {
    assert_eq!(resolve_limit(MAX_PAGE_LIMIT + 500).unwrap(), MAX_PAGE_LIMIT);
}

#[test]
fn resolve_limit_negative_is_invalid() {
    let err = resolve_limit(-1).unwrap_err();
    assert_eq!(err.status, Status::Invalid);
}

/// Deliberately not valid base64 (all `!`): if the cap check ran AFTER the decode
/// attempt, this would fail with `MALFORMED_CURSOR` instead — the MESSAGE is what
/// proves the ORDER, not just that both arms answer `Invalid`.
#[test]
fn cursor_over_cap_is_rejected_before_decode_is_attempted() {
    let oversized = "!".repeat(MAX_CURSOR_BYTES + 1);
    let err = decode_cursor(&oversized).unwrap_err();
    assert_eq!(err.status, Status::Invalid);
    assert!(
        err.msg.contains("exceeds"),
        "the cap must be checked before the decode, got: {}",
        err.msg
    );
}

#[test]
fn empty_cursor_is_the_first_page() {
    assert_eq!(decode_cursor("").unwrap(), None);
}

#[test]
fn non_base64_cursor_is_malformed() {
    let err = decode_cursor("not valid base64!!").unwrap_err();
    assert_eq!(err.status, Status::Invalid);
    assert_eq!(err.msg, MALFORMED_CURSOR);
}

/// Valid base64, but the decoded bytes are not valid UTF-8 — the `String::from_utf8`
/// arm, distinct from a plain base64 decode failure.
#[test]
fn non_utf8_cursor_payload_is_malformed() {
    use base64::Engine;
    const B64: base64::engine::general_purpose::GeneralPurpose =
        base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let cursor = B64.encode([0xff, 0xfe, 0xfd]);
    let err = decode_cursor(&cursor).unwrap_err();
    assert_eq!(err.status, Status::Invalid);
    assert_eq!(err.msg, MALFORMED_CURSOR);
}

/// Valid base64, valid UTF-8, but no `|` separator — the `split_once` arm.
#[test]
fn cursor_missing_separator_is_malformed() {
    use base64::Engine;
    const B64: base64::engine::general_purpose::GeneralPurpose =
        base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let cursor = B64.encode("2026-01-01T00:00:00.000000Z-no-pipe-here");
    let err = decode_cursor(&cursor).unwrap_err();
    assert_eq!(err.status, Status::Invalid);
    assert_eq!(err.msg, MALFORMED_CURSOR);
}

fn encoded(created_at: &str, id: &str) -> String {
    crate::service::encode_cursor(created_at, id)
}

/// A digit-SHAPED but calendar-impossible date: day 30 of February. If the calendar
/// check were absent, this would reach `$2::timestamptz` and raise `22008`
/// (uncaught -> 500), contradicting the contract's 400-for-malformed-cursor promise.
#[test]
fn cursor_with_february_30_is_malformed() {
    let cursor = encoded("2026-02-30T00:00:00.000000Z", "00000000-0000-4000-8000-000000000000");
    let err = decode_cursor(&cursor).unwrap_err();
    assert_eq!(err.status, Status::Invalid);
    assert_eq!(err.msg, MALFORMED_CURSOR);
}

#[test]
fn cursor_with_hour_25_is_malformed() {
    let cursor = encoded("2026-01-01T25:00:00.000000Z", "00000000-0000-4000-8000-000000000000");
    let err = decode_cursor(&cursor).unwrap_err();
    assert_eq!(err.status, Status::Invalid);
    assert_eq!(err.msg, MALFORMED_CURSOR);
}

#[test]
fn cursor_with_year_0000_is_malformed() {
    let cursor = encoded("0000-01-01T00:00:00.000000Z", "00000000-0000-4000-8000-000000000000");
    let err = decode_cursor(&cursor).unwrap_err();
    assert_eq!(err.status, Status::Invalid);
    assert_eq!(err.msg, MALFORMED_CURSOR);
}

#[test]
fn cursor_with_non_uuid_tail_is_malformed() {
    let cursor = encoded("2026-01-01T00:00:00.000000Z", "not-a-uuid-at-all-not-a-uuid-at-al");
    let err = decode_cursor(&cursor).unwrap_err();
    assert_eq!(err.status, Status::Invalid);
    assert_eq!(err.msg, MALFORMED_CURSOR);
}

/// A well-formed cursor decodes to the pair it was encoded from — the round trip a
/// reject-only suite never proves on its own.
#[test]
fn a_well_formed_cursor_round_trips() {
    let cursor = encoded("2026-01-01T00:00:00.000000Z", "00000000-0000-4000-8000-000000000000");
    assert_eq!(
        decode_cursor(&cursor).unwrap(),
        Some((
            "2026-01-01T00:00:00.000000Z".to_string(),
            "00000000-0000-4000-8000-000000000000".to_string()
        ))
    );
}

#[test]
fn is_uuid_text_rejects_wrong_shapes() {
    assert!(is_uuid_text("00000000-0000-4000-8000-000000000000"));
    assert!(!is_uuid_text("not-a-uuid"));
    assert!(!is_uuid_text("00000000-0000-4000-8000-00000000000")); // one short
}
