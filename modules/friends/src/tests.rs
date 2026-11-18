//! friends tests. Unit tests for the pure cursor/limit codecs need no DB; the rest drive
//! the real `Service` against the local Postgres over a fake `accountsapi::Directory` (this
//! module never imports the `accounts` impl crate — fortress rule), through a real durable
//! plane so an emitted event is a real `asyncevents.events` row. Live-Postgres tests SKIP
//! cleanly when the local DB is unreachable. In-crate so they can drive the private
//! `Service`/`Store` directly.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use super::*;

use accountsapi::{Directory, PlayerSummary};
use bus::{AnyTx, Delivery, TxHandler};
use friendsapi::{
    Player as _, DEFAULT_PAGE_LIMIT, MAX_CURSOR_BYTES, MAX_PAGE_LIMIT, MAX_PENDING_OUTSTANDING,
    STATE_ACCEPTED, STATE_PENDING,
};
use friendsevents::{REASON_DECLINED, REASON_UNFRIENDED, REASON_WITHDRAWN};
use opsapi::{Error, Identity, Status};
use sqlx::PgPool;

use crate::service::{decode_cursor, is_uuid_text, resolve_limit};

const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

use testdb::test_pool;

/// `sweep_purged_players` scans the WHOLE shared `friends.purged_players` table (not just
/// one test's rows) and `pending_purges()` counts across it too — an unfiltered `count(*)`,
/// not scoped to any one test's ids. Every purge test that plants a row (even one that never
/// sweeps) or reads that count must serialize with every OTHER such test: a concurrent
/// plant landing inside a before/after counting window inflates it, and a concurrent sweep
/// can race another sweep's `FOR UPDATE SKIP LOCKED` batch over the same shared rows.
/// Mirrors `groups`' `purge_tests::DB_LOCK`, widened here because `friends` also has a
/// whole-table counting test `groups` does not.
pub(crate) static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn ensure_schema(pool: &PgPool) {
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    asyncevents::Plane::new(pool.clone(), dsn)
        .unwrap()
        .migrate()
        .await
        .unwrap();
    let ctx = Context::with_db(pool.clone());
    let module = Friends::new();
    module.register(&ctx).unwrap();
    module.migrate(&ctx).await.unwrap();
}

/// A real durable plane over the live pool, with a fake `Directory` filling friends' sync
/// `accounts` dependency directly (never through the real accounts impl — fortress rule).
async fn wired(pool: &PgPool, directory: Arc<dyn Directory>) -> (Context, Arc<Service>) {
    ensure_schema(pool).await;
    let transport = asyncevents::testing::transport(pool.clone());
    let ctx = Context::with_db_and_transport(pool.clone(), transport.handle());
    let svc = Arc::new(Service::new(pool.clone(), ctx.bus().clone()));
    svc.directory.set(directory).ok().expect("directory set once");
    (ctx, svc)
}

/// Like [`wired`], but every durable append fails ([`asyncevents::testing::failing_transport`])
/// — for proving `emit_tx` shares the store's transaction (item 14).
async fn wired_with_failing_bus(pool: &PgPool, directory: Arc<dyn Directory>) -> Arc<Service> {
    ensure_schema(pool).await;
    let ctx = Context::with_db_and_transport(pool.clone(), asyncevents::testing::failing_transport());
    let svc = Arc::new(Service::new(pool.clone(), ctx.bus().clone()));
    svc.directory.set(directory).ok().expect("directory set once");
    svc
}

static SUFFIX: AtomicU64 = AtomicU64::new(0);

fn unique_suffix() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    nanos.wrapping_add(SUFFIX.fetch_add(1, Ordering::SeqCst))
}

/// A canonical-uuid-shaped id whose FIRST byte is `tier`, so two calls with different tiers
/// sort exactly as their tiers do (both as text and as `uuid`, since Postgres uuid
/// comparison is bytewise and the canonical hex text encodes those bytes left-to-right) —
/// deterministic, not left to `gen_random_uuid()` chance, because the once-wrong predicate
/// this module fixes depended on which side of the pair sorts low.
fn fixed_uuid(tier: u8) -> String {
    // The last group is exactly 12 hex digits, so the suffix is masked to 48 bits —
    // `unique_suffix()`'s full 64 bits would overflow the group and break the uuid shape.
    format!("{tier:02x}000000-0000-4000-8000-{:012x}", unique_suffix() & 0xFFFF_FFFF_FFFF)
}

fn low_uuid() -> String {
    fixed_uuid(0x00)
}
fn high_uuid() -> String {
    fixed_uuid(0xff)
}

/// Deletes every row/event this suite could have written for `ids` — both `friends.edges`
/// (by either side of the pair) and every `asyncevents.events` row whose payload names one
/// of `ids` under any of the four id fields the three payload shapes use.
async fn cleanup_ids(pool: &PgPool, ids: &[String]) {
    if ids.is_empty() {
        return;
    }
    let _ = sqlx::query(
        "DELETE FROM friends.edges WHERE low_id = ANY($1::uuid[]) OR high_id = ANY($1::uuid[])",
    )
    .bind(ids)
    .execute(pool)
    .await;
    let _ = sqlx::query(
        "DELETE FROM asyncevents.events WHERE \
           payload->>'requester_id' = ANY($1) OR payload->>'addressee_id' = ANY($1) \
           OR payload->>'actor_id' = ANY($1) OR payload->>'other_id' = ANY($1)",
    )
    .bind(ids)
    .execute(pool)
    .await;
}

/// Runs `body` on its own task and cleans up even when it panicked, then re-raises the
/// panic (a `Drop` guard cannot `await`, so it cannot reach the shared `asyncevents` log's
/// cleanup) — mirrors `leaderboard`'s `with_cleanup`. A red run must never leak a row into
/// the shared `friends.edges`/`asyncevents.events` tables and fail every later run.
async fn with_cleanup<Fut>(pool: &PgPool, ids: Vec<String>, body: Fut)
where
    Fut: Future<Output = ()> + Send + 'static,
{
    let outcome = tokio::spawn(body).await;
    cleanup_ids(pool, &ids).await;
    if let Err(e) = outcome {
        if e.is_panic() {
            std::panic::resume_unwind(e.into_panic());
        }
        panic!("friends test task ended without completing: {e}");
    }
}

/// An in-memory `accountsapi::Directory` — never the real `accounts` impl crate (fortress
/// rule: a module never imports another module's impl). The two `fail_*` flags are
/// independent so a test can fail exactly ONE of the two calls `request` makes
/// (`find_by_handle` then `players_by_id`) and pin each call site separately.
struct FakeDirectory {
    players: Mutex<HashMap<String, PlayerSummary>>,
    fail_lookup: AtomicBool,
    fail_handle: AtomicBool,
}

impl FakeDirectory {
    fn new() -> Self {
        FakeDirectory {
            players: Mutex::new(HashMap::new()),
            fail_lookup: AtomicBool::new(false),
            fail_handle: AtomicBool::new(false),
        }
    }

