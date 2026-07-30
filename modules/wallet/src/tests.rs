use super::*;
use crate::projection::{REASON, STARTER_AMOUNT, STARTER_CURRENCY};
use bus::AnyTx;
use opsapi::Status;
use sqlx::PgPool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use walletapi::{
    Movement, MAX_CURRENCY_CODE_BYTES, MAX_IDEMPOTENCY_KEY_BYTES, MAX_MOVEMENT_AMOUNT,
    MAX_REASON_BYTES,
};

/// Fallback DSN for the live tests (which otherwise read `DATABASE_URL`).
const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

// ---- Live Postgres integration (the local DB is the test DB) ----------

/// Opens the local Postgres; returns `None` (printing a skip line) when
/// unreachable, so the suite RUNS but SKIPs cleanly with no DB.
async fn test_pool() -> Option<PgPool> {
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let pool = match tokio::time::timeout(Duration::from_secs(3), PgPool::connect(&dsn)).await {
        Ok(Ok(p)) => p,
        _ => {
            eprintln!("SKIP: postgres unreachable at {dsn} — wallet DB tests skipped");
            return None;
        }
    };
    Some(pool)
}

/// Migrates BOTH the asyncevents (durable plane's event log) and wallet schemas
/// EXACTLY ONCE per test binary — concurrent idempotent DDL across parallel tests
/// can deadlock on catalog locks, so it is serialized to a single run.
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
            let w = WalletModule::new();
            w.register(&ctx).unwrap();
            w.migrate(&ctx).await.unwrap();
        })
        .await;
}

/// Builds a real durable plane over the live pool (schema migrated once via
/// `ensure_schema`), then registers `WalletModule` against a `Context` carrying that
/// plane's transport — so `apply_on`'s `emit_tx` is a REAL durable append, not a
/// no-op. Returns the ctx (kept alive; owns the bus) and the concrete `Service`
/// fetched through the module's own `register()`, never hand-assembled from private
/// fields.
async fn wired(pool: &PgPool) -> (Context, Arc<Service>) {
    ensure_schema(pool).await;
    let transport = asyncevents::testing::transport(pool.clone());
    let ctx = Context::with_db_and_transport(pool.clone(), transport.handle());
    let w = WalletModule::new();
    w.register(&ctx).unwrap();
    let svc = w.svc();
    (ctx, svc)
}

/// A fresh random player_id (a valid uuid) so parallel test runs never collide.
async fn unique_player(pool: &PgPool) -> String {
    let (id,): (String,) = sqlx::query_as("SELECT gen_random_uuid()::text")
        .fetch_one(pool)
        .await
        .unwrap();
    id
}

/// A fresh currency row (code well under the 32-byte cap) — most movement tests
/// need one to satisfy `balances_currency_fkey`.
async fn unique_currency(pool: &PgPool) -> String {
    let (suffix,): (String,) = sqlx::query_as("SELECT substr(gen_random_uuid()::text, 1, 12)")
        .fetch_one(pool)
        .await
        .unwrap();
    let code = format!("t{suffix}");
    sqlx::query(
        "INSERT INTO wallet.currencies (code, display_name, kind, decimals) \
         VALUES ($1, $1, 'soft', 0)",
    )
    .bind(&code)
    .execute(pool)
    .await
    .unwrap();
    code
}

/// A process-unique idempotency key — no DB round trip needed, only uniqueness
/// within this test binary (parallel `#[tokio::test]`s run in the same process).
static KEY_COUNTER: AtomicU64 = AtomicU64::new(0);
fn unique_key(prefix: &str) -> String {
    let n = KEY_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{n}-{}", std::process::id())
}

async fn cleanup(pool: &PgPool, players: &[&str], currencies: &[&str]) {
    for pid in players {
        let _ = sqlx::query("DELETE FROM wallet.ledger WHERE player_id = $1::uuid")
            .bind(pid)
            .execute(pool)
            .await;
        let _ = sqlx::query("DELETE FROM wallet.balances WHERE player_id = $1::uuid")
            .bind(pid)
            .execute(pool)
            .await;
        let _ = asyncevents::testing::cleanup_events(pool, "player_id", pid).await;
    }
    for code in currencies {
        let _ = sqlx::query("DELETE FROM wallet.currencies WHERE code = $1")
            .bind(code)
            .execute(pool)
            .await;
    }
}

fn movement(key: &str, player_id: &str, currency: &str, amount: i64, reason: &str) -> Movement {
    Movement {
        idempotency_key: key.into(),
        player_id: player_id.into(),
        currency: currency.into(),
        amount,
        reason: reason.into(),
    }
}

// ---- 1: pool-path movements ---------------------------------------------

/// A credit then a debit each write their own ledger row, and each row's
/// `balance_after` matches the balance at that point — the D3 write shape end to end.
#[tokio::test]
async fn credit_then_debit_moves_balance_and_writes_ledger() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let currency = unique_currency(&pool).await;
    let pid = unique_player(&pool).await;

    let credit_key = unique_key("credit");
    let balance = svc
        .credit(movement(&credit_key, &pid, &currency, 100, "topup"))
        .await
        .unwrap();
    assert_eq!(balance, 100);

    let debit_key = unique_key("debit");
    let balance = svc
        .debit(movement(&debit_key, &pid, &currency, 40, "spend"))
        .await
        .unwrap();
    assert_eq!(balance, 60);

    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT idempotency_key, balance_after FROM wallet.ledger \
          WHERE player_id = $1::uuid ORDER BY seq",
    )
    .bind(&pid)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows, vec![(credit_key, 100), (debit_key, 60)]);

    cleanup(&pool, &[&pid], &[&currency]).await;
}

// ---- 2: duplicate-key replay licenses #[retry_safe] ---------------------

/// A replayed key returns the ORIGINAL call's stored `balance_after`, even when a
/// DIFFERENT movement landed on the same balance in between. A "re-read the current
/// balance" implementation would return the post-second-credit balance here instead
/// — exactly the divergence that would make `#[retry_safe]` unsafe (D4).
#[tokio::test]
async fn duplicate_key_same_movement_returns_the_original_balance_after() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let currency = unique_currency(&pool).await;
    let pid = unique_player(&pool).await;
    let key = unique_key("dup-same");
    let m = movement(&key, &pid, &currency, 100, "promo");

    let first = svc.credit(m.clone()).await.unwrap();
    assert_eq!(first, 100);

    // A DIFFERENT movement moves the balance in between — the replay must not
    // observe it.
    svc.credit(movement(&unique_key("other"), &pid, &currency, 500, "unrelated"))
        .await
        .unwrap();

    let replay = svc.credit(m).await.unwrap();
    assert_eq!(
        replay, first,
        "a replay must return the ORIGINAL balance_after, not a re-read of the live balance"
    );

    // Same key, same movement, but the player id spelled DIFFERENTLY (uppercase) —
    // `service::player_id_eq` is what recognizes these as the SAME player rather than
    // reporting `Conflict`, which would push the caller to mint a fresh key and move
    // the money twice. Reducing `player_id_eq` to `a == b` fails ONLY this assertion.
    let respelled = movement(&key, &pid.to_uppercase(), &currency, 100, "promo");
    let replay_respelled = svc
        .credit(respelled)
        .await
        .expect("a differently-spelled but equal player_id must still be recognized as the SAME replay");
    assert_eq!(
        replay_respelled, first,
        "a respelled replay must return the ORIGINAL balance_after too"
    );

    let (rows,): (i64,) = sqlx::query_as("SELECT count(*) FROM wallet.ledger WHERE idempotency_key = $1")
        .bind(&key)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        rows, 1,
        "neither replay (identical spelling nor respelled) may write a second ledger row for the key"
    );

    cleanup(&pool, &[&pid], &[&currency]).await;
}

// ---- 3: duplicate key, different movement ---------------------------------

/// A resubmit under the same key with a DIFFERENT movement is `Conflict` (409), never
/// a silent success — including the `reason`-only variant, which pins rev 4's
/// widened `(player, currency, delta, reason)` identity: a narrower
/// `(player, currency, delta)` comparison would let this one through as a Duplicate.
#[tokio::test]
async fn duplicate_key_different_movement_is_409() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let currency = unique_currency(&pool).await;
    let pid = unique_player(&pool).await;

    let key_amount = unique_key("dup-diff-amount");
    svc.credit(movement(&key_amount, &pid, &currency, 100, "promo"))
        .await
        .unwrap();
    let err = svc
        .credit(movement(&key_amount, &pid, &currency, 200, "promo"))
        .await
        .unwrap_err();
    assert_eq!(err.status, Status::Conflict);
    assert!(err.msg.contains("different movement"), "msg = {}", err.msg);

    // Same player/currency/delta, DIFFERENT reason — the branch a narrow identity
    // tuple would silently swallow as a Duplicate.
    let key_reason = unique_key("dup-diff-reason");
    svc.credit(movement(&key_reason, &pid, &currency, 100, "promo"))
        .await
        .unwrap();
    let err = svc
        .credit(movement(&key_reason, &pid, &currency, 100, "refund"))
        .await
        .unwrap_err();
    assert_eq!(
        err.status,
        Status::Conflict,
        "same amount/currency but a DIFFERENT reason must still be a 409, not a silent success"
    );

    let (rows,): (i64,) = sqlx::query_as("SELECT count(*) FROM wallet.ledger WHERE idempotency_key = $1")
        .bind(&key_reason)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 1, "the rejected resubmit must not overwrite or add a row");

    // Same player/amount/reason, DIFFERENT currency — `same_movement`'s `currency`
    // conjunct. Deleting it would let this resubmit collapse into a `Duplicate`
    // carrying the FIRST currency's stored balance, for a movement naming a second
    // currency the caller never got an answer for.
    let other_currency = unique_currency(&pool).await;
    let key_currency = unique_key("dup-diff-currency");
    svc.credit(movement(&key_currency, &pid, &currency, 100, "promo"))
        .await
        .unwrap();
    let err = svc
        .credit(movement(&key_currency, &pid, &other_currency, 100, "promo"))
        .await
        .unwrap_err();
    assert_eq!(
        err.status,
        Status::Conflict,
        "same player/amount/reason but a DIFFERENT currency must still be a 409"
    );

    // Same currency/amount/reason, DIFFERENT player — `same_movement`'s `player_id`
    // conjunct (via `player_id_eq`). Deleting it would let a key minted for one
    // player collapse a second player's movement into a `Duplicate` carrying the
    // FIRST player's balance.
    let other_pid = unique_player(&pool).await;
    let key_player = unique_key("dup-diff-player");
    svc.credit(movement(&key_player, &pid, &currency, 100, "promo"))
        .await
        .unwrap();
    let err = svc
        .credit(movement(&key_player, &other_pid, &currency, 100, "promo"))
        .await
        .unwrap_err();
    assert_eq!(
        err.status,
        Status::Conflict,
        "same currency/amount/reason but a DIFFERENT player must still be a 409"
    );

    cleanup(&pool, &[&pid, &other_pid], &[&currency, &other_currency]).await;
}

