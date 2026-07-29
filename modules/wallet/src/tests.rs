use super::*;
use opsapi::Status;
use sqlx::PgPool;
use std::sync::atomic::{AtomicU64, Ordering};
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
            sweep_stale_test_currencies(pool).await;
        })
        .await;
}

/// One-shot, before any test in this binary creates a currency: removes leftover
/// `unique_currency` rows (and their ledger/balance rows) from a run that was
/// interrupted before its own `cleanup` ran. Scoped to THIS file's own naming shape
/// (`t` + 12 hex/dash chars, the exact `unique_currency` pattern) and to rows old
/// enough (2 minutes) that they cannot be the currently-running test binary's own —
/// never a blanket sweep of `wallet.currencies`, which would drop another test's rows.
async fn sweep_stale_test_currencies(pool: &PgPool) {
    let stale: Vec<(String,)> = sqlx::query_as(
        "SELECT code FROM wallet.currencies \
          WHERE code ~ '^t[0-9a-f]{8}-[0-9a-f]{3}$' AND created_at < now() - interval '2 minutes'",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    if stale.is_empty() {
        return;
    }
    let codes: Vec<String> = stale.into_iter().map(|(c,)| c).collect();
    let _ = sqlx::query("DELETE FROM wallet.ledger WHERE currency = ANY($1)")
        .bind(&codes)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM wallet.balances WHERE currency = ANY($1)")
        .bind(&codes)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM wallet.currencies WHERE code = ANY($1)")
        .bind(&codes)
        .execute(pool)
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

    let (rows,): (i64,) = sqlx::query_as("SELECT count(*) FROM wallet.ledger WHERE idempotency_key = $1")
        .bind(&key)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 1, "the replay must not write a second ledger row for the key");

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

    cleanup(&pool, &[&pid], &[&currency]).await;
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
    assert_eq!(rows.len(), 2);
    assert!(
        rows[0].0 <= rows[1].0,
        "seq order must match balance-application order for a credit-only sequence, got {rows:?}"
    );

    cleanup(&pool, &[&pid], &[&currency]).await;
}

// ---- 5: the balance CHECK, not the aborted-tx trap -------------------------

/// The balance CHECK firing must surface as `Conflict` (409), never `Internal` — the
/// aborted-transaction trap D3 warns against (any statement after 23514 on that
/// connection fails 25P02, so a naive "read the balance to enrich the message" arm
/// would turn this into a 500). No key is consumed, so the SAME key succeeds once
/// the balance covers it.
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

/// A debit against an EXISTING balance row that would go negative. Test 5
/// (`debit_beyond_balance_is_409_and_consumes_no_key`) debits a MISSING row, so its 409
/// comes from the fallback INSERT's tentative-row CHECK (787a95b's named fallback path);
/// this debit's UPDATE MATCHES the row directly, so its 409 comes from the CHECK on the
/// RESULTING row of `Store::apply_balance_tx`'s own UPDATE statement — the branch a revert
/// to the pre-787a95b single upsert would silently break while leaving test 5 green.
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