    fn insert(&self, id: &str, handle: &str) {
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

    fn remove(&self, id: &str) {
        self.players.lock().unwrap().remove(id);
    }

    /// Fails BOTH methods — the coarse switch most tests want.
    fn set_failing(&self, failing: bool) {
        self.fail_lookup.store(failing, Ordering::SeqCst);
        self.fail_handle.store(failing, Ordering::SeqCst);
    }

    fn set_failing_lookup(&self, failing: bool) {
        self.fail_lookup.store(failing, Ordering::SeqCst);
    }

    fn set_failing_handle(&self, failing: bool) {
        self.fail_handle.store(failing, Ordering::SeqCst);
    }
}

#[async_trait]
impl Directory for FakeDirectory {
    // Deliberately NOT Error::unavailable: production's directory_unavailable mapping
    // (service.rs) is the ONLY thing allowed to produce Status::Unavailable, so the
    // fake must inject a DIFFERENT status here — otherwise a test asserting Unavailable
    // would stay green even if that mapping were deleted and the fake's own status
    // propagated untouched through a bare `?`.
    async fn players_by_id(&self, ids: Vec<String>) -> Result<Vec<PlayerSummary>, Error> {
        if self.fail_lookup.load(Ordering::SeqCst) {
            return Err(Error::internal("fake directory down"));
        }
        let players = self.players.lock().unwrap();
        // Case-INSENSITIVE, matching the real directory's Postgres `::uuid` cast (both
        // spellings parse to the same 16 bytes) — a byte-exact `HashMap::get` here would
        // make the fake reject an uppercase id the real accounts service accepts.
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

async fn state_of(pool: &PgPool, edge_id: &str) -> String {
    sqlx::query_scalar("SELECT state FROM friends.edges WHERE id = $1::uuid")
        .bind(edge_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn row_count_for_pair(pool: &PgPool, a: &str, b: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM friends.edges \
          WHERE low_id = least($1::uuid, $2::uuid) AND high_id = greatest($1::uuid, $2::uuid)",
    )
    .bind(a)
    .bind(b)
    .fetch_one(pool)
    .await
    .unwrap()
}

// ============================================================================
// Item 13 — paging semantics (pure, no DB): `limit`/cursor decode.
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

/// Deliberately not valid base64 (all `!`): if the cap check ran AFTER the decode attempt,
/// this would fail with the MALFORMED_CURSOR message instead — the message is what proves
/// the ORDER, not just that both arms answer `Invalid`.
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
fn is_uuid_text_rejects_wrong_shapes() {
    assert!(is_uuid_text("00000000-0000-4000-8000-000000000000"));
    assert!(!is_uuid_text("not-a-uuid"));
}

// ============================================================================
// Item 1 — canonicalization: both request directions produce ONE row, low_id < high_id.
// ============================================================================

#[tokio::test]
async fn request_canonicalizes_the_pair_regardless_of_which_side_calls() {
    let Some(pool) = test_pool().await else { return };
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;

    // Direction A: the low_id side calls `request`.
    let (low_a, high_a) = (low_uuid(), high_uuid());
    dir.insert(&low_a, "LowA#0001");
    dir.insert(&high_a, "HighA#0002");
    // Direction B: the high_id side calls `request`.
    let (low_b, high_b) = (low_uuid(), high_uuid());
    dir.insert(&low_b, "LowB#0003");
    dir.insert(&high_b, "HighB#0004");

    let ids = vec![low_a.clone(), high_a.clone(), low_b.clone(), high_b.clone()];
    let (p, la, ha, lb, hb) = (pool.clone(), low_a, high_a, low_b, high_b);
    with_cleanup(&pool, ids, async move {
        svc.request(Identity::player(&la), "HighA#0002".into()).await.unwrap();
        svc.request(Identity::player(&hb), "LowB#0003".into()).await.unwrap();

        let row_a: (String, String) = sqlx::query_as(
            "SELECT low_id::text, high_id::text FROM friends.edges \
              WHERE low_id = $1::uuid AND high_id = $2::uuid",
        )
        .bind(&la)
        .bind(&ha)
        .fetch_one(&p)
        .await
        .unwrap();
        assert_eq!(row_a, (la.clone(), ha.clone()), "direction A must land low<high");
        assert_eq!(row_count_for_pair(&p, &la, &ha).await, 1);

        let row_b: (String, String) = sqlx::query_as(
            "SELECT low_id::text, high_id::text FROM friends.edges \
              WHERE low_id = $1::uuid AND high_id = $2::uuid",
        )
        .bind(&lb)
        .bind(&hb)
        .fetch_one(&p)
        .await
        .unwrap();
        assert_eq!(row_b, (lb.clone(), hb.clone()), "direction B must ALSO land low<high");
        assert_eq!(row_count_for_pair(&p, &lb, &hb).await, 1);
    })
    .await;
}

// ============================================================================
// Item 2 — consent: accept by the requester is NotFound, accept by the addressee
// succeeds, on BOTH uuid orderings. Regresses if the predicate ever goes back to a
// `high_id`-keyed check (F4's original bug): one of these two tests would then see the
// WRONG side win.
// ============================================================================

async fn accept_consent_case(requester_is_low: bool) {
    let Some(pool) = test_pool().await else { return };
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;

    let (low, high) = (low_uuid(), high_uuid());
    dir.insert(&low, "Lo#0001");
    dir.insert(&high, "Hi#0002");
    let (requester, addressee, target_handle) = if requester_is_low {
        (low.clone(), high.clone(), "Hi#0002")
    } else {
        (high.clone(), low.clone(), "Lo#0001")
    };

    let ids = vec![low.clone(), high.clone()];
    let (p, req, addr) = (pool.clone(), requester, addressee);
    let th = target_handle.to_string();
    with_cleanup(&pool, ids, async move {
        let f = svc.request(Identity::player(&req), th).await.unwrap();
        assert_eq!(f.state, STATE_PENDING);

        let by_requester = svc.accept(Identity::player(&req), f.edge_id.clone()).await;
        assert_eq!(
            by_requester.unwrap_err().status,
            Status::NotFound,
            "the requester must never accept its own request"
        );
        assert_eq!(
            state_of(&p, &f.edge_id).await,
            STATE_PENDING,
            "a failed accept must not mutate state"
        );

        svc.accept(Identity::player(&addr), f.edge_id.clone()).await.unwrap();
        assert_eq!(state_of(&p, &f.edge_id).await, STATE_ACCEPTED, "the addressee's accept must succeed");
    })
    .await;
}

#[tokio::test]
async fn accept_consent_holds_when_requester_is_the_low_id() {
    accept_consent_case(true).await;
}

#[tokio::test]
async fn accept_consent_holds_when_requester_is_the_high_id() {
    accept_consent_case(false).await;
}

// ============================================================================
// Item 3 — accept by a third party is NotFound, and leaves the edge untouched.
// ============================================================================

#[tokio::test]
async fn accept_by_third_party_is_not_found() {
    let Some(pool) = test_pool().await else { return };
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;

    let (low, high, third) = (low_uuid(), high_uuid(), fixed_uuid(0x77));
    dir.insert(&low, "Lo#0001");
    dir.insert(&high, "Hi#0002");
    dir.insert(&third, "Third#0009");

    let ids = vec![low.clone(), high.clone(), third.clone()];
    let (p, lo, hi, tp) = (pool.clone(), low, high, third);
    with_cleanup(&pool, ids, async move {
        let f = svc.request(Identity::player(&lo), "Hi#0002".into()).await.unwrap();

        let err = svc.accept(Identity::player(&tp), f.edge_id.clone()).await.unwrap_err();
        assert_eq!(err.status, Status::NotFound);
        assert_eq!(state_of(&p, &f.edge_id).await, STATE_PENDING);

        // The party clause exists TWICE (view_edge's pre-read AND accept_tx's own
        // WHERE) — drive Store::accept_tx directly so this defence layer is pinned on
        // its own, independent of view_edge's.
        let mut probe = p.begin().await.unwrap();
        let direct = svc
            .store
            .accept_tx(&mut probe, &f.edge_id, &tp, STATE_PENDING, STATE_ACCEPTED)
            .await
            .unwrap();
        assert_eq!(direct, None, "accept_tx's own party clause must independently refuse a third party");
        probe.rollback().await.unwrap();

        // sanity: the real addressee can still accept afterwards.
        svc.accept(Identity::player(&hi), f.edge_id.clone()).await.unwrap();
        assert_eq!(state_of(&p, &f.edge_id).await, STATE_ACCEPTED);
    })
    .await;
}

// ============================================================================
// Item 4 — duplicate own request is a no-op, not an accept, and emits NOTHING.
// Regresses if the conflict-branch predicate is ever relaxed to admit the caller's own
// duplicate (it would then read STATE_ACCEPTED here, or a second `friend.requested`).
// ============================================================================

#[tokio::test]
async fn duplicate_own_pending_request_is_noop_and_emits_nothing() {
    let Some(pool) = test_pool().await else { return };
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;

    let (low, high) = (low_uuid(), high_uuid());
    dir.insert(&low, "Lo#0001");
    dir.insert(&high, "Hi#0002");

    let ids = vec![low.clone(), high.clone()];
    let (p, lo, hi) = (pool.clone(), low.clone(), high.clone());
    with_cleanup(&pool, ids, async move {
        let f1 = svc.request(Identity::player(&lo), "Hi#0002".into()).await.unwrap();
        assert_eq!(f1.state, STATE_PENDING);

        let f2 = svc.request(Identity::player(&lo), "Hi#0002".into()).await.unwrap();
        assert_eq!(
            f2.state, STATE_PENDING,
            "a repeated own request must stay pending, never auto-accept"
        );
        assert_eq!(f2.edge_id, f1.edge_id);
        assert_eq!(row_count_for_pair(&p, &lo, &hi).await, 1, "the repeat must not create a second row");

        let requested = asyncevents::testing::events_count(&p, "friend.requested", "requester_id", &lo)
            .await
            .unwrap();
        assert_eq!(requested, 1, "the duplicate must not emit a second friend.requested");
        let accepted = asyncevents::testing::events_count(&p, "friend.accepted", "requester_id", &lo)
            .await
            .unwrap();
        assert_eq!(accepted, 0, "a duplicate own request must never auto-accept or emit friend.accepted");
    })
    .await;
}

// ============================================================================
// Item — `request`'s OWN two `directory_unavailable` call sites (service.rs's
// `find_by_handle` and the caller's `players_by_id`), distinct from `list`/`pending`'s.
// ============================================================================

#[tokio::test]
async fn request_directory_failure_on_handle_lookup_is_unavailable() {
    let Some(pool) = test_pool().await else { return };
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;

    let (me, target) = (low_uuid(), high_uuid());
    dir.insert(&me, "Me#0001");
    dir.insert(&target, "Target#0002");
    dir.set_failing_handle(true);

    let ids = vec![me.clone(), target.clone()];
    let (p, m, t) = (pool.clone(), me, target);
    with_cleanup(&pool, ids, async move {
        let err = svc.request(Identity::player(&m), "Target#0002".into()).await.unwrap_err();
        assert_eq!(err.status, Status::Unavailable);
        assert_eq!(row_count_for_pair(&p, &m, &t).await, 0, "a failed handle lookup must write nothing");
    })
    .await;
}

#[tokio::test]
async fn request_directory_failure_on_caller_lookup_is_unavailable() {
    let Some(pool) = test_pool().await else { return };
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;

    let (me, target) = (low_uuid(), high_uuid());
    dir.insert(&me, "Me#0001");
    dir.insert(&target, "Target#0002");
    // `find_by_handle` (the target) must still succeed; only the CALLER's own
    // `players_by_id` lookup fails.
    dir.set_failing_lookup(true);

    let ids = vec![me.clone(), target.clone()];
    let (p, m, t) = (pool.clone(), me, target);
    with_cleanup(&pool, ids, async move {
        let err = svc.request(Identity::player(&m), "Target#0002".into()).await.unwrap_err();
        assert_eq!(err.status, Status::Unavailable);
        assert_eq!(row_count_for_pair(&p, &m, &t).await, 0, "a failed caller lookup must write nothing");
    })
    .await;
}

// ============================================================================
// Item 5 + 6 — a reverse-pending request auto-accepts and emits `friend.accepted`
// EXACTLY once, with the ORIGINAL roles; a further request once already accepted is
// also a no-op with no second emission. Both uuid orderings for the crossing accept.
// ============================================================================

async fn crossing_case(first_requester_is_low: bool) {
    let Some(pool) = test_pool().await else { return };
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;

    let (low, high) = (low_uuid(), high_uuid());
    dir.insert(&low, "Lo#0001");
    dir.insert(&high, "Hi#0002");
    let (first, first_handle, second, second_handle) = if first_requester_is_low {
        (low.clone(), "Lo#0001", high.clone(), "Hi#0002")
    } else {
        (high.clone(), "Hi#0002", low.clone(), "Lo#0001")
    };

    let ids = vec![low.clone(), high.clone()];
    let (p, f, s) = (pool.clone(), first.clone(), second.clone());
    let sh = second_handle.to_string();
    let fh = first_handle.to_string();
    with_cleanup(&pool, ids, async move {
        let f1 = svc.request(Identity::player(&f), sh).await.unwrap();
        assert_eq!(f1.state, STATE_PENDING);

        let f2 = svc.request(Identity::player(&s), fh.clone()).await.unwrap();
        assert_eq!(f2.state, STATE_ACCEPTED, "the crossing request must auto-accept");
        assert_eq!(f2.edge_id, f1.edge_id, "both directions address the SAME row");

        let accepted_count =
            asyncevents::testing::events_count(&p, "friend.accepted", "requester_id", &f).await.unwrap();
        assert_eq!(accepted_count, 1, "exactly one friend.accepted for this edge");

        let (req_id, addr_id): (String, String) = sqlx::query_as(
            "SELECT payload->>'requester_id', payload->>'addressee_id' FROM asyncevents.events \
              WHERE topic = 'friend.accepted' AND payload->>'edge_id' = $1",
        )
        .bind(&f1.edge_id)
        .fetch_one(&p)
        .await
        .unwrap();
        assert_eq!(req_id, f, "requester_id must stay the ORIGINAL requester");
        assert_eq!(addr_id, s, "addressee_id must stay the ORIGINAL addressee");

        // Item 6: once accepted, a further request from EITHER side is a no-op — no
        // second friend.accepted, whichever side calls again.
        let repeat_by_first = svc.request(Identity::player(&f), second_handle_of(&dir, &s)).await.unwrap();
        assert_eq!(repeat_by_first.state, STATE_ACCEPTED);
        let repeat_by_second = svc.request(Identity::player(&s), second_handle_of(&dir, &f)).await.unwrap();
        assert_eq!(repeat_by_second.state, STATE_ACCEPTED);

        // Keyed on `edge_id`, NOT `requester_id`: a spurious no-op emission that (wrongly)
        // stamps `requester_id` as the CALLER rather than the original requester would
        // still count as 1 here if we kept keying on `requester_id` — the field whose
        // wrongness the bug this test guards against actually is.
        let accepted_count_after =
            asyncevents::testing::events_count(&p, "friend.accepted", "edge_id", &f1.edge_id).await.unwrap();
        assert_eq!(accepted_count_after, 1, "an already-accepted edge must never emit a second friend.accepted");
    })
    .await;
}

/// Reads `id`'s handle back out of the fake directory — used to repeat a request against
/// the ORIGINAL target after the pair is already accepted, without hardcoding which
/// fixture handle belongs to which side.
fn second_handle_of(dir: &FakeDirectory, id: &str) -> String {
    dir.players.lock().unwrap().get(id).unwrap().handle.clone()
}

#[tokio::test]
async fn crossing_request_auto_accepts_once_when_first_requester_is_low() {
    crossing_case(true).await;
}

#[tokio::test]
async fn crossing_request_auto_accepts_once_when_first_requester_is_high() {
    crossing_case(false).await;
}

// ============================================================================
// Item 7 — self-friend is refused BEFORE any write: no row, no event.
// ============================================================================

#[tokio::test]
async fn self_friend_request_is_refused_before_any_write() {
    let Some(pool) = test_pool().await else { return };
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;

    let me = low_uuid();
    dir.insert(&me, "Me#0001");

    let ids = vec![me.clone()];
    let (p, m) = (pool.clone(), me.clone());
    with_cleanup(&pool, ids, async move {
        let err = svc.request(Identity::player(&m), "Me#0001".into()).await.unwrap_err();
        assert_eq!(err.status, Status::Invalid);

        let rows: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM friends.edges WHERE low_id = $1::uuid OR high_id = $1::uuid",
        )
        .bind(&m)
        .fetch_one(&p)
        .await
        .unwrap();
        assert_eq!(rows, 0, "a refused self-request must never reach the store");

        let events = asyncevents::testing::events_count(&p, "friend.requested", "requester_id", &m)
            .await
            .unwrap();
        assert_eq!(events, 0);
    })
    .await;
}

// ============================================================================
// Item 8 — decline/remove by a third party is NotFound.
// ============================================================================

#[tokio::test]
async fn decline_by_third_party_is_not_found() {
    let Some(pool) = test_pool().await else { return };
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;

    let (low, high, third) = (low_uuid(), high_uuid(), fixed_uuid(0x66));
    dir.insert(&low, "Lo#0001");
    dir.insert(&high, "Hi#0002");
    dir.insert(&third, "Third#0008");

    let ids = vec![low.clone(), high.clone(), third.clone()];
    let (p, lo, tp) = (pool.clone(), low, third);
    with_cleanup(&pool, ids, async move {
        let f = svc.request(Identity::player(&lo), "Hi#0002".into()).await.unwrap();

        let err = svc.decline(Identity::player(&tp), f.edge_id.clone()).await.unwrap_err();
        assert_eq!(err.status, Status::NotFound);
        assert_eq!(state_of(&p, &f.edge_id).await, STATE_PENDING, "a third party's decline must not touch the row");

        // Same double-defence pin as accept: decline_tx's OWN party clause, independent
        // of view_edge's pre-read.
        let mut probe = p.begin().await.unwrap();
        let direct = svc.store.decline_tx(&mut probe, &f.edge_id, &tp, STATE_PENDING).await.unwrap();
        assert_eq!(direct, None, "decline_tx's own party clause must independently refuse a third party");
        probe.rollback().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn remove_by_third_party_is_not_found() {
    let Some(pool) = test_pool().await else { return };
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;

    let (low, high, third) = (low_uuid(), high_uuid(), fixed_uuid(0x55));
    dir.insert(&low, "Lo#0001");
    dir.insert(&high, "Hi#0002");
    dir.insert(&third, "Third#0007");

    let ids = vec![low.clone(), high.clone(), third.clone()];
    let (p, lo, hi, tp) = (pool.clone(), low, high, third);
    with_cleanup(&pool, ids, async move {
        let f = svc.request(Identity::player(&lo), "Hi#0002".into()).await.unwrap();
        svc.accept(Identity::player(&hi), f.edge_id.clone()).await.unwrap();

        let err = svc.remove(Identity::player(&tp), f.edge_id.clone()).await.unwrap_err();
        assert_eq!(err.status, Status::NotFound);
        assert_eq!(state_of(&p, &f.edge_id).await, STATE_ACCEPTED, "a third party's remove must not touch the row");

        // Same double-defence pin as accept/decline: delete_tx's OWN party clause,
        // independent of view_edge's pre-read.
        let mut probe = p.begin().await.unwrap();
        let direct = svc.store.delete_tx(&mut probe, &f.edge_id, &tp).await.unwrap();
        assert_eq!(direct, None, "delete_tx's own party clause must independently refuse a third party");
        probe.rollback().await.unwrap();
    })
    .await;
}

// ============================================================================
// Item 9 — the outstanding cap refuses the request that would exceed
// MAX_PENDING_OUTSTANDING, and a REPEAT by a caller already at the cap is still 201.
// ============================================================================

#[tokio::test]
async fn outstanding_cap_refuses_new_request_but_a_repeat_stays_201() {
    let Some(pool) = test_pool().await else { return };
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;

    let me = low_uuid();
    dir.insert(&me, "CapMe#0001");
    // 99 rows seeded DIRECTLY (bulk insert), not through 99 real `request` calls: the
    // boundary this test proves only needs the 100th and 101st calls to run through
    // production. Both extra fixtures are minted BEFORE `with_cleanup` and folded into
    // its id list, so a panic mid-body (e.g. under a reverted cap) still cleans them up.
    let seeded_targets: Vec<String> = (0..MAX_PENDING_OUTSTANDING - 1).map(|_| high_uuid()).collect();
    let hundredth_target = high_uuid();
    let overflow_target = high_uuid();
    dir.insert(&hundredth_target, "CapHundredth#0001");
    dir.insert(&overflow_target, "CapOverflow#0001");

    let mut ids = vec![me.clone(), hundredth_target, overflow_target.clone()];
    ids.extend(seeded_targets.iter().cloned());
    let (p, m, seeded, overflow) = (pool.clone(), me.clone(), seeded_targets, overflow_target);
    with_cleanup(&pool, ids, async move {
        sqlx::query(
            "INSERT INTO friends.edges (id, low_id, high_id, requester_id, state) \
             SELECT gen_random_uuid(), least($1::uuid, t), greatest($1::uuid, t), $1::uuid, 'pending' \
               FROM unnest($2::uuid[]) AS t",
        )
        .bind(&m)
        .bind(&seeded)
        .execute(&p)
        .await
        .unwrap();

        let outstanding_before: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM friends.edges WHERE requester_id = $1::uuid AND state = $2",
        )
        .bind(&m)
        .bind(STATE_PENDING)
        .fetch_one(&p)
        .await
        .unwrap();
        assert_eq!(outstanding_before, MAX_PENDING_OUTSTANDING - 1, "fixture must seed one row BELOW the cap");

        // The 100th request, driven through production, must succeed and land the caller
        // EXACTLY at the cap.
        let hundredth = svc.request(Identity::player(&m), "CapHundredth#0001".into()).await.unwrap();
        assert_eq!(hundredth.state, STATE_PENDING);
        let outstanding: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM friends.edges WHERE requester_id = $1::uuid AND state = $2",
        )
        .bind(&m)
        .bind(STATE_PENDING)
        .fetch_one(&p)
        .await
        .unwrap();
        assert_eq!(outstanding, MAX_PENDING_OUTSTANDING, "the 100th request must land the caller EXACTLY at the cap");

        // The 101st (a brand-new target) must be refused.
        let err = svc.request(Identity::player(&m), "CapOverflow#0001".into()).await.unwrap_err();
        assert_eq!(err.status, Status::Conflict, "the request that would exceed the cap must be refused");
        assert_eq!(row_count_for_pair(&p, &m, &overflow).await, 0, "the refused request must not be written");

        // A REPEAT of an existing pending request must still be 201, never 409.
        let repeat = svc.request(Identity::player(&m), "CapHundredth#0001".into()).await.unwrap();
        assert_eq!(repeat.state, STATE_PENDING, "a repeat at the cap must never be reported as a conflict");
    })
    .await;
}

// ============================================================================
// Item 10 — `remove` emits the reason of the DELETED row's state/authorship.
// ============================================================================

#[tokio::test]
async fn remove_reason_is_unfriended_for_an_accepted_edge() {
    let Some(pool) = test_pool().await else { return };
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;

    let (low, high) = (low_uuid(), high_uuid());
    dir.insert(&low, "Lo#0001");
    dir.insert(&high, "Hi#0002");

    let ids = vec![low.clone(), high.clone()];
    let (p, lo, hi) = (pool.clone(), low, high);
    with_cleanup(&pool, ids, async move {
        let f = svc.request(Identity::player(&lo), "Hi#0002".into()).await.unwrap();
        svc.accept(Identity::player(&hi), f.edge_id.clone()).await.unwrap();

        svc.remove(Identity::player(&hi), f.edge_id.clone()).await.unwrap();
        let reason: String = sqlx::query_scalar(
            "SELECT payload->>'reason' FROM asyncevents.events \
              WHERE topic = 'friend.removed' AND payload->>'edge_id' = $1",
        )
        .bind(&f.edge_id)
        .fetch_one(&p)
        .await
        .unwrap();
        assert_eq!(reason, REASON_UNFRIENDED);
    })
    .await;
}

#[tokio::test]
async fn remove_reason_is_withdrawn_when_the_requester_removes_their_own_pending_request() {
    let Some(pool) = test_pool().await else { return };
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;

    let (low, high) = (low_uuid(), high_uuid());
    dir.insert(&low, "Lo#0001");
    dir.insert(&high, "Hi#0002");

    let ids = vec![low.clone(), high.clone()];
    let (p, lo) = (pool.clone(), low);
    with_cleanup(&pool, ids, async move {
        let f = svc.request(Identity::player(&lo), "Hi#0002".into()).await.unwrap();

        svc.remove(Identity::player(&lo), f.edge_id.clone()).await.unwrap();
        let reason: String = sqlx::query_scalar(
            "SELECT payload->>'reason' FROM asyncevents.events \
              WHERE topic = 'friend.removed' AND payload->>'edge_id' = $1",
        )
        .bind(&f.edge_id)
        .fetch_one(&p)
        .await
        .unwrap();
        assert_eq!(reason, REASON_WITHDRAWN);
    })
    .await;
}

#[tokio::test]
async fn remove_reason_is_declined_when_the_addressee_removes_a_still_pending_edge() {
    let Some(pool) = test_pool().await else { return };
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;

    let (low, high) = (low_uuid(), high_uuid());
    dir.insert(&low, "Lo#0001");
    dir.insert(&high, "Hi#0002");

    let ids = vec![low.clone(), high.clone()];
    let (p, lo, hi) = (pool.clone(), low, high);
    with_cleanup(&pool, ids, async move {
        let f = svc.request(Identity::player(&lo), "Hi#0002".into()).await.unwrap();

        // The ADDRESSEE removes (not decline) a still-pending edge: F4's fix, no reason
        // covered this before.
        svc.remove(Identity::player(&hi), f.edge_id.clone()).await.unwrap();
        let reason: String = sqlx::query_scalar(
            "SELECT payload->>'reason' FROM asyncevents.events \
              WHERE topic = 'friend.removed' AND payload->>'edge_id' = $1",
        )
        .bind(&f.edge_id)
        .fetch_one(&p)
        .await
        .unwrap();
        assert_eq!(reason, REASON_DECLINED);
    })
    .await;
}

// ============================================================================
// Item 11 — paging: a page boundary with equal `created_at` breaks the tie on id, and
// both UNION branches (caller as low_id / caller as high_id) contribute rows.
// ============================================================================

#[tokio::test]
async fn list_page_boundary_ties_break_on_id_and_both_union_branches_return() {
    let Some(pool) = test_pool().await else { return };
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;

    // FOUR tied rows, not two: with only two, an outer sort that dropped the `id`
    // tie-break key would still pass about half the time (Postgres' order for equal
    // keys is implementation-defined but not adversarial). Four rows in a fixed
    // fetch order make accidental agreement on the full DESC sequence ~1-in-24.
    let caller = fixed_uuid(0x50);
    let fr_a = fixed_uuid(0x10); // caller is the HIGH side of this relation.
    let fr_b = fixed_uuid(0x20); // caller is the HIGH side of this relation.
    let fr_c = fixed_uuid(0x90); // caller is the LOW side of this relation.
    let fr_d = fixed_uuid(0xa0); // caller is the LOW side of this relation.
    dir.insert(&caller, "Caller#0001");
    dir.insert(&fr_a, "FrA#0002");
    dir.insert(&fr_b, "FrB#0003");
    dir.insert(&fr_c, "FrC#0004");
    dir.insert(&fr_d, "FrD#0005");

    let ids = vec![caller.clone(), fr_a.clone(), fr_b.clone(), fr_c.clone(), fr_d.clone()];
    let (p, c, a, b, cc, d) = (pool.clone(), caller, fr_a, fr_b, fr_c, fr_d);
    with_cleanup(&pool, ids, async move {
        let f1 = svc.request(Identity::player(&a), "Caller#0001".into()).await.unwrap();
        svc.accept(Identity::player(&c), f1.edge_id.clone()).await.unwrap();
        let f2 = svc.request(Identity::player(&b), "Caller#0001".into()).await.unwrap();
        svc.accept(Identity::player(&c), f2.edge_id.clone()).await.unwrap();
        let f3 = svc.request(Identity::player(&c), "FrC#0004".into()).await.unwrap();
        svc.accept(Identity::player(&cc), f3.edge_id.clone()).await.unwrap();
        let f4 = svc.request(Identity::player(&c), "FrD#0005".into()).await.unwrap();
        svc.accept(Identity::player(&d), f4.edge_id.clone()).await.unwrap();

        let edge_ids = vec![f1.edge_id.clone(), f2.edge_id.clone(), f3.edge_id.clone(), f4.edge_id.clone()];
        // Force an exact tie on created_at: ordering can then ONLY be decided by id.
        sqlx::query("UPDATE friends.edges SET created_at = now() WHERE id = ANY($1::uuid[])")
            .bind(edge_ids.clone())
            .execute(&p)
            .await
            .unwrap();

        let mut expected = edge_ids.clone();
        expected.sort();
        expected.reverse();

        let mut cursor = String::new();
        let mut observed = Vec::new();
        for _ in 0..4 {
            let page = svc.list(Identity::player(&c), cursor.clone(), 1).await.unwrap();
            assert_eq!(page.items.len(), 1, "each page must carry exactly one of the 4 tied rows");
            observed.push(page.items[0].edge_id.clone());
            cursor = page.next_cursor;
        }
        assert_eq!(cursor, "", "the last page carries no cursor");
        assert_eq!(
            observed, expected,
            "a tie must break DESC on id across the WHOLE sequence, not just a coin-flip pair"
        );

        let is_low_a: bool = sqlx::query_scalar("SELECT low_id = $1::uuid FROM friends.edges WHERE id = $2::uuid")
            .bind(&c)
            .bind(&f1.edge_id)
            .fetch_one(&p)
            .await
            .unwrap();
        let is_low_c: bool = sqlx::query_scalar("SELECT low_id = $1::uuid FROM friends.edges WHERE id = $2::uuid")
            .bind(&c)
            .bind(&f3.edge_id)
            .fetch_one(&p)
            .await
            .unwrap();
        assert_ne!(is_low_a, is_low_c, "the caller must be low_id in one relation and high_id in the other");
    })
    .await;
}

// ============================================================================
// Item 12 — a directory MISS keeps the row with empty name/handle; a directory FAILURE
// is Unavailable with no page. Different outcomes, per the contract.
// ============================================================================

#[tokio::test]
async fn list_row_with_directory_miss_keeps_empty_name_and_handle() {
    let Some(pool) = test_pool().await else { return };
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;

    let (caller, friend) = (low_uuid(), high_uuid());
    dir.insert(&caller, "Caller#0001");
    dir.insert(&friend, "Friend#0002");

    let ids = vec![caller.clone(), friend.clone()];
    let (c, fr) = (caller, friend);
    with_cleanup(&pool, ids, async move {
        let f = svc.request(Identity::player(&c), "Friend#0002".into()).await.unwrap();
        svc.accept(Identity::player(&fr), f.edge_id.clone()).await.unwrap();

        // The friend disappears from the directory batch (a MISS, not a failure).
        dir.remove(&fr);

        let page = svc.list(Identity::player(&c), String::new(), 10).await.unwrap();
        let row = page
            .items
            .iter()
            .find(|i| i.edge_id == f.edge_id)
            .expect("the edge must still be listed despite the directory miss");
        assert_eq!(row.display_name, "");
        assert_eq!(row.handle, "");
        assert_eq!(row.player_id, fr, "the row keeps the raw id even without a directory hit");
    })
    .await;
}

#[tokio::test]
async fn list_directory_failure_is_unavailable_with_no_page() {
    let Some(pool) = test_pool().await else { return };
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;

    let (caller, friend) = (low_uuid(), high_uuid());
    dir.insert(&caller, "Caller#0001");
    dir.insert(&friend, "Friend#0002");

    let ids = vec![caller.clone(), friend.clone()];
    let (c, fr) = (caller, friend);
    with_cleanup(&pool, ids, async move {
        let f = svc.request(Identity::player(&c), "Friend#0002".into()).await.unwrap();
        svc.accept(Identity::player(&fr), f.edge_id.clone()).await.unwrap();

        dir.set_failing(true);
        let err = svc.list(Identity::player(&c), String::new(), 10).await.unwrap_err();
        assert_eq!(err.status, Status::Unavailable, "a directory outage must never serve a page of blanks");
    })
    .await;
}

// ============================================================================
// Item 14 — emit shares the store's transaction: force the append to fail and assert
// NEITHER the domain row NOR the event row survive.
// ============================================================================

#[tokio::test]
async fn request_rolls_back_the_row_and_the_event_when_the_append_fails() {
    let Some(pool) = test_pool().await else { return };
    let dir = Arc::new(FakeDirectory::new());
    let (low, high) = (low_uuid(), high_uuid());
    dir.insert(&low, "Lo#0001");
    dir.insert(&high, "Hi#0002");
    let svc = wired_with_failing_bus(&pool, dir.clone()).await;

    let ids = vec![low.clone(), high.clone()];
    let (p, lo, hi) = (pool.clone(), low, high);
    with_cleanup(&pool, ids, async move {
        let err = svc.request(Identity::player(&lo), "Hi#0002".into()).await.unwrap_err();
        assert_eq!(err.status, Status::Internal);

        let rows = row_count_for_pair(&p, &lo, &hi).await;
        assert_eq!(rows, 0, "the insert must roll back with the failed append");

        let events = asyncevents::testing::events_count(&p, "friend.requested", "requester_id", &lo)
            .await
            .unwrap();
        assert_eq!(events, 0, "no event may survive a rolled-back transaction");
    })
    .await;
}

// ============================================================================
// Item — the raced retry branch in `request` (removed pair between the conflicting
// insert and the re-read) needs TWO CONCURRENT connections to reach: within ONE
// transaction, `insert_pending_tx`'s `ON CONFLICT DO NOTHING` needs the pair PRESENT
// and the following `find_pair_tx` needs it ABSENT — production exposes no hook to
// interleave a second session's commit between those two statements, and pool.begin()
// (service.rs) issues a plain `BEGIN`, i.e. READ COMMITTED (as modules/wallet's store
// states outright), not an isolation level that would make this unreachable on its own.
// NOT exercised here — recorded as a known gap rather than faked with a single-session
// "delete then re-insert", which would prove only what the unique index already
// guarantees.
// ============================================================================

/// A raw insert reaches the same `friends.edges` row shape the service produces without
/// touching the directory at all.
async fn insert_edge_raw(pool: &PgPool, a: &str, b: &str, requester: &str, state: &str) -> String {
    let row: (String,) = sqlx::query_as(
        "INSERT INTO friends.edges (id, low_id, high_id, requester_id, state) \
         VALUES (gen_random_uuid(), least($1::uuid, $2::uuid), greatest($1::uuid, $2::uuid), \
                 $3::uuid, $4) \
         RETURNING id::text",
    )
    .bind(a)
    .bind(b)
    .bind(requester)
    .bind(state)
    .fetch_one(pool)
    .await
    .unwrap();
    row.0
}

// ============================================================================
// Account-deletion purge (Steps 5-7): the tombstone, the sweep (both sides of the
// pair, including a pending edge the player did not author), and the `request`
// write guard.
// ============================================================================

async fn purge_swept_at(pool: &PgPool, player_id: &str) -> Option<Option<String>> {
    sqlx::query_as::<_, (Option<String>,)>(
        "SELECT swept_at::text FROM friends.purged_players WHERE player_id = $1::uuid",
    )
    .bind(player_id)
    .fetch_optional(pool)
    .await
    .unwrap()
    .map(|(s,)| s)
}

async fn stamp_purge_swept(pool: &PgPool, player_id: &str) {
    sqlx::query("UPDATE friends.purged_players SET swept_at = now() WHERE player_id = $1::uuid")
        .bind(player_id)
        .execute(pool)
        .await
        .unwrap();
}

async fn cleanup_purge_row(pool: &PgPool, player_id: &str) {
    let _ = sqlx::query("DELETE FROM friends.purged_players WHERE player_id = $1::uuid")
        .bind(player_id)
        .execute(pool)
        .await;
}

async fn edge_exists(pool: &PgPool, edge_id: &str) -> bool {
    sqlx::query_scalar::<_, i32>("SELECT 1 FROM friends.edges WHERE id = $1::uuid")
        .bind(edge_id)
        .fetch_optional(pool)
        .await
        .unwrap()
        .is_some()
}

fn purge_payload(name: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({ "name": name })).unwrap()
}

/// A red run must not leave unswept tombstones, and a `Drop` guard cannot `await`. On a failure
/// or a hang the backends of `apps` are terminated first: a racer parked server-side keeps its
/// locks after its future is gone, and the cleanup DELETEs would wait on them.
async fn with_purge_cleanup<Fut>(
    pool: &PgPool,
    apps: Vec<String>,
    ids: Vec<String>,
    tombstones: Vec<String>,
    body: Fut,
)
where
    Fut: Future<Output = ()> + Send + 'static,
{
    let mut task = tokio::spawn(body);
    let outcome = tokio::time::timeout(Duration::from_secs(60), &mut task).await;
    if outcome.is_err() {
        task.abort();
        let _ = (&mut task).await;
    }
    if !matches!(outcome, Ok(Ok(()))) {
        for app in &apps {
            sqlx::query(
                "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE application_name = $1",
            )
            .bind(app)
            .execute(pool)
            .await
            .unwrap();
        }
    }
    cleanup_ids(pool, &ids).await;
    for pid in &tombstones {
        cleanup_purge_row(pool, pid).await;
    }
    match outcome {
        Ok(Ok(())) => {}
        Ok(Err(e)) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Ok(Err(e)) => panic!("friends purge test task ended without completing: {e}"),
        Err(_) => panic!("friends purge test body did not finish within 60s"),
    }
}

/// A fixed player id that sorts below every `fixed_uuid` id: one fire claims only the
/// `FRIENDS_PLAYERS_PER_FIRE` lowest unswept ids, so a sweep test's own players must be among
/// them. Each test owns a distinct `n` range.
fn fixed_player(n: u64) -> String {
    format!("00000000-0000-0000-0000-{n:012x}")
}

/// Drops whatever a crashed earlier run left under fixed ids: edges, events and tombstones.
async fn clear_fixed_players(pool: &PgPool, ids: &[String]) {
    cleanup_ids(pool, ids).await;
    for id in ids {
        cleanup_purge_row(pool, id).await;
    }
}

/// Precondition of a sweep test: one fire claims only the `FRIENDS_PLAYERS_PER_FIRE` lowest
/// unswept tombstones, so leftovers sorting below `player_id` would starve it and the test
/// would fail as a purge that never happened.
async fn assert_claimed_by_the_next_fire(pool: &PgPool, player_id: &str) {
    let below: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM friends.purged_players \
          WHERE swept_at IS NULL AND player_id < $1::uuid",
    )
    .bind(player_id)
    .fetch_one(pool)
    .await
    .unwrap();
    assert!(
        below < projection::FRIENDS_PLAYERS_PER_FIRE,
        "precondition: {below} unswept tombstones sort below {player_id}, so the next fire \
         cannot claim it"
    );
}

// Concurrency is staged, never slept: a racer runs on its OWN pool whose connections carry a
// unique `application_name`, so a test waits on `pg_stat_activity` until the racer is parked
// on the lock the test holds, then releases it.

fn racer_pool(app: &str) -> PgPool {
    let options = testdb::dsn()
        .parse::<sqlx::postgres::PgConnectOptions>()
        .expect("DATABASE_URL parses")
        .application_name(app);
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(60))
        .connect_lazy_with(options)
}

/// A service over `racer_pool(app)` sharing `svc`'s bus and directory, so its `request` runs
/// on a connection the test can find in `pg_stat_activity`.
fn racer_service(svc: &Service, app: &str) -> Arc<Service> {
    let racer = Service::new(racer_pool(app), svc.bus.clone());
    let directory = svc.directory.get().expect("wired sets the directory").clone();
    racer.directory.set(directory).ok().expect("directory set once");
    Arc::new(racer)
}

/// A unique `application_name` for one racer of one test run.
fn racer_app(what: &str) -> String {
    format!("friends-{what}-{}", unique_suffix())
}

/// Awaits a spawned racer with a hang guard, so a deadlocked race fails with a name instead
/// of stalling the run.
async fn finish<T>(what: &str, handle: tokio::task::JoinHandle<T>) -> T {
    match tokio::time::timeout(Duration::from_secs(30), handle).await {
        Ok(Ok(value)) => value,
        Ok(Err(e)) => panic!("{what} task did not complete: {e}"),
        Err(_) => panic!("{what} did not finish within 30s"),
    }
}

/// Polls observed database state until a backend of `app` waits on one of the advisory
/// `keys` (`true`), or `racer` finished without ever parking (`false`). The second answer is
/// how an unserialized racer shows itself; a racer parked on any OTHER lock reaches the
/// deadline, which is a hang guard, not a timing bound.
async fn wait_advisory_parked<T>(
    pool: &PgPool,
    app: &str,
    keys: &[i64],
    racer: &tokio::task::JoinHandle<T>,
) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let parked: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_locks l JOIN pg_stat_activity a ON a.pid = l.pid \
              WHERE a.application_name = $1 AND NOT l.granted AND l.locktype = 'advisory' \
                AND l.objsubid = 1 AND ((l.classid::bigint << 32) | l.objid::bigint) = ANY($2)",
        )
        .bind(app)
        .bind(keys)
        .fetch_one(pool)
        .await
        .unwrap();
        if parked > 0 {
            return true;
        }
        if racer.is_finished() {
            return false;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "racer {app} neither parked on the advisory locks {keys:?} nor finished"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Whether a backend of `app` holds the advisory lock `key`.
async fn holds_advisory(pool: &PgPool, app: &str, key: i64) -> bool {
    let held: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_locks l JOIN pg_stat_activity a ON a.pid = l.pid \
          WHERE a.application_name = $1 AND l.granted AND l.locktype = 'advisory' \
            AND l.objsubid = 1 AND ((l.classid::bigint << 32) | l.objid::bigint) = $2",
    )
    .bind(app)
    .bind(key)
    .fetch_one(pool)
    .await
    .unwrap();
    held > 0
}