// ---- 4: concurrent replay hits the in-tx re-verify arm ---------------------

/// Two concurrent `credit`s sharing one key: exactly one ledger row and a
/// single-application balance. Unlike the sequential duplicate test, this is the
/// only test that can land BOTH callers racing `INSERT ... ON CONFLICT DO NOTHING`
/// at once, driving the losing caller through the in-tx re-`SELECT` arm (D3 step 2)
/// under real contention rather than a call already known to be second.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_same_key_credits_apply_once() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let currency = unique_currency(&pool).await;
    let pid = unique_player(&pool).await;
    let key = unique_key("concurrent");
    let m = movement(&key, &pid, &currency, 77, "concurrent-credit");

    let a = tokio::spawn({
        let svc = svc.clone();
        let m = m.clone();
        async move { svc.credit(m).await }
    });
    let b = tokio::spawn({
        let svc = svc.clone();
        let m = m.clone();
        async move { svc.credit(m).await }
    });
    let ra = a.await.unwrap().unwrap();
    let rb = b.await.unwrap().unwrap();
    assert_eq!(ra, 77);
    assert_eq!(
        rb, 77,
        "the losing concurrent call must replay the SAME balance_after, not a fresh read"
    );

    let (rows,): (i64,) = sqlx::query_as("SELECT count(*) FROM wallet.ledger WHERE idempotency_key = $1")
        .bind(&key)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 1, "exactly one ledger row despite two concurrent callers sharing a key");

    let (balance,): (i64,) = sqlx::query_as(
        "SELECT amount FROM wallet.balances WHERE player_id = $1::uuid AND currency = $2",
    )
    .bind(&pid)
    .bind(&currency)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(balance, 77, "the balance must reflect a SINGLE application, not two");

    cleanup(&pool, &[&pid], &[&currency]).await;
}

// ---- 4b: apply_on's OWN validation (not the wrapper's) ---------------------

/// Calls `Service::apply_on` DIRECTLY on a pool connection with `sign = +1` and a
/// negative amount. `credit`/`debit` cannot reach this branch — `Service::apply`
/// validates before ever calling `apply_on` — so this is the only test in the tree
/// that proves the authority validates itself. Pre-fix (validation only in the pool
/// wrapper) this exact call would have inserted a ledger row and DEBITED a balance
/// while publishing whatever `reason` the caller supplied — precisely what would let
/// Step 8's admin `grant` form debit a player on a negative input.
#[tokio::test]
async fn apply_on_rejects_a_negative_or_zero_amount() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let currency = unique_currency(&pool).await;
    let pid = unique_player(&pool).await;
    let key = unique_key("neg-direct");
    let m = movement(&key, &pid, &currency, -500, "admin-grant");

    let mut conn = pool.acquire().await.unwrap();
    let err = svc
        .apply_on(&mut conn, &m, 1)
        .await
        .expect_err("apply_on must reject a non-positive amount itself, not rely on a caller");
    assert_eq!(err.status, Status::Invalid);
    drop(conn);

    let (rows,): (i64,) = sqlx::query_as("SELECT count(*) FROM wallet.ledger WHERE idempotency_key = $1")
        .bind(&key)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 0, "a rejected movement must write no ledger row");

    let balance: Option<(i64,)> = sqlx::query_as(
        "SELECT amount FROM wallet.balances WHERE player_id = $1::uuid AND currency = $2",
    )
    .bind(&pid)
    .bind(&currency)
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert!(
        balance.is_none(),
        "the balance must be untouched (no row at all) by a rejected movement"
    );

    cleanup(&pool, &[&pid], &[&currency]).await;
}

// ---- 4c: ledger seq is stamped under the balance lock, not at insert time --

/// Two `apply_on` calls on one `(player, currency)` from TWO connections, interleaved
/// so the SECOND caller's ledger INSERT-through-commit lands entirely before the
/// FIRST caller finishes: T1 claims its key and stalls before touching the balance;
/// T2 runs a full `apply_on` (claim, balance update, `seq` stamp) and commits; only
/// then does T1 apply its own balance delta and stamp its `seq`. `ORDER BY seq` must
/// then be non-decreasing in `balance_after` for this credit-only pair — T2's row
/// must sort BEFORE T1's despite T1 having inserted first. This fails against an
/// insert-time `bigserial` default, which would freeze T1's `seq` as the lower of
/// the two (its ledger row round-tripped first) and report the running balance going
/// DOWN on a credit.
#[tokio::test]
async fn ledger_seq_order_matches_balance_order() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let currency = unique_currency(&pool).await;
    let pid = unique_player(&pool).await;
    let key1 = unique_key("seq-t1");
    let key2 = unique_key("seq-t2");

    let mut tx1 = pool.begin().await.unwrap();
    let (ledger_id1, canonical_pid) = svc
        .store
        .insert_ledger_tx(&mut tx1, &key1, &pid, &currency, 100, "credit-t1")
        .await
        .unwrap()
        .expect("T1's ledger insert must win its own fresh key");

    // T2 runs a FULL apply_on and commits BEFORE T1 finishes.
    let m2 = movement(&key2, &pid, &currency, 50, "credit-t2");
    let mut tx2 = pool.begin().await.unwrap();
    let outcome2 = svc.apply_on(&mut tx2, &m2, 1).await.unwrap();
    assert_eq!(outcome2, Outcome::Applied(50));
    tx2.commit().await.unwrap();

    // T1 finishes last: its balance UPDATE observes T2's committed +50, and its
    // `seq` is stamped (`nextval`) AFTER T2's.
    let balance1 = svc
        .store
        .apply_balance_tx(&mut tx1, &pid, &currency, 100)
        .await
        .unwrap();
    assert_eq!(balance1, 150);
    svc.store
        .set_balance_after_tx(&mut tx1, &ledger_id1, balance1)
        .await
        .unwrap();
    tx1.commit().await.unwrap();

    let rows: Vec<(i64,)> = sqlx::query_as(
        "SELECT balance_after FROM wallet.ledger \
          WHERE player_id = $1::uuid AND currency = $2 ORDER BY seq",
    )
    .bind(&canonical_pid)
    .bind(&currency)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![(50,), (150,)],
        "T2's row (balance_after=50) must sort BEFORE T1's (balance_after=150): T2 \
         committed first despite T1 having inserted its ledger row first, got {rows:?}"
    );

    cleanup(&pool, &[&pid], &[&currency]).await;
}

// ---- 4d: two first-ever credits, the fallback INSERT's DO UPDATE arm -------

/// Two first-ever credits for the SAME `(player, currency)` — no balance row exists
/// before either starts — driven concurrently over TWO open transactions/connections
/// via `tokio::join!` (structured, not a `tokio::spawn` race): both calls to the REAL
/// `Store::apply_balance_tx` miss the UPDATE (neither row is visible to the other
/// yet), so both reach the fallback `INSERT ... ON CONFLICT DO UPDATE`. Whichever
/// commits first creates the row and returns its OWN delta; the other's INSERT blocks
/// on that uncommitted row at the Postgres level and, once the winner commits,
/// resolves through the `DO UPDATE` arm, returning the SUMMED total rather than being
/// lost. This is the arm `Store::apply_balance_tx`'s own doc comment names ("two
/// concurrent ones both miss the UPDATE ... neither is lost") and nothing else in the
/// suite drives it — every other credit test targets either an EXISTING row or a
/// single caller.
#[tokio::test]
async fn concurrent_first_ever_credits_both_land_via_do_update() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let currency = unique_currency(&pool).await;
    let pid = unique_player(&pool).await;

    let mut tx1 = pool.begin().await.unwrap();
    let mut tx2 = pool.begin().await.unwrap();

    let t1 = async {
        let balance = svc.store.apply_balance_tx(&mut tx1, &pid, &currency, 30).await.unwrap();
        tx1.commit().await.unwrap();
        balance
    };
    let t2 = async {
        let balance = svc.store.apply_balance_tx(&mut tx2, &pid, &currency, 70).await.unwrap();
        tx2.commit().await.unwrap();
        balance
    };
    let (b1, b2) = tokio::join!(t1, t2);

    // One caller took the fresh-INSERT path (its OWN delta only); the other took the
    // DO UPDATE arm (the SUMMED total) — whichever order Postgres resolved the lock
    // in. Both other outcomes (either seeing only their own delta, or the row ending
    // up short) are exactly the "lost update" this arm exists to prevent.
    assert!(
        (b1 == 30 && b2 == 100) || (b1 == 100 && b2 == 70),
        "one caller must see its own delta (the fresh INSERT), the other the summed \
         total (the DO UPDATE arm); got b1={b1} b2={b2}"
    );

    let (balance,): (i64,) = sqlx::query_as(
        "SELECT amount FROM wallet.balances WHERE player_id = $1::uuid AND currency = $2",
    )
    .bind(&pid)
    .bind(&currency)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        balance, 100,
        "both concurrent first-ever credits must land in the row — neither lost"
    );

    cleanup(&pool, &[&pid], &[&currency]).await;
}

// ---- 5: the balance CHECK, not the aborted-tx trap -------------------------

/// A debit against a MISSING balance row (no prior credit) for a KNOWN currency.
/// Since c712936 this never reaches any CHECK constraint at all — a debit's
/// missing-row UPDATE never falls to the fallback INSERT (`store.rs`'s own doc
/// comment on `apply_balance_tx` says so explicitly). The 409 here is decided
/// ENTIRELY in Rust by `apply_balance_tx`'s `currency_exists_tx` probe (known
/// currency, no row => `BalanceError::OutOfRange`, mapped to `Conflict`) — proving
/// that mapping is this test's job, not proving any CHECK fired. No key is
/// consumed, so the SAME key succeeds once the balance covers it.
#[tokio::test]
async fn debit_beyond_balance_is_409_and_consumes_no_key() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let currency = unique_currency(&pool).await;
    let pid = unique_player(&pool).await;
    let key = unique_key("overdraw");
    let m = movement(&key, &pid, &currency, 50, "overdraw");

    let err = svc.debit(m.clone()).await.unwrap_err();
    assert_eq!(
        err.status,
        Status::Conflict,
        "the balance CHECK must surface as Conflict (409), never Internal"
    );

    let (rows,): (i64,) = sqlx::query_as("SELECT count(*) FROM wallet.ledger WHERE idempotency_key = $1")
        .bind(&key)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 0, "a rejected debit must not consume the idempotency key");

    svc.credit(movement(&unique_key("topup"), &pid, &currency, 100, "topup"))
        .await
        .unwrap();

    let balance = svc.debit(m).await.unwrap();
    assert_eq!(balance, 50, "the same key must succeed once the balance covers it");

    cleanup(&pool, &[&pid], &[&currency]).await;
}

