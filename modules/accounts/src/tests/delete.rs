//! Account deletion (`begin_delete` / `delete_account`, Step 3 of the account-deletion
//! plan): the ticket upsert, the wrong-player/expired-ticket refusals, the permanent
//! handle reservation, the internal-edge replay, the one-transaction atomicity, and the
//! loud `migrate` guard against a pre-existing unclaimed handle. A child of `tests` so it
//! reuses that module's live-DB harness (`test_pool`/`wired`/`suffix`/`cleanup_player`).

use super::*;

/// Runs `f` when the guard drops — including through a panicking assertion, which a
/// trailing call never reaches. Mirrors `mail`'s `DbGuard`: `Drop` cannot await, so the
/// cleanup blocks the current (multi-thread) runtime; on any other flavour it degrades to
/// a warning rather than aborting the whole test binary during an unwind.
#[must_use = "the cleanup runs when this guard drops — binding it to `_` drops it at once"]
fn on_drop<F, Fut>(f: F) -> DbGuard
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    DbGuard(Some(Box::new(move || Box::pin(f()))))
}

struct DbGuard(Option<Box<dyn FnOnce() -> futures::future::BoxFuture<'static, ()> + Send>>);

impl Drop for DbGuard {
    fn drop(&mut self) {
        let Some(f) = self.0.take() else { return };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            eprintln!("WARN: accounts delete-fixture cleanup SKIPPED — no tokio runtime in scope");
            return;
        };
        if handle.runtime_flavor() != tokio::runtime::RuntimeFlavor::MultiThread {
            eprintln!(
                "WARN: accounts delete-fixture cleanup SKIPPED — a guard needs \
                 `flavor = \"multi_thread\"`"
            );
            return;
        }
        tokio::task::block_in_place(|| handle.block_on(f()));
    }
}

/// A `Service` over an UNREACHABLE lazy pool — proves a rejection happens before any store
/// contact, since a real connection attempt would hang/err rather than return `Invalid`.
fn unreachable_service() -> Arc<Service> {
    let pool = sqlx::PgPool::connect_lazy("postgres://nope:nope@127.0.0.1:1/nonexistent").unwrap();
    Arc::new(Service {
        store: Store { pool },
        bus: Arc::new(Bus::new()),
        dev_auth: true,
        providers: OnceLock::new(),
        argon_permits: Arc::new(Semaphore::new(2)),
        login_slots: Arc::new(Semaphore::new(32)),
        verifier: Arc::new(ArgonVerifier),
    })
}

async fn delete_ticket_row_count(pool: &PgPool, player_id: &str, ticket: &str) -> i64 {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM accounts.delete_tickets WHERE player_id = $1::uuid AND ticket = $2",
    )
    .bind(player_id)
    .bind(ticket)
    .fetch_one(pool)
    .await
    .unwrap();
    n
}

async fn player_row_count(pool: &PgPool, player_id: &str) -> i64 {
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM accounts.players WHERE id = $1::uuid")
        .bind(player_id)
        .fetch_one(pool)
        .await
        .unwrap();
    n
}