async fn wait_xact_parked<T>(pool: &PgPool, app: &str, xid: &str, racer: &tokio::task::JoinHandle<T>) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let parked: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_locks l JOIN pg_stat_activity a ON a.pid = l.pid \
              WHERE a.application_name = $1 AND NOT l.granted AND l.locktype = 'transactionid' \
                AND l.transactionid::text = $2",
        )
        .bind(app)
        .bind(xid)
        .fetch_one(pool)
        .await
        .unwrap();
        if parked > 0 {
            return true;
        }
        if racer.is_finished() {
            return false;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "racer {app} neither parked on the transaction {xid} nor finished"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn gate_pair_edge(gate: &mut sqlx::PgConnection, a: &str, b: &str) -> String {
    sqlx::query(
        "INSERT INTO friends.edges (id, low_id, high_id, requester_id, state) \
         VALUES (gen_random_uuid(), least($1::uuid, $2::uuid), greatest($1::uuid, $2::uuid), \
                 $1::uuid, 'pending')",
    )
    .bind(a)
    .bind(b)
    .execute(&mut *gate)
    .await
    .unwrap();
    sqlx::query_scalar("SELECT (pg_current_xact_id()::text::bigint % 4294967296)::text")
        .fetch_one(&mut *gate)
        .await
        .unwrap()
}

/// The advisory keys a locking call actually takes, read back from `pg_locks` on the locking
/// connection (a bigint key is split into `classid` high and `objid` low halves,
/// `objsubid = 1`).
async fn held_keys(tx: &mut sqlx::PgConnection) -> Vec<i64> {
    sqlx::query_scalar(
        "SELECT (classid::bigint << 32) | objid::bigint FROM pg_locks \
          WHERE locktype = 'advisory' AND objsubid = 1 AND pid = pg_backend_pid() \
          ORDER BY 1",
    )
    .fetch_all(&mut *tx)
    .await
    .unwrap()
}

/// The advisory key `Store::lock_player_tx` takes for `spelling`.
async fn taken_player_key(pool: &PgPool, svc: &Service, spelling: &str) -> i64 {
    let mut tx = pool.begin().await.unwrap();
    svc.store.lock_player_tx(&mut tx, spelling).await.unwrap();
    let keys = held_keys(&mut tx).await;
    tx.rollback().await.unwrap();
    assert_eq!(keys.len(), 1, "lock_player_tx must hold exactly one advisory lock, got {keys:?}");
    keys[0]
}

/// The tombstone insert is idempotent, and a redelivery after the sweep stamped
/// `swept_at` must NOT reset it.
#[tokio::test]
async fn plant_purged_player_is_idempotent_and_never_resets_swept_at() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let directory = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, directory).await;
    let pid = low_uuid();

    with_cleanup(&pool, vec![pid.clone()], {
        let pool = pool.clone();
        let svc = svc.clone();
        let pid = pid.clone();
        async move {
            let mut conn = pool.acquire().await.unwrap();
            svc.store.plant_purged_player_tx(&mut conn, &pid, "Handle#0001").await.unwrap();
            svc.store.plant_purged_player_tx(&mut conn, &pid, "Handle#0001").await.unwrap();

            let (n,): (i64,) =
                sqlx::query_as("SELECT count(*) FROM friends.purged_players WHERE player_id = $1::uuid")
                    .bind(&pid)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(n, 1, "a redelivered tombstone must not insert a second row");

            stamp_purge_swept(&pool, &pid).await;
            svc.store.plant_purged_player_tx(&mut conn, &pid, "Handle#0001").await.unwrap();
            assert!(
                purge_swept_at(&pool, &pid).await.flatten().is_some(),
                "a redelivery after the sweep stamped swept_at must NOT reset it"
            );
        }
    })
    .await;
    cleanup_purge_row(&pool, &pid).await;
}