// ---- 5b: the balance CHECK on the UPDATE path itself ------------------------

/// A debit against an EXISTING balance row that would go negative — decided by the
/// CHECK on the RESULTING row of `Store::apply_balance_tx`'s own UPDATE statement.
/// This is the ONLY test in the tree that pins `balances_amount_check`'s LOWER bound
/// at all: drop the CHECK (or widen its lower bound) and only THIS test goes red.
/// Test 5 (`debit_beyond_balance_is_409_and_consumes_no_key`) debits a MISSING row,
/// and since c712936 that path's 409 is decided in Rust by `currency_exists_tx`,
/// never by a CHECK — a debit no longer reaches the fallback INSERT at all, so test
/// 5 would stay green under a dropped CHECK and proves nothing about it.
#[tokio::test]
async fn debit_below_zero_against_an_existing_row_is_409() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let currency = unique_currency(&pool).await;
    let pid = unique_player(&pool).await;

    svc.credit(movement(&unique_key("topup"), &pid, &currency, 100, "topup"))
        .await
        .unwrap();

    let key = unique_key("overdraw-existing");
    let m = movement(&key, &pid, &currency, 300, "overdraw-existing");
    let err = svc.debit(m.clone()).await.unwrap_err();
    assert_eq!(
        err.status,
        Status::Conflict,
        "a debit that would take an EXISTING row negative must be Conflict (409)"
    );

    let (balance,): (i64,) = sqlx::query_as(
        "SELECT amount FROM wallet.balances WHERE player_id = $1::uuid AND currency = $2",
    )
    .bind(&pid)
    .bind(&currency)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(balance, 100, "the balance must be untouched by the rejected debit");

    let (rows,): (i64,) = sqlx::query_as("SELECT count(*) FROM wallet.ledger WHERE idempotency_key = $1")
        .bind(&key)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 0, "the rejected debit must not consume the idempotency key");

    svc.credit(movement(&unique_key("topup2"), &pid, &currency, 300, "topup2"))
        .await
        .unwrap();
    let balance = svc.debit(m).await.unwrap();
    assert_eq!(balance, 100, "the same key must succeed once the balance covers it");

    cleanup(&pool, &[&pid], &[&currency]).await;
}

// ---- 5c: the balance CHECK's upper bound, credit direction -----------------

/// A credit against an EXISTING row already AT the `1e15` ceiling. Seeded by raw SQL
/// (no caller-facing path can reach the ceiling in one movement — `MAX_MOVEMENT_AMOUNT`
/// is 10^12), so this is the only test in the tree that drives `apply_balance_tx`'s
/// UPDATE into the CHECK's UPPER bound rather than its lower one, which is the entire
/// premise of the design's "a bigint overflow (22003) is unreachable" argument for the
/// credit direction.
#[tokio::test]
async fn credit_past_the_ceiling_is_409() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let currency = unique_currency(&pool).await;
    let pid = unique_player(&pool).await;

    sqlx::query(
        "INSERT INTO wallet.balances (player_id, currency, amount) VALUES ($1::uuid, $2, 1000000000000000)",
    )
    .bind(&pid)
    .bind(&currency)
    .execute(&pool)
    .await
    .unwrap();

    let key = unique_key("ceiling");
    let err = svc
        .credit(movement(&key, &pid, &currency, 1, "over-ceiling"))
        .await
        .unwrap_err();
    assert_eq!(
        err.status,
        Status::Conflict,
        "a credit past balances_amount_check's ceiling must be Conflict (409), never Internal"
    );

    let (balance,): (i64,) = sqlx::query_as(
        "SELECT amount FROM wallet.balances WHERE player_id = $1::uuid AND currency = $2",
    )
    .bind(&pid)
    .bind(&currency)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        balance, 1_000_000_000_000_000,
        "the balance must be untouched by the rejected credit"
    );

    let (rows,): (i64,) = sqlx::query_as("SELECT count(*) FROM wallet.ledger WHERE idempotency_key = $1")
        .bind(&key)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 0, "a rejected credit must not consume the idempotency key");

    cleanup(&pool, &[&pid], &[&currency]).await;
}

// ---- 6: the FK arm (23503) -------------------------------------------------

#[tokio::test]
async fn unknown_currency_is_400() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let pid = unique_player(&pool).await;

    let err = svc
        .credit(movement(&unique_key("unknown-cur"), &pid, "no-such-currency", 10, "test"))
        .await
        .unwrap_err();
    assert_eq!(err.status, Status::Invalid);
    assert_eq!(err.msg, UNKNOWN_CURRENCY);

    cleanup(&pool, &[&pid], &[]).await;
}

// ---- 6b: the debit direction of the unknown-currency verdict ----------------

/// Before c712936, a debit's missing-row UPDATE fell to the fallback INSERT, whose
/// tentative negative row tripped `balances_amount_check` ahead of the FK trigger, so this
/// exact input answered Conflict (409) where the contract (and the credit direction) promise
/// Invalid (400). The KNOWN-currency debit below pins the OTHER half of the same branch —
/// same "no balance row" starting point, differing ONLY in catalog membership — so the test
/// distinguishes the two verdicts `Store::apply_balance_tx`'s catalog probe decides, rather
/// than merely proving "an error happened".
#[tokio::test]
async fn debit_with_an_unknown_currency_is_400() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let pid = unique_player(&pool).await;

    let err = svc
        .debit(movement(&unique_key("debit-unknown-cur"), &pid, "no-such-currency", 10, "test"))
        .await
        .unwrap_err();
    assert_eq!(
        err.status,
        Status::Invalid,
        "an unknown currency must be Invalid (400) in the debit direction too"
    );
    assert_eq!(err.msg, UNKNOWN_CURRENCY);

    let currency = unique_currency(&pool).await;
    let err = svc
        .debit(movement(&unique_key("debit-known-no-row"), &pid, &currency, 10, "test"))
        .await
        .unwrap_err();
    assert_eq!(
        err.status,
        Status::Conflict,
        "a KNOWN currency with no balance row is Conflict (409) — same input, differing only in catalog membership"
    );
    assert_eq!(err.msg, OUT_OF_RANGE);

    cleanup(&pool, &[&pid], &[&currency]).await;
}

// ---- 7: the malformed-uuid arm (22P02) -------------------------------------

#[tokio::test]
async fn malformed_player_id_is_400() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let currency = unique_currency(&pool).await;

    let err = svc
        .credit(movement(&unique_key("bad-uuid"), "not-a-uuid", &currency, 10, "test"))
        .await
        .unwrap_err();
    assert_eq!(
        err.status,
        Status::Invalid,
        "a malformed player_id must be a 400 (the $n::uuid cast + 22P02 arm), not a 500"
    );

    cleanup(&pool, &[], &[&currency]).await;
}

// ---- 8: durable emit, applied vs replayed ----------------------------------

#[tokio::test]
async fn credit_emits_wallet_changed_once() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let currency = unique_currency(&pool).await;
    let pid = unique_player(&pool).await;

    svc.credit(movement(&unique_key("emit-once"), &pid, &currency, 10, "emit-test"))
        .await
        .unwrap();

    let n = asyncevents::testing::events_count(&pool, "wallet.changed", "player_id", &pid)
        .await
        .unwrap();
    assert_eq!(n, 1, "a single credit must append exactly one wallet.changed event");

    cleanup(&pool, &[&pid], &[&currency]).await;
}

#[tokio::test]
async fn duplicate_credit_emits_no_second_event() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let currency = unique_currency(&pool).await;
    let pid = unique_player(&pool).await;
    let m = movement(&unique_key("emit-dup"), &pid, &currency, 10, "emit-test");

    svc.credit(m.clone()).await.unwrap();
    svc.credit(m).await.unwrap();

    let n = asyncevents::testing::events_count(&pool, "wallet.changed", "player_id", &pid)
        .await
        .unwrap();
    assert_eq!(n, 1, "a replayed key must NOT append a second wallet.changed event");

    cleanup(&pool, &[&pid], &[&currency]).await;
}

// ---- 9: the durable-append-fails direction of the atomic emit -------------

/// THE ATOMIC EMIT PROOF, failure direction: the durable append inside `apply_on`
/// FAILS (injected at the transport seam via `asyncevents::testing::failing_transport`,
/// landing exactly where `apply_on` calls `bus.emit_tx` AFTER the ledger insert and
/// balance update), so the whole pool transaction must roll back — no ledger row and
/// no balance row survive. This is the branch the success tests (1/8) can't reach: it
/// would go green even if the ledger/balance writes committed independently of the
/// event, since those tests never see a failing transport.
#[tokio::test]
async fn event_append_failure_rolls_back_the_balance() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let currency = unique_currency(&pool).await;
    let pid = unique_player(&pool).await;

    let ctx = Context::with_db_and_transport(pool.clone(), asyncevents::testing::failing_transport());
    let module = WalletModule::new();
    module.register(&ctx).unwrap();
    let svc = module.svc();

    let key = unique_key("emit-fail");
    let err = svc
        .credit(movement(&key, &pid, &currency, 10, "emit-fail-test"))
        .await
        .expect_err("emit_tx append failed, so credit must return Err");
    assert_eq!(err.status, Status::Internal, "a durable-append failure surfaces as Internal");

    let (ledger_rows,): (i64,) = sqlx::query_as("SELECT count(*) FROM wallet.ledger WHERE idempotency_key = $1")
        .bind(&key)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(ledger_rows, 0, "the failed durable append must roll back the ledger insert too");

    let balance: Option<(i64,)> = sqlx::query_as(
        "SELECT amount FROM wallet.balances WHERE player_id = $1::uuid AND currency = $2",
    )
    .bind(&pid)
    .bind(&currency)
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert!(balance.is_none(), "no balance row must survive a rolled-back movement");

    cleanup(&pool, &[&pid], &[&currency]).await;
}

// ---- 10: validate_movement, no DB ------------------------------------------

