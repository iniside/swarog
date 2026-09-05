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

async fn test_pool() -> Option<PgPool> {
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let pool = match tokio::time::timeout(Duration::from_secs(3), PgPool::connect(&dsn)).await {
        Ok(Ok(p)) => p,
        _ => {
            eprintln!("SKIP: postgres unreachable at {dsn} — friends DB tests skipped");
            return None;
        }
    };
    Some(pool)
}

/// Migrates BOTH the asyncevents plane and the friends schema EXACTLY ONCE per test
/// binary — concurrent idempotent DDL across parallel tests can deadlock on catalog locks.
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
            let module = Friends::new();
            module.register(&ctx).unwrap();
            module.migrate(&ctx).await.unwrap();
        })
        .await;
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
        Ok(ids.iter().filter_map(|id| players.get(id).cloned()).collect())
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