async fn session_row_count(pool: &PgPool, player_id: &str) -> i64 {
    let (n,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM accounts.sessions WHERE player_id = $1::uuid")
            .bind(player_id)
            .fetch_one(pool)
            .await
            .unwrap();
    n
}

async fn identity_row_count(pool: &PgPool, player_id: &str) -> i64 {
    let (n,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM accounts.identities WHERE player_id = $1::uuid")
            .bind(player_id)
            .fetch_one(pool)
            .await
            .unwrap();
    n
}

async fn deletion_receipt_row_count(pool: &PgPool, player_id: &str) -> i64 {
    let (n,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM accounts.deletion_receipts WHERE player_id = $1::uuid")
            .bind(player_id)
            .fetch_one(pool)
            .await
            .unwrap();
    n
}

/// The `(display_name_lower, discriminator)` pair a `"Name#1234"` handle names.
fn split_handle(handle: &str) -> (String, String) {
    let (name, disc) = handle.rsplit_once('#').expect("handle must contain '#'");
    (name.to_lowercase(), disc.to_string())
}

/// The `accounts.handles` row for `(name_lower, disc)`: `(exists, player_id_is_null)`.
async fn handle_claim_state(pool: &PgPool, name_lower: &str, disc: &str) -> Option<bool> {
    let row: Option<(Option<String>,)> = sqlx::query_as(
        "SELECT player_id::text FROM accounts.handles \
          WHERE display_name_lower = $1 AND discriminator = $2",
    )
    .bind(name_lower)
    .bind(disc)
    .fetch_optional(pool)
    .await
    .unwrap();
    row.map(|(pid,)| pid.is_none())
}

/// Forces a player's live delete ticket into the past — persisted-state timing, never a
/// real-clock race.
async fn expire_delete_ticket(pool: &PgPool, player_id: &str) {
    sqlx::query(
        "UPDATE accounts.delete_tickets SET expires_at = now() - interval '1 minute' \
          WHERE player_id = $1::uuid",
    )
    .bind(player_id)
    .execute(pool)
    .await
    .unwrap();
}

// ============================================================================
// Branch 1 — the ticket upsert: same ticket while live, a new one once expired.
// ============================================================================

/// `begin_delete` called twice returns the byte-identical ticket AND `expires_at`
/// (the upsert's whole safety property); once forced into the past, a third call mints a
/// DIFFERENT ticket. Pre-fix (a plain INSERT with no upsert) the second call would either
/// error on the primary key or silently mint a second live ticket the caller never saw.
#[tokio::test(flavor = "multi_thread")]
async fn begin_delete_upserts_same_ticket_until_expired() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;

    let email = format!("begindel-{}@test.local", suffix());
    let sess = svc.register(email, "pw".into(), "BeginDel".into()).await.unwrap();
    let pid = sess.player_id.clone();
    let _cleanup = on_drop({
        let pool = pool.clone();
        let pid = pid.clone();
        move || async move { cleanup_player(&pool, &pid).await }
    });

    let t1 = svc.begin_delete(Identity::player(&pid)).await.unwrap();
    let t2 = svc.begin_delete(Identity::player(&pid)).await.unwrap();
    assert_eq!(t1.ticket, t2.ticket, "a repeat begin_delete must return the SAME ticket");
    assert_eq!(
        t1.expires_at, t2.expires_at,
        "a repeat begin_delete must not extend the original expiry"
    );

    expire_delete_ticket(&pool, &pid).await;
    let t3 = svc.begin_delete(Identity::player(&pid)).await.unwrap();
    assert_ne!(
        t3.ticket, t1.ticket,
        "once the live ticket expired, begin_delete must mint a NEW one"
    );
}

// ============================================================================
// Branch 2 — a ticket presented by someone else is NotFound and NOT consumed.
// ============================================================================

/// A ticket minted for player A, presented by player B, is `NotFound` — and critically,
/// A's ticket row survives untouched. A test asserting only the 404 would pass even if the
/// DELETE half of `consume_delete_ticket_tx`'s WHERE clause dropped the `player_id`
/// predicate and consumed (and lost) the wrong player's ticket.
#[tokio::test(flavor = "multi_thread")]
async fn delete_account_wrong_player_ticket_is_not_found_and_not_consumed() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;

    let a = svc
        .register(format!("victimA-{}@test.local", suffix()), "pw".into(), "A".into())
        .await
        .unwrap();
    let b = svc
        .register(format!("attackerB-{}@test.local", suffix()), "pw".into(), "B".into())
        .await
        .unwrap();
    let a_pid = a.player_id.clone();
    let b_pid = b.player_id.clone();
    let _cleanup = on_drop({
        let pool = pool.clone();
        move || async move {
            cleanup_player(&pool, &a_pid).await;
            cleanup_player(&pool, &b_pid).await;
        }
    });

    let ticket = svc.begin_delete(Identity::player(&a.player_id)).await.unwrap().ticket;

    let e = svc
        .delete_account(Identity::player(&b.player_id), ticket.clone())
        .await
        .unwrap_err();
    assert_eq!(e.status, opsapi::Status::NotFound, "another player's ticket must be NotFound");

    assert_eq!(
        delete_ticket_row_count(&pool, &a.player_id, &ticket).await,
        1,
        "A's ticket row must survive a rejected cross-player attempt"
    );
    assert_eq!(player_row_count(&pool, &a.player_id).await, 1, "A must still exist");
    assert_eq!(player_row_count(&pool, &b.player_id).await, 1, "B must still exist");
}

// ============================================================================
// Branch 3 — an expired ticket is NotFound and the player is untouched.
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn delete_account_expired_ticket_is_not_found_player_survives() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;

    let sess = svc
        .register(format!("expdel-{}@test.local", suffix()), "pw".into(), "ExpDel".into())
        .await
        .unwrap();
    let pid = sess.player_id.clone();
    let _cleanup = on_drop({
        let pool = pool.clone();
        let pid = pid.clone();
        move || async move { cleanup_player(&pool, &pid).await }
    });

    let ticket = svc.begin_delete(Identity::player(&pid)).await.unwrap().ticket;
    expire_delete_ticket(&pool, &pid).await;

    let e = svc
        .delete_account(Identity::player(&pid), ticket)
        .await
        .unwrap_err();
    assert_eq!(e.status, opsapi::Status::NotFound, "an expired ticket must be NotFound");
    assert_eq!(player_row_count(&pool, &pid).await, 1, "an expired-ticket attempt must not delete the player");
}