/// The three byte caps and the amount bounds (`0`, negative, `i64::MIN`, `i64::MAX`,
/// `MAX_MOVEMENT_AMOUNT + 1`) each reject `Invalid` — no DB required, `validate_movement`
/// returns before any I/O. `i64::MIN` is included as an ordinary bounds case (it does
/// NOT panic here: the sign multiplication that could overflow on `i64::MIN` lives in
/// `apply_on`, downstream of this same guard, and is unreachable with a rejected
/// amount).
#[test]
fn validate_movement_rejects_oversized_fields() {
    let base = movement("k", "11111111-1111-1111-1111-111111111111", "gold", 10, "r");

    let over_key = Movement {
        idempotency_key: "k".repeat(MAX_IDEMPOTENCY_KEY_BYTES + 1),
        ..base.clone()
    };
    assert_eq!(validate_movement(&over_key).unwrap_err().status, Status::Invalid);

    let over_currency = Movement {
        currency: "c".repeat(MAX_CURRENCY_CODE_BYTES + 1),
        ..base.clone()
    };
    assert_eq!(validate_movement(&over_currency).unwrap_err().status, Status::Invalid);

    let over_reason = Movement {
        reason: "r".repeat(MAX_REASON_BYTES + 1),
        ..base.clone()
    };
    assert_eq!(validate_movement(&over_reason).unwrap_err().status, Status::Invalid);

    // The three empty-field branches. `idempotency_key == ""` is the load-bearing one:
    // remove that branch and two DIFFERENT movements sent with an empty key collide on
    // the ledger's `UNIQUE (idempotency_key)`, so the second becomes a false
    // `Duplicate`/409 instead of the `Invalid`/400 this pins.
    let empty_key = Movement {
        idempotency_key: "".into(),
        ..base.clone()
    };
    assert_eq!(validate_movement(&empty_key).unwrap_err().status, Status::Invalid);

    let empty_player = Movement {
        player_id: "".into(),
        ..base.clone()
    };
    assert_eq!(validate_movement(&empty_player).unwrap_err().status, Status::Invalid);

    let whitespace_player = Movement {
        player_id: "   ".into(),
        ..base.clone()
    };
    assert_eq!(
        validate_movement(&whitespace_player).unwrap_err().status,
        Status::Invalid,
        "a whitespace-only player_id must be rejected too (the check is on the trimmed value)"
    );

    let empty_currency = Movement {
        currency: "".into(),
        ..base.clone()
    };
    assert_eq!(validate_movement(&empty_currency).unwrap_err().status, Status::Invalid);

    for amount in [0, -1, i64::MIN, i64::MAX, MAX_MOVEMENT_AMOUNT + 1] {
        let m = Movement { amount, ..base.clone() };
        assert_eq!(
            validate_movement(&m).unwrap_err().status,
            Status::Invalid,
            "amount {amount} must be rejected"
        );
    }

    // The boundary itself must pass.
    let at_cap = Movement { amount: MAX_MOVEMENT_AMOUNT, ..base.clone() };
    assert!(validate_movement(&at_cap).is_ok());
}

// ---- 11: the optional starter grant on `player.registered` (Step 6) ---------
//
// POSTURE A is the property under test: `grant_starter` must never return `Err` for a
// data-quality problem, because an `Err` backs the ONE subscription off and, after 20 of
// them, pauses `wallet.player-registered.v1` for every subsequent player. So each skip
// test carries a non-poisoning proof as well as a "nothing was granted" assertion — the
// two together are what a poisoned handler cannot satisfy.

/// The subscription id `WalletModule::init` registers — an immutable contract, so the
/// tests name it by the same literal the module does rather than deriving it.
const STARTER_SUB: &str = "wallet.player-registered.v1";

/// A TEST-ONLY subscription id (never registered by shipping code — `topiccheck` scans
/// module sources, and this one lives under `#[cfg(test)]`). Its handler always fails; it
/// is the positive control for the non-poisoning assertions.
const DECOY_SUB: &str = "wallet.tests.poison-decoy.v1";

/// The starter tests share ONE durable subscription row (and reset its checkpoint), so
/// they must not interleave — they serialize on this lock rather than relying on the
/// caller having passed `--test-threads=1`.
static STARTER_SUB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The two starter knobs behind interior mutability, so a test can change them BETWEEN
/// two deliveries with no wallet-side refresh step — which is the whole claim of
/// "no wallet-owned second cache" (D9).
struct FakeConfig {
    currency: Mutex<String>,
    amount: Mutex<i64>,
}

impl FakeConfig {
    fn new(currency: &str, amount: i64) -> Arc<FakeConfig> {
        Arc::new(FakeConfig {
            currency: Mutex::new(currency.into()),
            amount: Mutex::new(amount),
        })
    }
}

impl Config for FakeConfig {
    fn get_string(&self, ns: &str, key: &str, def: &str) -> String {
        if ns == "wallet" && key == "starter_currency" {
            self.currency.lock().unwrap().clone()
        } else {
            def.into()
        }
    }
    fn get_bool(&self, _ns: &str, _key: &str, def: bool) -> bool {
        def
    }
    fn get_int(&self, ns: &str, key: &str, def: i64) -> i64 {
        if ns == "wallet" && key == "starter_amount" {
            *self.amount.lock().unwrap()
        } else {
            def
        }
    }
    fn get(&self, _ns: &str, _key: &str) -> Option<String> {
        None
    }
}

/// A config with NOTHING written: every getter answers the CALLER's compiled default,
/// which is exactly what an operator who never wrote a `wallet/starter_*` row has. It
/// exercises the defaults through the real read path instead of restating them.
struct UnsetConfig;

impl Config for UnsetConfig {
    fn get_string(&self, _ns: &str, _key: &str, def: &str) -> String {
        def.into()
    }
    fn get_bool(&self, _ns: &str, _key: &str, def: bool) -> bool {
        def
    }
    fn get_int(&self, _ns: &str, _key: &str, def: i64) -> i64 {
        def
    }
    fn get(&self, _ns: &str, _key: &str) -> Option<String> {
        None
    }
}

/// Drops the subscription's checkpoint row so the next `reconcile` re-materializes it at
/// `AfterRegistration` = now. Without this a test inherits whatever cursor an earlier run
/// left behind and replays every `player.registered` still in the shared log — foreign
/// payloads this suite makes no claim about.
async fn reset_starter_subscription(pool: &PgPool) {
    sqlx::query("DELETE FROM asyncevents.subscriptions WHERE subscription_id = $1")
        .bind(STARTER_SUB)
        .execute(pool)
        .await
        .unwrap();
}

/// Wires the module the way `app::run` does — `register` THEN `init` — over a
/// hand-driven durable transport. `init` is the piece the pool-path fixture (`wired`)
/// deliberately skips, and it is what records the starter subscription.
///
/// The trailing `deliver_all` is ORDERING-CRITICAL, not a warm-up: `AfterRegistration`
/// stamps the cursor with the RECONCILING transaction's xid, and reconcile happens inside
/// `deliver_all`. Reconciling only after the test's `emit_tx` would place the checkpoint
/// PAST the very event under test, and every one of these tests would pass vacuously with
/// nothing ever delivered.
async fn wired_for_delivery(
    pool: &PgPool,
    cfg: Arc<dyn Config>,
) -> (Context, Arc<Service>, asyncevents::testing::TestTransport) {
    ensure_schema(pool).await;
    reset_starter_subscription(pool).await;
    let transport = asyncevents::testing::transport(pool.clone());
    let ctx = Context::with_db_and_transport(pool.clone(), transport.handle());
    ctx.registry().provide::<dyn Config>(key("config", "reader"), cfg);
    let w = WalletModule::new();
    w.register(&ctx).unwrap();
    w.init(&ctx).unwrap();
    let drained = transport.deliver_all().await.unwrap();
    assert_eq!(
        drained, 0,
        "a freshly reset AfterRegistration checkpoint must start with nothing eligible"
    );
    (ctx, w.svc(), transport)
}