/// THE PREVIOUSLY-RISKY SHAPE: the sweep deletes on EITHER side of the canonical
/// pair (`low_id`/`high_id`) and in EITHER state — including a PENDING edge the
/// purged player did not author (a query filtered by `requester_id` would miss it).
/// A survivor pair is untouched, and the removal carries `REASON_ACCOUNT_DELETED`.
#[tokio::test]
async fn sweep_removes_edges_on_both_sides_including_a_pending_edge_not_authored_by_the_player() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let directory = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, directory).await;

    let target = low_uuid(); // the purged player
    let as_low_side = high_uuid(); // an ACCEPTED edge where target is the LOW id
    let pending_addressee = fixed_uuid(0x33); // a PENDING edge the TARGET did not author
    let survivor_a = fixed_uuid(0x44);
    let survivor_b = fixed_uuid(0x55);

    let ids = vec![
        target.clone(),
        as_low_side.clone(),
        pending_addressee.clone(),
        survivor_a.clone(),
        survivor_b.clone(),
    ];

    with_purge_cleanup(&pool, vec![], ids, vec![target.clone()], {
        let pool = pool.clone();
        let svc = svc.clone();
        let target = target.clone();
        let as_low_side = as_low_side.clone();
        let pending_addressee = pending_addressee.clone();
        let survivor_a = survivor_a.clone();
        let survivor_b = survivor_b.clone();
        async move {
            // target as low_id, accepted, requested BY target.
            let e1 = insert_edge_raw(&pool, &target, &as_low_side, &target, STATE_ACCEPTED).await;
            // target as high_id (pending_addressee < target numerically via fixed_uuid tiers),
            // PENDING, requested by the OTHER party — target never authored this request.
            let e2 =
                insert_edge_raw(&pool, &pending_addressee, &target, &pending_addressee, STATE_PENDING)
                    .await;
            // A survivor pair sharing NEITHER id with target.
            let e3 = insert_edge_raw(&pool, &survivor_a, &survivor_b, &survivor_a, STATE_ACCEPTED).await;

            let mut conn = pool.acquire().await.unwrap();
            svc.store.plant_purged_player_tx(&mut conn, &target, "Target#0001").await.unwrap();
            drop(conn);
            assert_claimed_by_the_next_fire(&pool, &target).await;

            let mut tx = pool.begin().await.unwrap();
            svc.sweep_purged_players(&mut tx).await.unwrap();
            tx.commit().await.unwrap();

            assert!(!edge_exists(&pool, &e1).await, "the accepted edge (target as low_id) must be removed");
            assert!(
                !edge_exists(&pool, &e2).await,
                "the PENDING edge the target did not author must also be removed"
            );
            assert!(edge_exists(&pool, &e3).await, "a survivor pair must be untouched");

            let removed = asyncevents::testing::events_count(&pool, "friend.removed", "actor_id", &target)
                .await
                .unwrap();
            assert_eq!(removed, 2, "one friend.removed per deleted edge");

            let reason_count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM asyncevents.events WHERE topic = 'friend.removed' \
                  AND payload->>'actor_id' = $1 AND payload->>'reason' = $2",
            )
            .bind(&target)
            .bind(friendsevents::REASON_ACCOUNT_DELETED)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(reason_count, 2, "both removals must carry REASON_ACCOUNT_DELETED");

            assert!(purge_swept_at(&pool, &target).await.flatten().is_some());

            // Redelivery: swept_at excludes the target now — nothing more emitted.
            let mut tx2 = pool.begin().await.unwrap();
            svc.sweep_purged_players(&mut tx2).await.unwrap();
            tx2.commit().await.unwrap();
            let removed_again =
                asyncevents::testing::events_count(&pool, "friend.removed", "actor_id", &target)
                    .await
                    .unwrap();
            assert_eq!(removed_again, 2, "a redelivery must emit NOTHING more");
        }
    })
    .await;
}