// ============================================================================
// Branch 4 — the handle reservation permanently blocks a re-mint.
// ============================================================================

/// After a successful deletion, `accounts.handles` still carries the pair with
/// `player_id IS NULL` — never removed — and the same INSERT ... ON CONFLICT DO NOTHING
/// [`Store::claim_handle_tx`] uses returns nothing for it (that store method is
/// module-private to `store.rs` with no test accessor, per this plan's guardrail against
/// adding a second one; the identical SQL against the row `reserve_handle_tx` — itself
/// exercised here — actually produced is the closest proof available without widening
/// production visibility).
#[tokio::test(flavor = "multi_thread")]
async fn delete_account_reserves_handle_permanently_blocking_remint() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;

    let sess = svc
        .register(format!("handledel-{}@test.local", suffix()), "pw".into(), "HandleDel".into())
        .await
        .unwrap();
    let pid = sess.player_id.clone();
    let me = svc.me(Identity::player(&pid)).await.unwrap();
    let (name_lower, disc) = split_handle(&me.handle);

    let ticket = svc.begin_delete(Identity::player(&pid)).await.unwrap().ticket;
    let receipt = svc.delete_account(Identity::player(&pid), ticket).await.unwrap();
    assert_eq!(receipt.handle, me.handle);

    let claim = handle_claim_state(&pool, &name_lower, &disc).await;
    assert_eq!(
        claim,
        Some(true),
        "the handle's accounts.handles row must survive the deletion with player_id NULL"
    );

    // The exact conflict-target SQL `claim_handle_tx` runs, against the SAME row: a
    // re-mint of this pair must find the row already claimed and return nothing.
    let reminted: Option<(String,)> = sqlx::query_as(
        "INSERT INTO accounts.handles (display_name_lower, discriminator, player_id) \
         VALUES ($1, $2, gen_random_uuid()) \
         ON CONFLICT (display_name_lower, discriminator) DO NOTHING \
         RETURNING player_id::text",
    )
    .bind(&name_lower)
    .bind(&disc)
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert!(
        reminted.is_none(),
        "a deleted player's exact handle pair must never be re-mintable"
    );
}

// ============================================================================
// Branch 5 — internal-edge replay: byte-identical receipt, sessions/identities gone.
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn delete_account_replay_returns_identical_receipt_and_wipes_sessions() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;

    let sess = svc
        .register(format!("replay-{}@test.local", suffix()), "pw".into(), "Replay".into())
        .await
        .unwrap();
    let pid = sess.player_id.clone();

    let ticket = svc.begin_delete(Identity::player(&pid)).await.unwrap().ticket;
    let receipt1 = svc
        .delete_account(Identity::player(&pid), ticket.clone())
        .await
        .unwrap();

    assert_eq!(session_row_count(&pool, &pid).await, 0, "sessions must be gone after the first delete");
    assert_eq!(identity_row_count(&pool, &pid).await, 0, "identities must be gone after the first delete");

    // The internal-edge replay: same identity/ticket, reaching the handler again after the
    // player row (and its ticket, via cascade) are already gone.
    let receipt2 = svc
        .delete_account(Identity::player(&pid), ticket)
        .await
        .unwrap();
    assert_eq!(receipt1, receipt2, "a replay must answer the byte-identical receipt");
}

// ============================================================================
// Branch 6 — one transaction: exactly one `player.deleted`, sessions gone at the SAME
// commit; a forced failure after the receipt insert leaves no reservation/receipt and an
// intact player.
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn delete_account_commits_one_event_with_sessions_emptied() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;

    let sess = svc
        .register(format!("onetx-{}@test.local", suffix()), "pw".into(), "OneTx".into())
        .await
        .unwrap();
    let pid = sess.player_id.clone();

    let ticket = svc.begin_delete(Identity::player(&pid)).await.unwrap().ticket;
    svc.delete_account(Identity::player(&pid), ticket).await.unwrap();

    let deleted_events =
        asyncevents::testing::events_count(&pool, "player.deleted", "player_id", &pid)
            .await
            .unwrap();
    assert_eq!(deleted_events, 1, "exactly one player.deleted must be appended");
    assert_eq!(session_row_count(&pool, &pid).await, 0, "sessions must be empty at the same commit");

    let _ = asyncevents::testing::cleanup_events(&pool, "player_id", &pid).await;
}