/// Appends a durable `player.registered` in its own committed transaction — the shape
/// accounts uses inside its registration store tx.
async fn emit_registered(ctx: &Context, pool: &PgPool, player_id: &str) {
    let mut tx = pool.begin().await.unwrap();
    let registered = accountsevents::PlayerRegistered {
        player_id: player_id.into(),
        display_name: "Test Player".into(),
        provider: "dev".into(),
    };
    ctx.bus()
        .emit_tx(
            AnyTx::new(&mut *tx),
            &accountsevents::PLAYER_REGISTERED,
            &registered,
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn balance_of(pool: &PgPool, player_id: &str, currency: &str) -> Option<i64> {
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT amount FROM wallet.balances WHERE player_id = $1::uuid AND currency = $2",
    )
    .bind(player_id)
    .bind(currency)
    .fetch_all(pool)
    .await
    .unwrap()
    .into_iter()
    .next();
    row.map(|(amount,)| amount)
}

/// The ledger rows carrying the grant's DETERMINISTIC key — `starter:{player_id}` is what
/// makes a redelivery collapse to `Outcome::Duplicate`, so the key is asserted by being
/// the thing looked up, not by a separate equality.
async fn starter_ledger_rows(pool: &PgPool, player_id: &str) -> Vec<(String, i64, i64, String)> {
    sqlx::query_as(
        "SELECT currency, delta, balance_after, reason FROM wallet.ledger \
          WHERE idempotency_key = $1 ORDER BY seq",
    )
    .bind(format!("starter:{player_id}"))
    .fetch_all(pool)
    .await
    .unwrap()
}

/// THE non-poisoning proof, direct form: a handler that returned `Err` leaves
/// `consecutive_failures = 1` plus a `next_attempt_at` backoff here (`worker::record_failure`),
/// and at 20 it flips `state` to `paused` — withholding the grant from every LATER player.
/// "No grant happened" alone is satisfied by a poisoned handler; this is not.
async fn assert_subscription_unpoisoned(pool: &PgPool) {
    let (state, failures, last_error): (String, i32, Option<String>) = sqlx::query_as(
        "SELECT state, consecutive_failures, last_error FROM asyncevents.subscriptions \
          WHERE subscription_id = $1",
    )
    .bind(STARTER_SUB)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(state, "active", "the starter subscription must still be active");
    assert_eq!(
        failures, 0,
        "a data-quality verdict must return Ok(()), never Err; last_error = {last_error:?}"
    );
}

/// A currency code that is NOT in the catalog (never inserted).
async fn absent_currency(pool: &PgPool) -> String {
    let (suffix,): (String,) = sqlx::query_as("SELECT substr(gen_random_uuid()::text, 1, 12)")
        .fetch_one(pool)
        .await
        .unwrap();
    format!("x{suffix}")
}

// ---- 11.1: the happy path, over the REAL plane -----------------------------

/// The one starter test driven by a real `asyncevents::Plane` — background pull workers,
/// NOTIFY wake-up, the plane's own delivery sessions — rather than the hand-driven
/// transport the skip tests use. It is the assertion that the subscription `init`
/// registers is actually reachable by the shipped plane, not merely by a test driver;
/// asserting only PRESENCE, it needs no barrier and cannot race.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn starter_grant_credits_a_new_player() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = STARTER_SUB_LOCK.lock().await;
    ensure_schema(&pool).await;
    reset_starter_subscription(&pool).await;
    let currency = unique_currency(&pool).await;
    let pid = unique_player(&pool).await;

    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let mut plane = asyncevents::Plane::new(pool.clone(), dsn).unwrap();
    let ctx = Context::with_db_and_transport(pool.clone(), plane.transport());
    ctx.registry().provide::<dyn Config>(
        key("config", "reader"),
        FakeConfig::new(&currency, 250) as Arc<dyn Config>,
    );
    let w = WalletModule::new();
    w.register(&ctx).unwrap();
    w.init(&ctx).unwrap();

    // start() reconciles the AfterRegistration checkpoint; the emit must follow it.
    plane.start().await.unwrap();
    emit_registered(&ctx, &pool, &pid).await;

    let mut granted = None;
    for _ in 0..50 {
        if let Some(balance) = balance_of(&pool, &pid, &currency).await {
            granted = Some(balance);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    plane.stop().await;

    assert_eq!(
        granted,
        Some(250),
        "the plane's own pull workers must land the configured starter grant"
    );
    assert_eq!(
        starter_ledger_rows(&pool, &pid).await,
        vec![(currency.clone(), 250, 250, REASON.to_string())],
        "exactly one ledger row, keyed starter:{pid}, at the configured amount"
    );
    assert_subscription_unpoisoned(&pool).await;

    cleanup(&pool, &[&pid], &[&currency]).await;
}

// ---- 11.2: the feature is OFF unless configured ----------------------------

/// The "optional" half. A config with nothing written makes `starter_spec` answer the
/// compiled defaults, and the empty-currency/zero-amount arm must skip. Change either
/// default to something non-empty and this is the test that goes red — the event IS
/// delivered (`delivered == 1`), so a green run cannot be explained by "nothing ran".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn starter_grant_is_off_by_default() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = STARTER_SUB_LOCK.lock().await;
    let (ctx, _svc, transport) = wired_for_delivery(&pool, Arc::new(UnsetConfig)).await;
    let pid = unique_player(&pool).await;

    emit_registered(&ctx, &pool, &pid).await;
    assert_eq!(
        transport.deliver_all().await.unwrap(),
        1,
        "the registration must be DELIVERED — a faulted delivery is never counted"
    );

    let balances: Vec<(String, i64)> =
        sqlx::query_as("SELECT currency, amount FROM wallet.balances WHERE player_id = $1::uuid")
            .bind(&pid)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(
        balances.is_empty(),
        "the compiled defaults ({STARTER_CURRENCY:?}, {STARTER_AMOUNT}) must grant nothing, got {balances:?}"
    );
    assert!(
        starter_ledger_rows(&pool, &pid).await.is_empty(),
        "an unconfigured starter grant must not consume its idempotency key either"
    );
    assert_subscription_unpoisoned(&pool).await;

    cleanup(&pool, &[&pid], &[]).await;
}

// ---- 11.3a: the STRUCTURAL half of the by-construction argument ------------

/// The catalog's own CHECK, asserted directly. D9's claim that the handler needs no
/// repeated `validate_movement` rests on a currency row longer than the contract's
/// 32-byte cap being IMPOSSIBLE: such a row would pass `currency_exists_tx` and then be
/// rejected by `validate_movement` inside `apply_on` — an `Err` on the delivery path,
/// which is the one thing posture A forbids. Drop `currencies_code_len_check` and only
/// this test notices.
#[tokio::test]
async fn catalog_rejects_an_oversized_currency_code() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let code = "c".repeat(MAX_CURRENCY_CODE_BYTES + 1);

    let err = sqlx::query(
        "INSERT INTO wallet.currencies (code, display_name, kind, decimals) \
         VALUES ($1, $1, 'soft', 0)",
    )
    .bind(&code)
    .execute(&pool)
    .await
    .expect_err("a currency code past the contract's byte cap must not be storable");
    let db = err
        .as_database_error()
        .expect("a CHECK violation, not a client-side error");
    assert_eq!(db.code().as_deref(), Some("23514"));
    assert_eq!(
        db.constraint(),
        Some("currencies_code_len_check"),
        "the rejection must come from the length CHECK, not some other constraint"
    );

    let (rows,): (i64,) = sqlx::query_as("SELECT count(*) FROM wallet.currencies WHERE code = $1")
        .bind(&code)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 0, "no oversized catalog row may survive");
}

// ---- 11.3b: an absurd configured amount -----------------------------------

/// `starter_amount = i64::MAX`. Without the `amount > MAX_MOVEMENT_AMOUNT` clamp this
/// reaches `apply_on`, whose `validate_movement` answers `Invalid` — which the handler
/// would surface as `Err`, faulting the subscription (D2's overflow arm is the reason the
/// clamp exists at all). The proof is BOTH forms: `consecutive_failures = 0` after the
/// skip, and a second, well-formed registration that still gets delivered and granted —
/// which a backed-off subscription (`next_attempt_at` in the future) could not do.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn starter_grant_skips_an_absurd_configured_amount_without_poisoning() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = STARTER_SUB_LOCK.lock().await;
    let currency = unique_currency(&pool).await;
    let cfg = FakeConfig::new(&currency, i64::MAX);
    let (ctx, _svc, transport) = wired_for_delivery(&pool, cfg.clone() as Arc<dyn Config>).await;
    let absurd = unique_player(&pool).await;
    let sane = unique_player(&pool).await;

    emit_registered(&ctx, &pool, &absurd).await;
    assert_eq!(
        transport.deliver_all().await.unwrap(),
        1,
        "an out-of-range starter_amount must still DELIVER the event (Ok), not fault it"
    );
    assert!(
        balance_of(&pool, &absurd, &currency).await.is_none(),
        "an out-of-range starter_amount must grant nothing"
    );
    assert!(starter_ledger_rows(&pool, &absurd).await.is_empty());
    assert_subscription_unpoisoned(&pool).await;

    *cfg.amount.lock().unwrap() = 40;
    emit_registered(&ctx, &pool, &sane).await;
    assert_eq!(
        transport.deliver_all().await.unwrap(),
        1,
        "the NEXT player must still be delivered — a backed-off subscription delivers nothing"
    );
    assert_eq!(
        balance_of(&pool, &sane, &currency).await,
        Some(40),
        "one player's bad config must not cost every later player their grant"
    );

    cleanup(&pool, &[&absurd, &sane], &[&currency]).await;
}

// ---- 11.3: a configured currency the catalog does not hold -----------------

/// The `currency_exists_tx` pre-check. Letting the FK fire instead would abort the
/// DELIVERY transaction, after which the plane's checkpoint `UPDATE` fails with 25P02 —
/// so the subscription poisons on the very error posture A means to tolerate. Removing
/// the pre-check leaves the "no grant" assertion below green and turns the two
/// non-poisoning assertions red, which is the point of pairing them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn starter_grant_skips_unknown_currency_without_poisoning() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = STARTER_SUB_LOCK.lock().await;
    let absent = absent_currency(&pool).await;
    let cfg = FakeConfig::new(&absent, 75);
    let (ctx, _svc, transport) = wired_for_delivery(&pool, cfg.clone() as Arc<dyn Config>).await;
    let early = unique_player(&pool).await;
    let later = unique_player(&pool).await;

    emit_registered(&ctx, &pool, &early).await;
    assert_eq!(
        transport.deliver_all().await.unwrap(),
        1,
        "an uncatalogued starter_currency must still DELIVER the event (Ok), not fault it"
    );
    assert!(
        balance_of(&pool, &early, &absent).await.is_none(),
        "an uncatalogued starter_currency must grant nothing"
    );
    assert!(starter_ledger_rows(&pool, &early).await.is_empty());
    assert_subscription_unpoisoned(&pool).await;

    let currency = unique_currency(&pool).await;
    *cfg.currency.lock().unwrap() = currency.clone();
    emit_registered(&ctx, &pool, &later).await;
    assert_eq!(
        transport.deliver_all().await.unwrap(),
        1,
        "the NEXT player must still be delivered — a backed-off subscription delivers nothing"
    );
    assert_eq!(
        balance_of(&pool, &later, &currency).await,
        Some(75),
        "correcting the config must grant the next player, proving the skip cost no backoff"
    );

    cleanup(&pool, &[&early, &later], &[&currency]).await;
}

// ---- 11.3c: a player_id that is not uuid-shaped ----------------------------

/// `is_uuid_text`'s pre-check (`e5df4f7`). Before it existed, this payload reached
/// `apply_on`, whose `$1::uuid` cast on the ledger insert raised 22P02 — an `Err` on the
/// delivery path, poisoning the subscription for every later player. `balance_of`'s own
/// `$1::uuid` cast can't be reused here (it would panic on this input the same way the
/// old handler code faulted), so the "no balance" check below casts the COLUMN to text
/// instead of casting the parameter to uuid.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn starter_grant_skips_a_malformed_player_id_without_poisoning() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = STARTER_SUB_LOCK.lock().await;
    let currency = unique_currency(&pool).await;
    let (ctx, _svc, transport) =
        wired_for_delivery(&pool, FakeConfig::new(&currency, 90) as Arc<dyn Config>).await;
    let malformed = "not-a-uuid";
    let sane = unique_player(&pool).await;

    emit_registered(&ctx, &pool, malformed).await;
    assert_eq!(
        transport.deliver_all().await.unwrap(),
        1,
        "a non-uuid player_id must still DELIVER the event (Ok), not fault it"
    );
    let (balances,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM wallet.balances WHERE player_id::text = $1")
            .bind(malformed)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(balances, 0, "a non-uuid player_id must grant nothing");
    assert!(starter_ledger_rows(&pool, malformed).await.is_empty());
    assert_subscription_unpoisoned(&pool).await;

    emit_registered(&ctx, &pool, &sane).await;
    assert_eq!(
        transport.deliver_all().await.unwrap(),
        1,
        "the NEXT player must still be delivered — a backed-off subscription delivers nothing"
    );
    assert_eq!(
        balance_of(&pool, &sane, &currency).await,
        Some(90),
        "one malformed payload must not cost every later player their grant"
    );

    cleanup(&pool, &[&sane], &[&currency]).await;
}

// ---- 11.4: at-least-once redelivery ---------------------------------------