/// `request` is blocked by the tombstone PREDICATE, not "a tombstone exists
/// somewhere": a purged caller gets Conflict and no edge row, while a different,
/// non-tombstoned caller still succeeds.
#[tokio::test]
async fn request_is_blocked_by_tombstone_predicate_not_presence() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let directory = Arc::new(FakeDirectory::new());
    let purged = low_uuid();
    let other = high_uuid();
    let target = fixed_uuid(0x66);
    directory.insert(&purged, "Purged#0001");
    directory.insert(&other, "Other#0002");
    directory.insert(&target, "Target#0003");

    let (_ctx, svc) = wired(&pool, directory).await;
    let ids = vec![purged.clone(), other.clone(), target.clone()];

    with_cleanup(&pool, ids, {
        let pool = pool.clone();
        let svc = svc.clone();
        let purged = purged.clone();
        let other = other.clone();
        async move {
            let mut conn = pool.acquire().await.unwrap();
            svc.store.plant_purged_player_tx(&mut conn, &purged, "Purged#0001").await.unwrap();
            drop(conn);

            let e = svc
                .request(Identity::player(&purged), "Target#0003".into())
                .await
                .unwrap_err();
            assert_eq!(e.status, Status::Conflict);
            let (n,): (i64,) = sqlx::query_as(
                "SELECT count(*) FROM friends.edges WHERE low_id = $1::uuid OR high_id = $1::uuid",
            )
            .bind(&purged)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(n, 0, "no edge row for a tombstoned requester");

            svc.request(Identity::player(&other), "Target#0003".into())
                .await
                .expect("a different, non-tombstoned player must still be able to request");
        }
    })
    .await;
    cleanup_purge_row(&pool, &purged).await;
}