/// A forced failure AFTER the receipt insert (the durable `emit_tx`, the last write of
/// `delete_player_steps`) must roll back EVERYTHING that transaction did: no handle
/// reservation, no receipt row, no player deletion, and — since the ticket consume shares
/// the same transaction — the original ticket survives too. Proven by construction: a
/// second `Service` sharing the pool but wired to an always-failing transport
/// ([`wired_with_failing_bus`]), so the failure lands exactly at `emit_tx` and nowhere else.
#[tokio::test(flavor = "multi_thread")]
async fn delete_player_atomic_failure_after_receipt_insert_leaves_no_trace() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;

    let sess = svc
        .register(format!("txfail-{}@test.local", suffix()), "pw".into(), "TxFail".into())
        .await
        .unwrap();
    let pid = sess.player_id.clone();
    let _cleanup = on_drop({
        let pool = pool.clone();
        let pid = pid.clone();
        move || async move { cleanup_player(&pool, &pid).await }
    });

    let me = svc.me(Identity::player(&pid)).await.unwrap();
    let (name_lower, disc) = split_handle(&me.handle);
    let ticket = svc.begin_delete(Identity::player(&pid)).await.unwrap().ticket;

    let failing_svc = wired_with_failing_bus(&pool).await;
    let e = failing_svc
        .delete_account(Identity::player(&pid), ticket.clone())
        .await
        .unwrap_err();
    assert_eq!(e.status, opsapi::Status::Internal);

    assert_eq!(player_row_count(&pool, &pid).await, 1, "the player must remain intact");
    assert_eq!(deletion_receipt_row_count(&pool, &pid).await, 0, "no receipt may survive the rollback");
    assert_eq!(
        delete_ticket_row_count(&pool, &pid, &ticket).await,
        1,
        "the consumed ticket must be restored by the rollback"
    );
    // `register` already claimed this pair FOR the live player (`player_id` = pid); the
    // failed delete must not have advanced it to the permanent NULL reservation.
    let claim = handle_claim_state(&pool, &name_lower, &disc).await;
    assert_eq!(
        claim,
        Some(false),
        "the handle reservation must not have been planted — the claim must still point at \
         the (still-live) player, not NULL"
    );
}

// ============================================================================
// Branch 7 — `migrate` fails loudly on a pre-existing unclaimed handle, rather than
// surfacing as a 500 later from `register`/`login`.
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn migrate_fails_loudly_on_a_player_with_no_handles_claim() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let ctx = Context::with_db(pool.clone());

    // A player row inserted directly, bypassing the handles claim `register` always takes
    // atomically — the shape a pre-`accounts.handles` schema would have left behind.
    let (ghost_id,): (String,) = sqlx::query_as(
        "INSERT INTO accounts.players (display_name, discriminator) \
         VALUES ($1, '9999') RETURNING id::text",
    )
    .bind(format!("Ghost{}", suffix()))
    .fetch_one(&pool)
    .await
    .unwrap();
    let _cleanup = on_drop({
        let pool = pool.clone();
        let ghost_id = ghost_id.clone();
        move || async move {
            let _ = sqlx::query("DELETE FROM accounts.players WHERE id = $1::uuid")
                .bind(&ghost_id)
                .execute(&pool)
                .await;
        }
    });

    let err = Accounts::new()
        .migrate(&ctx)
        .await
        .expect_err("a player with no accounts.handles claim must fail migrate loudly");
    assert!(
        err.to_string().contains("accounts.handles claim"),
        "unexpected migrate error: {err}"
    );
}

// ============================================================================
// Branch 8 — the cap gate rejects before any store contact.
// ============================================================================

#[tokio::test]
async fn delete_ticket_within_cap_rejects_over_cap_before_store_contact() {
    let svc = unreachable_service();
    let over_cap = "x".repeat(MAX_DELETE_TICKET_BYTES + 1);
    let e = svc
        .delete_account(Identity::player("11111111-1111-1111-1111-111111111111"), over_cap)
        .await
        .unwrap_err();
    assert_eq!(
        e.status,
        opsapi::Status::Invalid,
        "an over-cap ticket must be rejected without ever reaching the (unreachable) store"
    );
}