/// Two `player.registered` events for the SAME player — the at-least-once shape, driven
/// end to end rather than by calling `grant_starter` twice by hand. The deterministic
/// `starter:{player_id}` key must collapse the second into `Outcome::Duplicate`: one
/// ledger row, a single-application balance, and NO second `wallet.changed` (a consumer
/// of that topic would otherwise see a credit that never happened). Both deliveries are
/// counted, so the duplicate is proven to have run and returned `Ok`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn starter_grant_is_idempotent_across_redelivery() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = STARTER_SUB_LOCK.lock().await;
    let currency = unique_currency(&pool).await;
    let (ctx, _svc, transport) =
        wired_for_delivery(&pool, FakeConfig::new(&currency, 120) as Arc<dyn Config>).await;
    let pid = unique_player(&pool).await;

    emit_registered(&ctx, &pool, &pid).await;
    emit_registered(&ctx, &pool, &pid).await;
    assert_eq!(
        transport.deliver_all().await.unwrap(),
        2,
        "BOTH copies must be delivered Ok — a duplicate that Err'd would fault instead"
    );

    assert_eq!(
        balance_of(&pool, &pid, &currency).await,
        Some(120),
        "the balance must reflect a SINGLE application of the starter grant"
    );
    assert_eq!(
        starter_ledger_rows(&pool, &pid).await.len(),
        1,
        "the deterministic starter key must yield exactly one ledger row"
    );
    let changed = asyncevents::testing::events_count(&pool, "wallet.changed", "player_id", &pid)
        .await
        .unwrap();
    assert_eq!(
        changed, 1,
        "the redelivery must not append a second wallet.changed"
    );
    assert_subscription_unpoisoned(&pool).await;

    cleanup(&pool, &[&pid], &[&currency]).await;
}

// ---- 11.4b: the POSITIVE CONTROL for every non-poisoning assertion above ----

/// A second, TEST-ONLY subscription on the same topic whose handler always returns `Err`,
/// delivered in the same pass as wallet's. It is the decoy that proves the instrument:
/// without it, `deliver_all() == 1` and `consecutive_failures == 0` are assertions nobody
/// has shown can fail, and every skip test above would be "green by absence of errors".
///
/// It pins all three mechanics the skip tests rest on: a faulting handler is NOT counted
/// by `deliver_all`, it DOES leave `consecutive_failures = 1` + a `last_error`, and it
/// stops receiving on the next pass (the backoff that would withhold the grant from every
/// later player). Wallet's own subscription runs alongside and is unaffected — the two
/// checkpoints are independent, which is why one module's poison is one module's problem.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_faulting_handler_is_uncounted_backed_off_and_visible_in_the_catalog() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = STARTER_SUB_LOCK.lock().await;
    ensure_schema(&pool).await;
    reset_starter_subscription(&pool).await;
    let _ = sqlx::query("DELETE FROM asyncevents.subscriptions WHERE subscription_id = $1")
        .bind(DECOY_SUB)
        .execute(&pool)
        .await;

    let currency = unique_currency(&pool).await;
    let transport = asyncevents::testing::transport(pool.clone());
    let ctx = Context::with_db_and_transport(pool.clone(), transport.handle());
    ctx.registry().provide::<dyn Config>(
        key("config", "reader"),
        FakeConfig::new(&currency, 60) as Arc<dyn Config>,
    );
    let w = WalletModule::new();
    w.register(&ctx).unwrap();
    w.init(&ctx).unwrap();
    ctx.bus().on_tx(
        bus::SubscriptionSpec {
            id: DECOY_SUB,
            start: bus::StartPosition::AfterRegistration,
        },
        &accountsevents::PLAYER_REGISTERED,
        |_delivery, _e: accountsevents::PlayerRegistered| {
            Box::pin(async move {
                Err(bus::Error::transport(std::io::Error::other(
                    "decoy handler: always fails",
                )))
            })
        },
    );
    // Reconciles BOTH checkpoints before anything is emitted.
    assert_eq!(transport.deliver_all().await.unwrap(), 0);

    let first = unique_player(&pool).await;
    emit_registered(&ctx, &pool, &first).await;
    assert_eq!(
        transport.deliver_all().await.unwrap(),
        1,
        "one event, two subscriptions: only wallet's Ok delivery is counted — the decoy's \
         Err is not, which is exactly what `delivered == 1` asserts in the skip tests"
    );
    assert_eq!(balance_of(&pool, &first, &currency).await, Some(60));
    assert_subscription_unpoisoned(&pool).await;

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
        "a handler that returns Err DOES move consecutive_failures — so the `== 0` \
         assertions above are not vacuous"
    );
    assert!(last_error.is_some(), "the failure is recorded, not swallowed");

    let second = unique_player(&pool).await;
    emit_registered(&ctx, &pool, &second).await;
    assert_eq!(
        transport.deliver_all().await.unwrap(),
        1,
        "the SECOND player reaches wallet but NOT the backed-off decoy — the mechanism \
         that would have cost every later player their grant had the handler Err'd"
    );
    assert_eq!(balance_of(&pool, &second, &currency).await, Some(60));

    let _ = sqlx::query("DELETE FROM asyncevents.subscriptions WHERE subscription_id = $1")
        .bind(DECOY_SUB)
        .execute(&pool)
        .await;
    cleanup(&pool, &[&first, &second], &[&currency]).await;
}

// ---- 11.5: the knobs are read live, per delivery ---------------------------

/// The config is re-read on EVERY delivery — D9's "no wallet-owned second cache". A
/// wallet-side snapshot taken at `init` (or memoized on first use) would grant the second
/// player the FIRST player's amount, and only this test would notice.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn starter_grant_reflects_a_live_config_change() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = STARTER_SUB_LOCK.lock().await;
    let currency = unique_currency(&pool).await;
    let cfg = FakeConfig::new(&currency, 100);
    let (ctx, _svc, transport) = wired_for_delivery(&pool, cfg.clone() as Arc<dyn Config>).await;
    let first = unique_player(&pool).await;
    let second = unique_player(&pool).await;

    emit_registered(&ctx, &pool, &first).await;
    assert_eq!(transport.deliver_all().await.unwrap(), 1);
    assert_eq!(balance_of(&pool, &first, &currency).await, Some(100));

    *cfg.amount.lock().unwrap() = 300;
    emit_registered(&ctx, &pool, &second).await;
    assert_eq!(transport.deliver_all().await.unwrap(), 1);
    assert_eq!(
        balance_of(&pool, &second, &currency).await,
        Some(300),
        "the second grant must use the CURRENT config value, with no refresh step"
    );
    assert_eq!(
        balance_of(&pool, &first, &currency).await,
        Some(100),
        "the config change must not retroactively touch the already-granted player"
    );
    assert_subscription_unpoisoned(&pool).await;

    cleanup(&pool, &[&first, &second], &[&currency]).await;
}

// ---- 12: the admin page — the submit authority and the drill-down window ----

/// An [`adminapi::Params`] from literal pairs — the map the portal hands `apply_submit`
/// after allowlisting the rendered form's fields.
fn params(pairs: &[(&str, &str)]) -> adminapi::Params {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

/// [`crate::admin::Rejection`] carries no `Debug` (its two mappings ARE its surface), so
/// the tests flatten it to the text an operator would read.
fn rejection_text(r: crate::admin::Rejection) -> String {
    match r {
        crate::admin::Rejection::Stale => "stale".to_string(),
        crate::admin::Rejection::Rejected(msg) => msg,
        crate::admin::Rejection::Internal(msg) => format!("internal: {msg}"),
    }
}

/// The rendered form's hidden value for `field` — the render-time idempotency key the
/// browser echoes back on submit.
fn hidden_value(content: &adminapi::Content, field: &str) -> String {
    content
        .form
        .as_ref()
        .expect("the wallet page renders an action form")
        .hidden
        .iter()
        .find(|h| h.name == field)
        .unwrap_or_else(|| panic!("no hidden {field:?} on the rendered form"))
        .value
        .clone()
}

/// THE double-submit proof: ONE rendered form, submitted twice with identical values —
/// the browser resubmit (double-click, back-and-repost) that a submit-time key would turn
/// into two distinct movements. The key is minted at RENDER time and echoed as a hidden
/// field, so the second submit replays it and the movement authority collapses it to
/// `Outcome::Duplicate`: one ledger row, one application of the amount.
///
/// `admin_render` (not `admin_content_local`) is driven deliberately — it is the shipping
/// LOCAL path and its `block_in_place` bridge requires the multi-thread runtime, which a
/// plain `#[tokio::test]` would turn into a panic rather than a failure.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_double_submit_of_one_rendered_form_grants_once() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let currency = unique_currency(&pool).await;
    let pid = unique_player(&pool).await;

    let content = admin::admin_render(&svc, &params(&[("player", &pid)])).unwrap();
    let key = hidden_value(&content, "_idem_grant");
    assert!(key.starts_with("admin-grant-"), "key = {key}");

    let submit = params(&[
        ("_action", "grant"),
        ("player_id", &pid),
        ("currency", &currency),
        ("amount", "250"),
        ("reason", "proof grant"),
        ("_idem_grant", &key),
    ]);
    for attempt in 1..=2 {
        admin::apply_submit(&svc, submit.clone())
            .await
            .map_err(rejection_text)
            .unwrap_or_else(|msg| panic!("submit #{attempt} rejected: {msg}"));
    }

    let (rows,): (i64,) = sqlx::query_as("SELECT count(*) FROM wallet.ledger WHERE idempotency_key = $1")
        .bind(&key)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        rows, 1,
        "a resubmit of ONE rendered form must replay its key, not mint a second movement"
    );
    assert_eq!(
        balance_of(&pool, &pid, &currency).await,
        Some(250),
        "the amount must be applied exactly once"
    );

    cleanup(&pool, &[&pid], &[&currency]).await;
}

/// The page size the drill-down asks for. `recent_ledger`'s window arithmetic is
/// limit-agnostic (`limit + 1`, then truncate), so the tests pass it explicitly rather
/// than reaching for `admin`'s private const.
const PAGE: i64 = 50;

/// Seeds `n` ledger rows for one player in ONE statement — the code under test is
/// [`Store::recent_ledger`]'s window, never the writer, and a single INSERT ... SELECT
/// draws the `bigserial` default in `generate_series` order, so row `g = 1` is
/// unambiguously the OLDEST.
async fn seed_ledger(pool: &PgPool, pid: &str, prefix: &str, n: i64) {
    sqlx::query(
        "INSERT INTO wallet.ledger \
             (idempotency_key, player_id, currency, delta, balance_after, reason) \
         SELECT $1::text || '-' || g::text, $2::uuid, 'seed', 1, g, 'seed' \
           FROM generate_series(1, $3) AS g",
    )
    .bind(prefix)
    .bind(pid)
    .bind(n)
    .execute(pool)
    .await
    .unwrap();
}

/// Exactly `limit` rows: the arm that must NOT claim truncation. `recent_ledger` asks for
/// `limit + 1` and gets `limit`, so the surplus row is absent and the page is whole —
/// binding `limit` instead of `limit + 1`, or comparing `>=` instead of `>`, reports a
/// complete history as partial and only this assertion notices.
#[tokio::test]
async fn ledger_page_of_exactly_the_limit_is_not_truncated() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let pid = unique_player(&pool).await;
    seed_ledger(&pool, &pid, &unique_key("page-exact"), PAGE).await;

    let page = svc.store.recent_ledger(&pid, PAGE).await.unwrap();
    assert_eq!(page.rows.len(), PAGE as usize);
    assert!(
        !page.truncated,
        "a page holding the WHOLE history must not report older rows"
    );

    cleanup(&pool, &[&pid], &[]).await;
}