/// A foreign `scheduler.fired` name sweeps nothing; the real name (positive control on
/// the same row) drains it.
#[tokio::test]
async fn sweep_handler_ignores_a_foreign_schedule_name_with_positive_control() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let directory = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, directory).await;
    let target = low_uuid();
    let other = high_uuid();

    with_purge_cleanup(&pool, vec![], vec![target.clone(), other.clone()], vec![target.clone()], {
        let pool = pool.clone();
        let svc = svc.clone();
        let target = target.clone();
        let other = other.clone();
        async move {
            let edge = insert_edge_raw(&pool, &target, &other, &target, STATE_ACCEPTED).await;
            let mut conn = pool.acquire().await.unwrap();
            svc.store.plant_purged_player_tx(&mut conn, &target, "Target#0001").await.unwrap();
            drop(conn);
            assert_claimed_by_the_next_fire(&pool, &target).await;

            let handler = projection::PurgeSweepHandler { svc: svc.clone() };

            let mut tx = pool.begin().await.unwrap();
            let delivery = Delivery { event_id: "friends:test", tx: AnyTx::new(&mut *tx) };
            handler.call(delivery, purge_payload("some-other-job")).await.unwrap();
            tx.commit().await.unwrap();
            assert!(edge_exists(&pool, &edge).await, "a foreign schedule name must not sweep");

            let mut tx2 = pool.begin().await.unwrap();
            let delivery2 = Delivery { event_id: "friends:test", tx: AnyTx::new(&mut *tx2) };
            handler
                .call(delivery2, purge_payload(projection::PURGE_SCHEDULE_NAME))
                .await
                .unwrap();
            tx2.commit().await.unwrap();
            assert!(!edge_exists(&pool, &edge).await, "the real schedule name must sweep the same row");
        }
    })
    .await;
}

/// `pending_purges()` counts ONLY `swept_at IS NULL` — a swept row must not inflate
/// it. Proven by DELTA (the shared table may carry unrelated rows from concurrent
/// tests): planting one unswept and one swept tombstone must move the count by
/// exactly ONE, never two.
#[tokio::test]
async fn pending_purges_counts_only_swept_at_null_a_swept_row_does_not_inflate_it() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let directory = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, directory).await;
    let unswept = fixed_uuid(0x88);
    let already_swept = fixed_uuid(0x99);

    let (before, _) = svc.store.pending_purges().await.unwrap();

    let mut conn = pool.acquire().await.unwrap();
    svc.store.plant_purged_player_tx(&mut conn, &unswept, "Unswept#0001").await.unwrap();
    svc.store.plant_purged_player_tx(&mut conn, &already_swept, "Swept#0002").await.unwrap();
    drop(conn);
    sqlx::query("UPDATE friends.purged_players SET swept_at = now() WHERE player_id = $1::uuid")
        .bind(&already_swept)
        .execute(&pool)
        .await
        .unwrap();

    let (after, _) = svc.store.pending_purges().await.unwrap();
    assert_eq!(
        after - before,
        1,
        "only the UNSWEPT tombstone may count — the already-swept one must not inflate it"
    );

    let _ = sqlx::query("DELETE FROM friends.purged_players WHERE player_id = ANY($1::uuid[])")
        .bind([unswept.as_str(), already_swept.as_str()].as_slice())
        .execute(&pool)
        .await;
}

// ============================================================================
// `request` and the purge sweep serialize on per-player advisory locks: a request locks both
// parties in ascending order, the sweep locks each claimed player in the same order.
// ============================================================================

/// The key is read back from the lock `lock_player_tx` really takes: the
/// same for every spelling `$1::uuid` treats as one player, and different for a different uuid.
#[tokio::test(flavor = "multi_thread")]
async fn player_lock_key_is_one_per_uuid_across_spellings() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool, Arc::new(FakeDirectory::new())).await;
    let canon = "a2b7e8c1-0000-4000-8000-0000000000f1";
    let hex = canon.replace('-', "");

    let key = taken_player_key(&pool, &svc, canon).await;
    assert_eq!(key, taken_player_key(&pool, &svc, &canon.to_uppercase()).await, "case must not change the key");
    assert_eq!(
        key,
        taken_player_key(&pool, &svc, &format!("{{{canon}}}")).await,
        "braces must not change the key"
    );
    assert_eq!(key, taken_player_key(&pool, &svc, &hex).await, "hyphenation must not change the key");
    assert_ne!(
        key,
        taken_player_key(&pool, &svc, "a2b7e8c1-0000-4000-8000-0000000000f2").await,
        "a genuinely different uuid must take a different key"
    );
}

/// `lock_pair_tx` holds both players' keys, and one key when both spellings name one player.
#[tokio::test(flavor = "multi_thread")]
async fn lock_pair_tx_holds_both_keys_and_one_for_a_single_player() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool, Arc::new(FakeDirectory::new())).await;
    let (a, b) = (fixed_player(0x600), fixed_player(0x601));
    let (ka, kb) = (taken_player_key(&pool, &svc, &a).await, taken_player_key(&pool, &svc, &b).await);

    let mut tx = pool.begin().await.unwrap();
    svc.store.lock_pair_tx(&mut tx, &b, &a).await.unwrap();
    let mut both = held_keys(&mut tx).await;
    tx.rollback().await.unwrap();
    both.sort_unstable();
    let mut want = vec![ka, kb];
    want.sort_unstable();
    assert_eq!(both, want, "a pair must hold exactly both players' keys");

    let mut tx = pool.begin().await.unwrap();
    svc.store.lock_pair_tx(&mut tx, &a, &a.to_uppercase()).await.unwrap();
    let one = held_keys(&mut tx).await;
    tx.rollback().await.unwrap();
    assert_eq!(one, vec![ka], "one player spelled twice must take its key once");
}