/// One row past the limit: truncation is reported, the page still carries exactly `limit`
/// rows, the window is the NEWEST (`seq` descending), and the row dropped is the OLDEST.
/// Together these kill both the off-by-one and a `LIMIT` that kept the wrong end of the
/// history — a page that silently showed the oldest 50 of 51 movements reads as a complete
/// audit trail while hiding the newest one.
#[tokio::test]
async fn ledger_page_past_the_limit_truncates_the_oldest_row() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let pid = unique_player(&pool).await;
    seed_ledger(&pool, &pid, &unique_key("page-over"), PAGE + 1).await;

    let (oldest,): (i64,) = sqlx::query_as("SELECT min(seq) FROM wallet.ledger WHERE player_id = $1::uuid")
        .bind(&pid)
        .fetch_one(&pool)
        .await
        .unwrap();

    let page = svc.store.recent_ledger(&pid, PAGE).await.unwrap();
    assert_eq!(page.rows.len(), PAGE as usize, "the surplus row must be dropped");
    assert!(page.truncated, "a partial page MUST say it is partial");
    assert!(
        page.rows[0].seq > page.rows[PAGE as usize - 1].seq,
        "the window is newest-first: {} .. {}",
        page.rows[0].seq,
        page.rows[PAGE as usize - 1].seq
    );
    assert!(
        !page.rows.iter().any(|r| r.seq == oldest),
        "the dropped row must be the OLDEST (seq {oldest}), never the newest"
    );

    cleanup(&pool, &[&pid], &[]).await;
}

/// A caller asking for far MORE than the hard ceiling, against exactly `MAX_RECENT_LEDGER`
/// rows. The clamp lowers the request to the ceiling, and the ceiling is met exactly — so
/// the page is whole. A clamp applied to `limit + 1` (or a `truncated` derived from the
/// caller's original limit) would manufacture truncation out of its own bound and report a
/// complete history as partial.
#[tokio::test]
async fn recent_ledger_clamp_does_not_manufacture_truncation() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let pid = unique_player(&pool).await;
    seed_ledger(&pool, &pid, &unique_key("page-clamp"), MAX_RECENT_LEDGER).await;

    let page = svc
        .store
        .recent_ledger(&pid, MAX_RECENT_LEDGER + 500)
        .await
        .unwrap();
    assert_eq!(page.rows.len(), MAX_RECENT_LEDGER as usize);
    assert!(
        !page.truncated,
        "meeting the hard ceiling exactly is a WHOLE history, not a truncated one"
    );

    cleanup(&pool, &[&pid], &[]).await;
}

/// The dev seed is insert-if-absent, and this drives the REAL `migrate` seed loop (a
/// `Service` built with `dev_seed = true`, the module's own `svc` slot), not a hand-picked
/// store call — so it pins the `OnConflict::Skip` argument at the shipping call site.
/// An operator's edit to a seeded currency must SURVIVE the next boot with the flag on;
/// before `b912fa1` the seed wrote `DO UPDATE` and silently reverted it.
///
/// The edit is read back BEFORE it is restored, so a failure cannot leave the shared dev
/// catalog holding the sentinel.
#[tokio::test]
async fn dev_seed_migrate_preserves_an_operator_edit() {
    let Some(pool) = test_pool().await else { return };
    let (ctx, _svc) = wired(&pool).await;
    let seeded = DEV_SEED_CURRENCIES[0].0;

    let module = WalletModule::new();
    module
        .svc
        .set(Arc::new(Service::new(pool.clone(), ctx.bus().clone(), true)))
        .map_err(|_| ())
        .unwrap();
    module.migrate(&ctx).await.unwrap();

    let (original,): (String,) =
        sqlx::query_as("SELECT display_name FROM wallet.currencies WHERE code = $1")
            .bind(seeded)
            .fetch_one(&pool)
            .await
            .expect("the dev seed must have created the row");

    let edited = format!("Edited by an operator {}", std::process::id());
    sqlx::query("UPDATE wallet.currencies SET display_name = $2 WHERE code = $1")
        .bind(seeded)
        .bind(&edited)
        .execute(&pool)
        .await
        .unwrap();
    module.migrate(&ctx).await.unwrap();

    let (after,): (String,) =
        sqlx::query_as("SELECT display_name FROM wallet.currencies WHERE code = $1")
            .bind(seeded)
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query("UPDATE wallet.currencies SET display_name = $2 WHERE code = $1")
        .bind(seeded)
        .bind(&original)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        after, edited,
        "a boot with WALLET_DEV_SEED on must not revert an operator's catalog edit"
    );
}

/// The other half of the seed's contract: it still GUARANTEES the code exists. A row an
/// operator deleted is recreated by the next seed write — a seed reduced to a plain
/// `INSERT` (or one that gave up on conflicts entirely) fails here.
#[tokio::test]
async fn seed_write_recreates_a_deleted_currency() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let code = unique_currency(&pool).await;
    let mut conn = svc.store.pool.acquire().await.unwrap();

    sqlx::query("DELETE FROM wallet.currencies WHERE code = $1")
        .bind(&code)
        .execute(&pool)
        .await
        .unwrap();
    svc.store
        .write_currency_tx(&mut conn, &code, "Reseeded", "soft", 0, OnConflict::Skip)
        .await
        .unwrap();

    let (rows,): (i64,) = sqlx::query_as("SELECT count(*) FROM wallet.currencies WHERE code = $1")
        .bind(&code)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 1, "the seed must recreate a code an operator deleted");

    drop(conn);
    cleanup(&pool, &[], &[&code]).await;
}

/// The rendered `SEQ` column carries the RAW ordering value: strictly decreasing (newest
/// first) and deliberately NOT contiguous. Contiguity is asserted FALSE by construction —
/// every movement burns one sequence value on its ledger INSERT before drawing the real one
/// under the balance row lock, so two sequential movements are at least two apart. A future
/// "fix" that renumbers the column to look like a tidy 1,2,3 audit trail turns the gap
/// assertion red, which is the point: the guarantee is ordering, not contiguity, and a
/// renumbered column would invent a promise the ledger does not make.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rendered_ledger_seq_column_is_decreasing_and_gapped() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let currency = unique_currency(&pool).await;
    let pid = unique_player(&pool).await;
    for (amount, reason) in [(100, "first"), (50, "second"), (25, "third")] {
        svc.credit(movement(&unique_key("seqcol"), &pid, &currency, amount, reason))
            .await
            .unwrap();
    }

    let content = admin::admin_render(&svc, &params(&[("player", &pid)])).unwrap();
    let table = content.table.as_ref().expect("the drill-down renders the ledger table");
    assert_eq!(table.columns[0], "SEQ", "SEQ leads — it is what the rows are ordered by");
    let seqs: Vec<i64> = table
        .rows
        .iter()
        .map(|r| r[0].text.parse::<i64>().expect("SEQ renders the raw value"))
        .collect();
    assert_eq!(seqs.len(), 3);
    for pair in seqs.windows(2) {
        assert!(
            pair[0] > pair[1],
            "SEQ must be strictly decreasing (newest first): {seqs:?}"
        );
        assert!(
            pair[0] - pair[1] > 1,
            "SEQ is monotonic but GAPPED — a contiguous column means the value was \
             renumbered for looks: {seqs:?}"
        );
    }

    cleanup(&pool, &[&pid], &[&currency]).await;
}

// ---- 13: the catalog's operator input, capped at BOTH levels ----------------

fn catalog_params(code: &str, display_name: &str, kind: &str, decimals: &str) -> adminapi::Params {
    params(&[
        ("_action", "create-currency"),
        ("code", code),
        ("display_name", display_name),
        ("kind", kind),
        ("decimals", decimals),
    ])
}

async fn catalog_rows(pool: &PgPool, code: &str) -> i64 {
    let (rows,): (i64,) = sqlx::query_as("SELECT count(*) FROM wallet.currencies WHERE code = $1")
        .bind(code)
        .fetch_one(pool)
        .await
        .unwrap();
    rows
}

/// The SUBMIT PATH's verdict on the catalog caps, per field and in BOTH directions: one
/// byte past the cap is rejected with a message NAMING that field and NO row is written,
/// and the cap itself is accepted. It does not say WHICH level rejected — `catalog_rejection`
/// maps every mirroring CHECK back through the same `CATALOG_CAPS` and the same `over_cap`,
/// so the Rust caps and the column CHECKs are indistinguishable from here (that is the point
/// of the DDL-equality test below, and the reason the direct `check_catalog_caps` test
/// exists). What this pins is the path end to end: the field's own ceiling reaches the
/// operator and the write does not land.
#[tokio::test]
async fn catalog_form_rejects_each_oversized_field_by_name() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let base = unique_currency(&pool).await;
    let mut created = vec![base.clone()];

    let over_code = "c".repeat(MAX_CURRENCY_CODE_BYTES + 1);
    let msg = rejection_text(
        crate::admin::apply_submit(&svc, catalog_params(&over_code, "Name", "soft", "0"))
            .await
            .expect_err("an over-long code must be refused"),
    );
    assert!(
        msg.contains("currency code") && msg.contains(&MAX_CURRENCY_CODE_BYTES.to_string()),
        "the rejection must name the offending field: {msg}"
    );
    assert_eq!(catalog_rows(&pool, &over_code).await, 0);

    let over_name = "n".repeat(MAX_CURRENCY_DISPLAY_NAME_BYTES + 1);
    let code_name = format!("{base}n");
    let msg = rejection_text(
        crate::admin::apply_submit(&svc, catalog_params(&code_name, &over_name, "soft", "0"))
            .await
            .expect_err("an over-long display name must be refused"),
    );
    assert!(
        msg.contains("display name") && msg.contains(&MAX_CURRENCY_DISPLAY_NAME_BYTES.to_string()),
        "the rejection must name the offending field: {msg}"
    );
    assert_eq!(catalog_rows(&pool, &code_name).await, 0);

    let over_kind = "k".repeat(MAX_CURRENCY_KIND_BYTES + 1);
    let code_kind = format!("{base}k");
    let msg = rejection_text(
        crate::admin::apply_submit(&svc, catalog_params(&code_kind, "Name", &over_kind, "0"))
            .await
            .expect_err("an over-long kind must be refused"),
    );
    assert!(
        msg.contains("kind") && msg.contains(&MAX_CURRENCY_KIND_BYTES.to_string()),
        "the rejection must name the offending field: {msg}"
    );
    assert_eq!(catalog_rows(&pool, &code_kind).await, 0);

    let msg = rejection_text(
        crate::admin::apply_submit(&svc, catalog_params(&base, "Name", "soft", "19"))
            .await
            .expect_err("decimals past the range must be refused"),
    );
    assert!(
        msg.contains(&format!("0..={MAX_CURRENCY_DECIMALS}")),
        "the rejection must name the range: {msg}"
    );

    // AT the cap, each field: the ceilings are inclusive, and a field measured against
    // another field's (shorter) bound would fail exactly here.
    let at_name = "n".repeat(MAX_CURRENCY_DISPLAY_NAME_BYTES);
    let at_kind = "k".repeat(MAX_CURRENCY_KIND_BYTES);
    let code_at = format!("{base}a");
    crate::admin::apply_submit(
        &svc,
        catalog_params(&code_at, &at_name, &at_kind, &MAX_CURRENCY_DECIMALS.to_string()),
    )
    .await
    .map_err(rejection_text)
    .unwrap_or_else(|msg| panic!("at-cap catalog input must be accepted: {msg}"));
    created.push(code_at.clone());
    assert_eq!(catalog_rows(&pool, &code_at).await, 1);

    let refs: Vec<&str> = created.iter().map(String::as_str).collect();
    cleanup(&pool, &[], &refs).await;
}