/// Race 1, request first — the once-wrong branch. A real `request` has passed both tombstone
/// guards and is held at its edge INSERT, holding both pair locks, behind a gate transaction's
/// uncommitted edge for the same pair, when one party's tombstone commits and a sweep fires.
/// The sweep's DELETE cannot see either uncommitted edge, so without the per-player lock
/// it stamped `swept_at` around it and the edge outlived the erasure. Now the sweep parks on
/// the purged party's key, which the request holds, until `request` commits, then deletes the
/// edge with one `friend.removed`.
async fn sweep_waits_for_an_in_flight_request_case(tombstone_requester: bool, base: u64) {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let directory = Arc::new(FakeDirectory::new());
    let (me, target) = (fixed_player(base), fixed_player(base + 1));
    directory.insert(&me, &format!("Me#{base:04x}"));
    directory.insert(&target, &format!("Target#{base:04x}"));
    let (_ctx, svc) = wired(&pool, directory).await;
    let pair = vec![me.clone(), target.clone()];
    clear_fixed_players(&pool, &pair).await;
    let purged = if tombstone_requester { me.clone() } else { target.clone() };
    let lock_key = taken_player_key(&pool, &svc, &purged).await;
    let gate_app = racer_app("race1-gate");
    let request_app = racer_app("race1-request");
    let sweep_app = racer_app("race1-sweep");

    let apps = vec![gate_app.clone(), request_app.clone(), sweep_app.clone()];
    with_purge_cleanup(&pool, apps, pair.clone(), pair, {
        let pool = pool.clone();
        let svc = svc.clone();
        async move {
            let gate_pool = racer_pool(&gate_app);
            let mut gate = gate_pool.begin().await.unwrap();
            let gate_xid = gate_pair_edge(&mut gate, &me, &target).await;

            let request = tokio::spawn({
                let racer = racer_service(&svc, &request_app);
                let (me, handle) = (me.clone(), format!("Target#{base:04x}"));
                async move { racer.request(Identity::player(&me), handle).await }
            });
            assert!(
                wait_xact_parked(&pool, &request_app, &gate_xid, &request).await,
                "request must be held at its edge INSERT, past its guards and pair locks"
            );

            let mut conn = pool.acquire().await.unwrap();
            svc.store.plant_purged_player_tx(&mut conn, &purged, "Race#0001").await.unwrap();
            drop(conn);
            assert_claimed_by_the_next_fire(&pool, &purged).await;

            let sweep = tokio::spawn({
                let handler = projection::PurgeSweepHandler { svc: svc.clone() };
                let racer = racer_pool(&sweep_app);
                async move {
                    let mut tx = racer.begin().await.unwrap();
                    let delivery = Delivery { event_id: "friends:race1", tx: AnyTx::new(&mut *tx) };
                    handler
                        .call(delivery, purge_payload(projection::PURGE_SCHEDULE_NAME))
                        .await
                        .unwrap();
                    tx.commit().await.unwrap();
                }
            });
            let parked = wait_advisory_parked(&pool, &sweep_app, &[lock_key], &sweep).await;

            gate.rollback().await.unwrap();
            let f = finish("race-1 request", request)
                .await
                .expect("the request passed its guards before the tombstone");
            finish("race-1 sweep", sweep).await;

            assert!(!edge_exists(&pool, &f.edge_id).await, "the in-flight request's edge survived the erasure");
            let removed = asyncevents::testing::events_count(&pool, "friend.removed", "actor_id", &purged)
                .await
                .unwrap();
            assert_eq!(removed, 1, "the sweep must remove the in-flight edge with one friend.removed");
            assert!(purge_swept_at(&pool, &purged).await.flatten().is_some(), "the fire must stamp swept_at");
            assert!(parked, "the sweep must have parked on the purged player's advisory lock");
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sweep_waits_for_an_in_flight_request_from_a_purged_requester() {
    sweep_waits_for_an_in_flight_request_case(true, 0x200).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sweep_waits_for_an_in_flight_request_to_a_purged_target() {
    sweep_waits_for_an_in_flight_request_case(false, 0x210).await;
}

/// Race 2, sweep first. An open sweep transaction has locked and swept one party; a racer
/// `request` parks on that party's key, then, once the sweep commits, answers 409
/// `PLAYER_DELETED` for a purged requester or exactly the directory-miss answer for a purged
/// target, and writes no edge. For the requester the tombstone is already committed, so only
/// the "request parks" assertion needs the lock; for the target the answer needs the target
/// guard, because the directory still resolved the handle.
async fn request_parks_behind_an_open_sweep_case(tombstone_requester: bool, base: u64) {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let directory = Arc::new(FakeDirectory::new());
    let (me, target) = (fixed_player(base), fixed_player(base + 1));
    directory.insert(&me, &format!("Me#{base:04x}"));
    directory.insert(&target, &format!("Target#{base:04x}"));
    let (_ctx, svc) = wired(&pool, directory).await;
    let pair = vec![me.clone(), target.clone()];
    clear_fixed_players(&pool, &pair).await;
    let purged = if tombstone_requester { me.clone() } else { target.clone() };
    let lock_key = taken_player_key(&pool, &svc, &purged).await;
    let sweep_app = racer_app("race2-sweep");
    let request_app = racer_app("race2-request");

    let apps = vec![sweep_app.clone(), request_app.clone()];
    with_purge_cleanup(&pool, apps, pair.clone(), pair, {
        let pool = pool.clone();
        let svc = svc.clone();
        async move {
            let miss = svc
                .request(Identity::player(&me), "Nobody#0000".into())
                .await
                .unwrap_err();

            let mut conn = pool.acquire().await.unwrap();
            svc.store.plant_purged_player_tx(&mut conn, &purged, "Race#0002").await.unwrap();
            drop(conn);
            assert_claimed_by_the_next_fire(&pool, &purged).await;

            let sweep_pool = racer_pool(&sweep_app);
            let mut sweep_tx = sweep_pool.begin().await.unwrap();
            svc.sweep_purged_players(&mut sweep_tx).await.unwrap();
            let stamped: bool = sqlx::query_scalar(
                "SELECT swept_at IS NOT NULL FROM friends.purged_players WHERE player_id = $1::uuid",
            )
            .bind(&purged)
            .fetch_one(&mut *sweep_tx)
            .await
            .unwrap();
            assert!(stamped, "the open sweep must have locked and swept this player");

            let request = tokio::spawn({
                let racer = racer_service(&svc, &request_app);
                let (me, handle) = (me.clone(), format!("Target#{base:04x}"));
                async move { racer.request(Identity::player(&me), handle).await }
            });
            let parked = wait_advisory_parked(&pool, &request_app, &[lock_key], &request).await;
            assert!(parked, "the request must park on the purged player's lock while the sweep is open");

            sweep_tx.commit().await.unwrap();
            let e = finish("race-2 request", request).await.unwrap_err();
            if tombstone_requester {
                assert_eq!(e.status, Status::Conflict);
                assert_eq!(e.msg, crate::service::PLAYER_DELETED);
            } else {
                assert_eq!(
                    (e.status, e.msg.as_str()),
                    (miss.status, miss.msg.as_str()),
                    "a purged target must get exactly the directory-miss answer"
                );
            }
            assert_eq!(row_count_for_pair(&pool, &me, &target).await, 0, "the refused request must write no edge");
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn request_parks_behind_an_open_sweep_then_answers_player_deleted() {
    request_parks_behind_an_open_sweep_case(true, 0x300).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn request_parks_behind_an_open_sweep_then_answers_no_such_player() {
    request_parks_behind_an_open_sweep_case(false, 0x310).await;
}

async fn request_takes_the_pair_lock_before_it_reads_the_tombstone_case(tombstone_requester: bool, base: u64) {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let directory = Arc::new(FakeDirectory::new());
    let (me, target) = (fixed_player(base), fixed_player(base + 1));
    directory.insert(&me, &format!("Me#{base:04x}"));
    directory.insert(&target, &format!("Target#{base:04x}"));
    let (_ctx, svc) = wired(&pool, directory).await;
    let pair = vec![me.clone(), target.clone()];
    clear_fixed_players(&pool, &pair).await;
    let purged = if tombstone_requester { me.clone() } else { target.clone() };
    let lock_key = taken_player_key(&pool, &svc, &purged).await;
    let gate_app = racer_app("guard-gate");
    let request_app = racer_app("guard-request");

    let apps = vec![gate_app.clone(), request_app.clone()];
    with_purge_cleanup(&pool, apps, pair.clone(), pair, {
        let pool = pool.clone();
        let svc = svc.clone();
        async move {
            let miss = svc
                .request(Identity::player(&me), "Nobody#0000".into())
                .await
                .unwrap_err();
            assert_eq!(purge_swept_at(&pool, &purged).await, None, "precondition: no tombstone yet");

            let gate_pool = racer_pool(&gate_app);
            let mut gate = gate_pool.begin().await.unwrap();
            svc.store.lock_player_tx(&mut gate, &purged).await.unwrap();

            let request = tokio::spawn({
                let racer = racer_service(&svc, &request_app);
                let (me, handle) = (me.clone(), format!("Target#{base:04x}"));
                async move { racer.request(Identity::player(&me), handle).await }
            });
            let parked = wait_advisory_parked(&pool, &request_app, &[lock_key], &request).await;
            assert!(parked, "the request must park on the player's lock before its tombstone read");

            let mut conn = pool.acquire().await.unwrap();
            svc.store.plant_purged_player_tx(&mut conn, &purged, "Guard#0001").await.unwrap();
            drop(conn);
            gate.rollback().await.unwrap();

            let e = finish("guard-order request", request).await.unwrap_err();
            if tombstone_requester {
                assert_eq!(e.status, Status::Conflict);
                assert_eq!(e.msg, crate::service::PLAYER_DELETED);
            } else {
                assert_eq!(
                    (e.status, e.msg.as_str()),
                    (miss.status, miss.msg.as_str()),
                    "a purged target must get exactly the directory-miss answer"
                );
            }
            assert_eq!(
                row_count_for_pair(&pool, &me, &target).await,
                0,
                "a request that read the tombstone before the lock would insert after the release"
            );
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn request_takes_the_requester_lock_before_it_reads_the_requester_tombstone() {
    request_takes_the_pair_lock_before_it_reads_the_tombstone_case(true, 0x700).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn request_takes_the_target_lock_before_it_reads_the_target_tombstone() {
    request_takes_the_pair_lock_before_it_reads_the_tombstone_case(false, 0x710).await;
}

/// The pair order is the sweep's claim order, so a request between two purged players and a
/// sweep claiming both never deadlock. The test holds B's key; the request A→B takes A and
/// parks on B; the sweep claims A and B and parks on A. Once B is released the request takes
/// it, answers `PLAYER_DELETED` and releases both, and the sweep purges both. Locking the
/// pair B-first instead leaves the request parked on B holding nothing while the sweep takes
/// A and parks on B; on release the request takes B and waits on A, and Postgres aborts one
/// of the two with 40P01.
#[tokio::test(flavor = "multi_thread")]
async fn request_between_two_purged_players_and_their_sweep_do_not_deadlock() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let directory = Arc::new(FakeDirectory::new());
    let (a, b, c) = (fixed_player(0x400), fixed_player(0x401), fixed_player(0x402));
    directory.insert(&a, "OrderA#0001");
    directory.insert(&b, "OrderB#0002");
    let (_ctx, svc) = wired(&pool, directory).await;
    let ids = vec![a.clone(), b.clone(), c.clone()];
    clear_fixed_players(&pool, &ids).await;
    let (key_a, key_b) = (taken_player_key(&pool, &svc, &a).await, taken_player_key(&pool, &svc, &b).await);
    let gate_app = racer_app("order-gate");
    let request_app = racer_app("order-request");
    let sweep_app = racer_app("order-sweep");

    let apps = vec![gate_app.clone(), request_app.clone(), sweep_app.clone()];
    with_purge_cleanup(&pool, apps, ids.clone(), vec![a.clone(), b.clone()], {
        let pool = pool.clone();
        let svc = svc.clone();
        async move {
            let ab = insert_edge_raw(&pool, &a, &b, &a, STATE_ACCEPTED).await;
            let bc = insert_edge_raw(&pool, &b, &c, &c, STATE_PENDING).await;
            let mut conn = pool.acquire().await.unwrap();
            svc.store.plant_purged_player_tx(&mut conn, &a, "OrderA#0001").await.unwrap();
            svc.store.plant_purged_player_tx(&mut conn, &b, "OrderB#0002").await.unwrap();
            drop(conn);
            assert_claimed_by_the_next_fire(&pool, &b).await;

            let gate_pool = racer_pool(&gate_app);
            let mut gate = gate_pool.begin().await.unwrap();
            sqlx::query("SELECT pg_advisory_xact_lock($1)")
                .bind(key_b)
                .execute(&mut *gate)
                .await
                .unwrap();

            let request = tokio::spawn({
                let racer = racer_service(&svc, &request_app);
                let a = a.clone();
                async move { racer.request(Identity::player(&a), "OrderB#0002".into()).await }
            });
            assert!(
                wait_advisory_parked(&pool, &request_app, &[key_b], &request).await,
                "the request must park on B's key"
            );
            let request_held_a = holds_advisory(&pool, &request_app, key_a).await;

            let sweep = tokio::spawn({
                let handler = projection::PurgeSweepHandler { svc: svc.clone() };
                let racer = racer_pool(&sweep_app);
                async move {
                    let mut tx = racer.begin().await.map_err(|e| e.to_string())?;
                    let delivery = Delivery { event_id: "friends:order", tx: AnyTx::new(&mut *tx) };
                    handler
                        .call(delivery, purge_payload(projection::PURGE_SCHEDULE_NAME))
                        .await
                        .map_err(|e| e.to_string())?;
                    tx.commit().await.map_err(|e| e.to_string())
                }
            });
            assert!(
                wait_advisory_parked(&pool, &sweep_app, &[key_a, key_b], &sweep).await,
                "the sweep must park on one of the two players' keys"
            );

            gate.commit().await.unwrap();
            let answer = finish("order request", request).await;
            let swept = finish("order sweep", sweep).await;
            assert!(swept.is_ok(), "the sweep must not fail (40P01 is a deadlock): {swept:?}");
            let e = answer.expect_err("both parties are purged, so the request is refused");
            assert_eq!(
                (e.status, e.msg.as_str()),
                (Status::Conflict, crate::service::PLAYER_DELETED),
                "the request must answer PLAYER_DELETED, not a deadlock: {e:?}"
            );

            assert!(purge_swept_at(&pool, &a).await.flatten().is_some(), "A must be swept");
            assert!(purge_swept_at(&pool, &b).await.flatten().is_some(), "B must be swept");
            assert!(!edge_exists(&pool, &ab).await, "the A-B edge must be purged");
            assert!(!edge_exists(&pool, &bc).await, "B's edge with a live player must be purged");
            assert!(request_held_a, "the request must have parked on B while holding A");
        }
    })
    .await;
}

/// One fire claims and locks at most 64 players. With 65 of this test's own lowest-sorting
/// tombstones, one fire stamps exactly the 64 lowest and leaves the 65th for the next fire.
/// The counts are literal, not derived from `FRIENDS_PLAYERS_PER_FIRE`, so raising the claim
/// limit or restoring a batch loop turns this red. The sweep's 5s budget is far above 64
/// players' few statements each, so it does not stop this fire early.
#[tokio::test(flavor = "multi_thread")]
async fn one_sweep_fire_stamps_at_most_64_players() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (_ctx, svc) = wired(&pool, Arc::new(FakeDirectory::new())).await;
    // The LOWEST fixed range, below the race tests' ids: a sibling that failed mid-race and
    // left its tombstone unswept must not sort among these.
    let ids: Vec<String> = (0x01..0x01 + 65).map(fixed_player).collect();
    for id in &ids {
        cleanup_purge_row(&pool, id).await;
    }
    let foreign_below: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM friends.purged_players \
          WHERE swept_at IS NULL AND player_id <= $1::uuid",
    )
    .bind(ids.last().unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(foreign_below, 0, "precondition: a foreign unswept tombstone sorts among this test's ids");

    let app = racer_app("fire-bound");
    with_purge_cleanup(&pool, vec![app.clone()], vec![], ids.clone(), {
        let (pool, svc, ids) = (racer_pool(&app), svc.clone(), ids.clone());
        async move {
            let mut conn = pool.acquire().await.unwrap();
            for (i, id) in ids.iter().enumerate() {
                svc.store
                    .plant_purged_player_tx(&mut conn, id, &format!("Cap#{i:04}"))
                    .await
                    .unwrap();
            }
            drop(conn);

            let mut tx = pool.begin().await.unwrap();
            svc.sweep_purged_players(&mut tx).await.unwrap();
            tx.commit().await.unwrap();
            assert_eq!(
                unstamped_tombstones(&pool, &ids).await,
                vec![ids[64].clone()],
                "one fire must stamp exactly the 64 lowest of 65 tombstones"
            );

            let mut tx2 = pool.begin().await.unwrap();
            svc.sweep_purged_players(&mut tx2).await.unwrap();
            tx2.commit().await.unwrap();
            assert!(
                unstamped_tombstones(&pool, &ids).await.is_empty(),
                "the next fire must resume and stamp the last one"
            );
        }
    })
    .await;
}

async fn unstamped_tombstones(pool: &PgPool, ids: &[String]) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT player_id::text FROM friends.purged_players \
          WHERE player_id = ANY($1::uuid[]) AND swept_at IS NULL ORDER BY player_id",
    )
    .bind(ids)
    .fetch_all(pool)
    .await
    .unwrap()
}