/// The property the direct unit tests cannot reach and the form test cannot separate: the
/// Rust caps run BEFORE any SQL. Proven by construction on a CLOSED pool — `apply_submit`
/// cannot even acquire a connection, so a verdict that still names the field's own ceiling
/// can only have come from the Rust check, never from a column CHECK. The at-cap control
/// below is what keeps this from being vacuous: with a legitimate value the same call is an
/// `Internal` pool error, so the pool really is dead.
#[tokio::test]
async fn catalog_caps_reject_before_any_sql_runs() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let over_code = "c".repeat(MAX_CURRENCY_CODE_BYTES + 1);
    pool.close().await;

    let msg = rejection_text(
        crate::admin::apply_submit(&svc, catalog_params(&over_code, "Name", "soft", "0"))
            .await
            .expect_err("an over-long code must be refused"),
    );
    assert!(
        msg.contains("currency code") && msg.contains(&MAX_CURRENCY_CODE_BYTES.to_string()),
        "with no reachable DB the verdict must still be the Rust cap's: {msg}"
    );

    let msg = rejection_text(
        crate::admin::apply_submit(&svc, catalog_params("ok", "Name", "soft", "19"))
            .await
            .expect_err("decimals past the range must be refused"),
    );
    assert!(
        msg.contains(&format!("0..={MAX_CURRENCY_DECIMALS}")),
        "with no reachable DB the verdict must still be the Rust range's: {msg}"
    );

    let msg = rejection_text(
        crate::admin::apply_submit(&svc, catalog_params("ok", "Name", "soft", "0"))
            .await
            .expect_err("the closed pool must fail a legitimate write"),
    );
    assert!(
        msg.starts_with("internal:"),
        "control: a within-cap write on a closed pool is an Internal error, so the two \
         assertions above really did answer without SQL: {msg}"
    );
}

/// The DB half, on its own: each catalog CHECK rejects a direct INSERT past its bound
/// under the constraint name `admin::catalog_rejection` maps. This is the class fail-safe
/// for every writer that does not go through the admin form — a raw `psql` INSERT, or a
/// future writer that never calls `check_catalog_caps`. It cannot substitute for the Rust
/// half and the Rust half cannot substitute for it: through `apply_submit` the two produce
/// the SAME message, so only the direct calls below separate them.
#[tokio::test]
async fn catalog_columns_reject_oversized_values() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let code = format!("t{}", std::process::id());

    let cases: [(&str, String, i32, &str); 3] = [
        (
            "display_name",
            "n".repeat(MAX_CURRENCY_DISPLAY_NAME_BYTES + 1),
            0,
            "currencies_display_name_len_check",
        ),
        (
            "kind",
            "k".repeat(MAX_CURRENCY_KIND_BYTES + 1),
            0,
            "currencies_kind_len_check",
        ),
        (
            "decimals",
            "ok".to_string(),
            MAX_CURRENCY_DECIMALS + 1,
            "currencies_decimals_range_check",
        ),
    ];
    for (field, value, decimals, constraint) in cases {
        let (display_name, kind) = match field {
            "display_name" => (value.clone(), "soft".to_string()),
            "kind" => ("Name".to_string(), value.clone()),
            _ => ("Name".to_string(), "soft".to_string()),
        };
        let err = sqlx::query(
            "INSERT INTO wallet.currencies (code, display_name, kind, decimals) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(&code)
        .bind(&display_name)
        .bind(&kind)
        .bind(decimals)
        .execute(&pool)
        .await
        .unwrap_err();
        let db = err
            .as_database_error()
            .unwrap_or_else(|| panic!("{field}: a CHECK violation, not a client-side error"));
        assert_eq!(db.code().as_deref(), Some("23514"), "{field}");
        assert_eq!(db.constraint(), Some(constraint), "{field}");
    }
    assert_eq!(catalog_rows(&pool, &code).await, 0, "no over-cap row may survive");
}

// ---------------------------------------------------------------------------
// The catalog cap table vs the DDL — pure, no Postgres.
// ---------------------------------------------------------------------------

/// The Rust caps as their OWN authority, called directly, both directions, per field. This
/// is the only test that separates them from the column CHECKs: through `apply_submit` a
/// deleted `check_catalog_caps` call still answers with a byte-identical message, because
/// `catalog_rejection` looks the fired constraint up in the SAME `CATALOG_CAPS` and formats
/// it with the SAME `over_cap` — so every form assertion stays green while the pre-SQL
/// verdict is gone.
///
/// The per-field message also pins `CATALOG_CAPS`' positional `zip` with
/// `[code, display_name, kind]`: reordering the table without touching the value list would
/// measure a value against the WRONG ceiling and name the WRONG field with no compile error,
/// and the at-cap acceptance catches the direction the rejection cannot.
#[test]
fn check_catalog_caps_names_each_field_at_one_byte_past_its_own_cap() {
    for (index, cap) in crate::admin::CATALOG_CAPS.iter().enumerate() {
        let mut values = ["c".to_string(), "n".to_string(), "k".to_string()];

        values[index] = "x".repeat(cap.max_bytes + 1);
        let msg = rejection_text(
            crate::admin::check_catalog_caps(&values[0], &values[1], &values[2])
                .expect_err(cap.label),
        );
        assert!(
            msg.contains(cap.label) && msg.contains(&cap.max_bytes.to_string()),
            "{}: the rejection must name the field and its own ceiling: {msg}",
            cap.label
        );

        values[index] = "x".repeat(cap.max_bytes);
        crate::admin::check_catalog_caps(&values[0], &values[1], &values[2])
            .map_err(rejection_text)
            .unwrap_or_else(|msg| panic!("{}: the cap itself is inclusive: {msg}", cap.label));
    }
}

/// The `decimals` bound, called directly — the same separation argument as the byte caps:
/// `currencies_decimals_range_check` maps back through `over_decimals`, so through
/// `apply_submit` the two levels word the verdict identically.
#[test]
fn check_decimals_range_accepts_its_bounds_and_names_them_when_refusing() {
    for accepted in [0, MAX_CURRENCY_DECIMALS] {
        crate::admin::check_decimals_range(accepted)
            .map_err(rejection_text)
            .unwrap_or_else(|msg| panic!("decimals {accepted} is within the range: {msg}"));
    }
    for refused in [-1, MAX_CURRENCY_DECIMALS + 1] {
        let msg = rejection_text(
            crate::admin::check_decimals_range(refused).expect_err("outside the range"),
        );
        assert!(
            msg.contains(&format!("0..={MAX_CURRENCY_DECIMALS}")),
            "decimals {refused}: the rejection must name the range: {msg}"
        );
    }
}

/// The one whitespace-normalized `CONSTRAINT <name> CHECK (...)` clause `SCHEMA_DDL`
/// declares, or a panic naming the missing constraint.
fn ddl_clause(constraint: &str) -> String {
    let flat = SCHEMA_DDL.split_whitespace().collect::<Vec<_>>().join(" ");
    let needle = format!("CONSTRAINT {constraint} ");
    let start = flat.find(&needle).unwrap_or_else(|| {
        panic!(
            "SCHEMA_DDL declares no `CONSTRAINT {constraint}` — admin::CATALOG_CAPS names a \
             constraint the schema never creates, so nothing backstops that column and a \
             23514 could never map back to it"
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

/// Each catalog ceiling is stated in TWO languages — `admin::CATALOG_CAPS` in Rust and an
/// `octet_length(...) <= N` CHECK in `SCHEMA_DDL`. Raise the const alone and a legitimate
/// value passes Rust, dies on the column CHECK, and `catalog_rejection` maps the 23514 back
/// through the SAME table, reporting the NEW number for the OLD limit — a lying message.
/// Pure and always-runs, unlike the live-Postgres constraint test beside it, which is
/// simply absent when the DB is.
#[test]
fn every_catalog_cap_matches_its_check_constraint_in_the_ddl() {
    for cap in crate::admin::CATALOG_CAPS {
        let clause = ddl_clause(cap.constraint);
        assert!(
            clause.ends_with(&format!("<= {})", cap.max_bytes)),
            "{}: Rust caps the {} at {} bytes, but SCHEMA_DDL says `{clause}`",
            cap.constraint,
            cap.label,
            cap.max_bytes
        );
        assert!(
            clause.contains("octet_length("),
            "{}: the CHECK must count OCTETS (the Rust twin is str::len), got `{clause}`",
            cap.constraint
        );
    }
}

/// The reverse leg: a `*_len_check` in the DDL that no `CatalogCap` names would fire as an
/// UNMAPPED 23514, which `catalog_rejection` deliberately keeps as `Internal` — operator
/// input reported as a 500.
#[test]
fn every_len_check_in_the_ddl_is_mapped_by_a_catalog_cap() {
    let flat = SCHEMA_DDL.split_whitespace().collect::<Vec<_>>().join(" ");
    let declared: Vec<&str> = flat
        .match_indices("CONSTRAINT ")
        .map(|(index, marker)| {
            flat[index + marker.len()..]
                .split_whitespace()
                .next()
                .unwrap_or_default()
        })
        .filter(|name| name.ends_with("_len_check"))
        .collect();
    assert!(!declared.is_empty(), "SCHEMA_DDL declares no length CHECK");
    for name in declared {
        assert!(
            crate::admin::CATALOG_CAPS
                .iter()
                .any(|cap| cap.constraint == name),
            "SCHEMA_DDL declares `CONSTRAINT {name}` that admin::CATALOG_CAPS does not name — \
             its 23514 stays an unmapped Internal error instead of the operator-input verdict"
        );
    }
}
