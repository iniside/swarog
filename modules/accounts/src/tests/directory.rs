//! Step 1 (`922cc63`) + its follow-up (`a84538d`): handle minting, the `Directory`
//! capability, and `MeView.handle`. A child of `tests` so it reuses that module's
//! live-DB harness (`test_pool`/`wired`/`suffix`/`cleanup_player`).

use super::*;
use accountsapi::Directory as _;
use rand::RngCore as _;
use std::future::Future;

/// A random canonical-uuid-shaped string that names no player — this crate has no
/// `uuid` dependency, so the shape is hand-rolled to match `is_canonical_uuid`.
fn random_uuid() -> String {
    let mut b = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut b);
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

/// Runs `body` on its own task and deletes every listed player (and its orphaned
/// `player.registered` durable event row, via `cleanup_player`) even when `body`
/// panicked, then re-raises the panic. A `Drop` guard cannot `await`, so it cannot
/// reach the shared `asyncevents` log's cleanup — this must join first and clean up
/// after, mirroring `leaderboard`'s `with_cleanup`.
async fn with_cleanup<Fut>(pool: &PgPool, player_ids: Vec<String>, body: Fut)
where
    Fut: Future<Output = ()> + Send + 'static,
{
    let outcome = tokio::spawn(body).await;
    for id in &player_ids {
        cleanup_player(pool, id).await;
    }
    if let Err(e) = outcome {
        if e.is_panic() {
            std::panic::resume_unwind(e.into_panic());
        }
        panic!("accounts directory test task ended without completing: {e}");
    }
}

/// Two registrations with the SAME `display_name` must mint distinct discriminators,
/// and each handle must resolve, via `find_by_handle`, back to its OWN `player_id` —
/// the whole point of the discriminator (item 1).
#[tokio::test]
async fn same_display_name_mints_distinct_handles_that_resolve_back() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let name = format!("Dup-{}", suffix());

    let a = svc
        .register(format!("a-{}@test.local", suffix()), "pw".into(), name.clone())
        .await
        .unwrap();
    let b = svc
        .register(format!("b-{}@test.local", suffix()), "pw".into(), name.clone())
        .await
        .unwrap();

    let (a_id, b_id) = (a.player_id.clone(), b.player_id.clone());
    with_cleanup(&pool, vec![a.player_id.clone(), b.player_id.clone()], async move {
        let me_a = svc.me(Identity::player(&a_id)).await.unwrap();
        let me_b = svc.me(Identity::player(&b_id)).await.unwrap();
        assert_ne!(me_a.handle, me_b.handle, "same display name must mint distinct handles");
        assert!(me_a.handle.starts_with(&format!("{name}#")));
        assert!(me_b.handle.starts_with(&format!("{name}#")));

        let found_a = svc.find_by_handle(me_a.handle.clone()).await.unwrap().unwrap();
        let found_b = svc.find_by_handle(me_b.handle.clone()).await.unwrap().unwrap();
        assert_eq!(found_a.player_id, a_id);
        assert_eq!(found_b.player_id, b_id);
    })
    .await;
}

/// A miss is `Ok(None)`, never an error (item 2).
#[tokio::test]
async fn find_by_handle_miss_is_ok_none() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let missing = format!("NoSuchPlayer-{}#0001", suffix());
    assert_eq!(svc.find_by_handle(missing).await.unwrap(), None);
}

/// `players_by_id` OMITS ids that name no player: a mixed known+unknown batch comes
/// back SHORT, never an error (item 3).
#[tokio::test]
async fn players_by_id_omits_unknown_ids() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let p = svc
        .register(format!("known-{}@test.local", suffix()), "pw".into(), "Known".into())
        .await
        .unwrap();
    let unknown = random_uuid();

    let player_id = p.player_id.clone();
    with_cleanup(&pool, vec![p.player_id.clone()], async move {
        let got = svc
            .players_by_id(vec![player_id.clone(), unknown])
            .await
            .unwrap();
        assert_eq!(got.len(), 1, "the unknown id must be omitted, not erroring the batch");
        assert_eq!(got[0].player_id, player_id);
    })
    .await;
}

/// The `is_canonical_uuid` prefilter's whole reason to exist: before it, ONE malformed
/// element in the `= ANY($1::text[]::uuid[])` bind raised 22P02 and aborted the WHOLE
/// statement, so a caller with one stale/garbage id got zero results back, including
/// for ids that were perfectly valid. This test goes red on that regression because it
/// asserts the valid id's summary is present in the same batch as the malformed one —
/// a whole-batch error (not an empty/short list) would fail the `.unwrap()` on the
/// call itself (item 4, the previously-wrong branch).
#[tokio::test]
async fn malformed_id_in_batch_does_not_poison_the_valid_ones() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let p = svc
        .register(format!("valid-{}@test.local", suffix()), "pw".into(), "Valid".into())
        .await
        .unwrap();

    let player_id = p.player_id.clone();
    with_cleanup(&pool, vec![p.player_id.clone()], async move {
        let got = svc
            .players_by_id(vec![player_id.clone(), "not-a-uuid-at-all".into()])
            .await
            .expect("a malformed id must not error the whole batch");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].player_id, player_id);
    })
    .await;
}

/// `MAX_LOOKUP_IDS` (256) is enforced, not merely declared: 257 ids is refused (item 5).
#[tokio::test]
async fn players_by_id_over_max_lookup_ids_is_invalid() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let ids: Vec<String> = (0..accountsapi::MAX_LOOKUP_IDS + 1)
        .map(|_| random_uuid())
        .collect();
    let e = svc.players_by_id(ids).await.unwrap_err();
    assert_eq!(e.status, opsapi::Status::Invalid);

    let ok_ids: Vec<String> = (0..accountsapi::MAX_LOOKUP_IDS)
        .map(|_| random_uuid())
        .collect();
    assert!(svc.players_by_id(ok_ids).await.is_ok(), "exactly the cap must still be accepted");
}

/// `MAX_HANDLE_BYTES` is bracketed the same way the lookup cap is: exactly the cap is
/// still a genuine lookup (`Ok(None)`, since no such player exists), one byte over is
/// rejected (item 6, `handle_within_cap`'s previously-unexecuted guard).
#[tokio::test]
async fn find_by_handle_is_bracketed_at_max_handle_bytes() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;

    let at_cap = format!("{}#0001", "n".repeat(accountsapi::MAX_HANDLE_BYTES - 5));
    assert_eq!(at_cap.len(), accountsapi::MAX_HANDLE_BYTES);
    assert_eq!(
        svc.find_by_handle(at_cap).await.unwrap(),
        None,
        "exactly the cap must still reach the store as a genuine (missing) lookup"
    );

    let over_cap = format!("{}#0001", "n".repeat(accountsapi::MAX_HANDLE_BYTES - 4));
    assert_eq!(over_cap.len(), accountsapi::MAX_HANDLE_BYTES + 1);
    let e = svc.find_by_handle(over_cap).await.unwrap_err();
    assert_eq!(e.status, opsapi::Status::Invalid);
}

/// `MAX_DISPLAY_NAME_BYTES` is bracketed on BOTH sides with a byte-counting fixture
/// (item 6/C): exactly the cap registers successfully (mutating `<=` to `<` reds this
/// half), one byte over is `Invalid` using a multibyte `"é"` fixture (mutating
/// `display_name_within_cap` from `.len()` to `.chars().count()` reds this half — an
/// ascii-only fixture could not tell the two apart). Distinct from the pre-existing
/// `register_rejects_effective_display_cap_before_argon_or_db` (`tests.rs`), which
/// proves the reject precedes Argon/DB but never exercises the accept side.
#[tokio::test]
async fn register_display_name_is_bracketed_at_the_shared_byte_cap() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;

    let at_cap = "é".repeat(accountsapi::MAX_DISPLAY_NAME_BYTES / 2);
    assert_eq!(at_cap.len(), accountsapi::MAX_DISPLAY_NAME_BYTES);
    let ok = svc
        .register(format!("cap-ok-{}@test.local", suffix()), "pw".into(), at_cap)
        .await
        .unwrap();

    with_cleanup(&pool, vec![ok.player_id.clone()], async move {
        let over_cap = format!("{}a", "é".repeat(accountsapi::MAX_DISPLAY_NAME_BYTES / 2));
        assert_eq!(over_cap.len(), accountsapi::MAX_DISPLAY_NAME_BYTES + 1);
        let e = svc
            .register(format!("cap-over-{}@test.local", suffix()), "pw".into(), over_cap)
            .await
            .unwrap_err();
        assert_eq!(e.status, opsapi::Status::Invalid);
    })
    .await;
}

/// `split_handle`'s rejection guard (item D) — proven by construction against a DEAD
/// store rather than by absence-of-match: a malformed shape must answer `Ok(None)`
/// WITHOUT ever reaching the pool, while a well-formed handle DOES reach it and
/// surfaces the pool's own error. If `split_handle` were loosened to accept a
/// no-`#` string or a non-4-digit tail, that shape would fall through to the store
/// call and this test would observe an `Err` where it asserts `Ok(None)`.
#[tokio::test]
async fn find_by_handle_rejects_malformed_shapes_before_touching_the_store() {
    let svc = dead_directory_service();

    for malformed in ["NoHashAtAll", "Bob#abc", "Bob#123", "Bob#12345", "#0001", ""] {
        assert_eq!(
            svc.find_by_handle(malformed.into()).await.unwrap(),
            None,
            "a malformed handle must short-circuit before any store call: {malformed:?}"
        );
    }

    let e = svc.find_by_handle("Bob#0001".into()).await.unwrap_err();
    assert_eq!(
        e.status,
        opsapi::Status::Internal,
        "a well-formed handle must actually reach the dead store, not short-circuit"
    );
}

/// A `Service` over an unroutable pool — any real query surfaces as `Err` fast,
/// nothing hangs, and nothing here needs a live Postgres.
fn dead_directory_service() -> Arc<Service> {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(200))
        .connect_lazy("postgres://gamebackend:gamebackend@127.0.0.1:1/accounts-directory-dead")
        .unwrap();
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

/// `online_until` is populated for a live session and EMPTY for an expired one. The
/// expired row is inserted explicitly with `expires_at` in the past — never a sleep
/// against a real clock (item 7, the timing-sensitive-tests doctrine). This test goes
/// red on a regression that drops the `s.expires_at > now()` filter (or otherwise
/// counts an expired session as live): the "expired" assertion would then observe a
/// non-empty `online_until`.
#[tokio::test]
async fn online_until_reflects_live_session_and_is_empty_when_expired() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let p = svc
        .register(format!("sess-{}@test.local", suffix()), "pw".into(), "Sessioned".into())
        .await
        .unwrap();

    let (db, player_id) = (pool.clone(), p.player_id.clone());
    with_cleanup(&pool, vec![p.player_id.clone()], async move {
        // p's token was minted by register() itself and is still live.
        let live = svc.players_by_id(vec![player_id.clone()]).await.unwrap();
        assert_eq!(live.len(), 1);
        assert!(!live[0].online_until.is_empty(), "a freshly minted session must be live");

        // Delete the live session and insert one explicitly expired.
        sqlx::query("DELETE FROM accounts.sessions WHERE player_id = $1::uuid")
            .bind(&player_id)
            .execute(&db)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO accounts.sessions (token, player_id, family_id, expires_at) \
             VALUES ($1, $2::uuid, $3::uuid, now() - interval '1 hour')",
        )
        .bind(store::new_token())
        .bind(&player_id)
        .bind(random_uuid())
        .execute(&db)
        .await
        .unwrap();

        let expired = svc.players_by_id(vec![player_id.clone()]).await.unwrap();
        assert_eq!(expired.len(), 1, "the player row itself must still be returned");
        assert!(
            expired[0].online_until.is_empty(),
            "an expired-only session must not count as online"
        );
    })
    .await;
}

/// `GET /accounts/me` (`Service::me`) returns exactly the `"Name#1234"` that
/// `find_by_handle` resolves back to the SAME `player_id` — one assertion pinning both
/// halves of the single render/parse authority (`summary_of` renders, `split_handle`
/// parses) (item 8).
#[tokio::test]
async fn me_handle_round_trips_through_find_by_handle() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let p = svc
        .register(format!("me-{}@test.local", suffix()), "pw".into(), "Rounder".into())
        .await
        .unwrap();

    let player_id = p.player_id.clone();
    with_cleanup(&pool, vec![p.player_id.clone()], async move {
        let me = svc.me(Identity::player(&player_id)).await.unwrap();
        assert!(me.handle.starts_with("Rounder#"));

        let found = svc.find_by_handle(me.handle.clone()).await.unwrap();
        assert_eq!(found.map(|f| f.player_id), Some(player_id));
    })
    .await;
}

/// `player_summary` — the branch the `a84538d` follow-up introduced — succeeds for a
/// player with NO session row at all (item 9). `me`'s own callers can never take this
/// exact path (a caller of `me` always holds a session that is itself live and would
/// satisfy `LEFT JOIN`'s predicate), so nothing else in this file constructs a player
/// with zero session rows before reading its summary. Note the sibling
/// `online_until_reflects_live_session_and_is_empty_when_expired` test also reaches an
/// `INNER JOIN` mutation first (its expired-session row fails the join predicate too,
/// dropping the player row) — this test's distinct value is the zero-rows case, not
/// exclusivity over the mutation class.
#[tokio::test]
async fn player_summary_succeeds_with_no_session_row() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let p = svc
        .register(format!("nosess-{}@test.local", suffix()), "pw".into(), "Sessionless".into())
        .await
        .unwrap();

    let (db, player_id) = (pool.clone(), p.player_id.clone());
    with_cleanup(&pool, vec![p.player_id.clone()], async move {
        // register() mints a live session as a side effect; delete it so the player has
        // NO row in accounts.sessions at all — the branch `me`'s callers never reach.
        sqlx::query("DELETE FROM accounts.sessions WHERE player_id = $1::uuid")
            .bind(&player_id)
            .execute(&db)
            .await
            .unwrap();

        let summary = svc.store.player_summary(&player_id).await.unwrap();
        assert!(summary.is_some(), "an inner join would drop the sessionless player entirely");
        let summary = summary.unwrap();
        assert_eq!(summary.player_id, player_id);
        assert!(summary.online_until.is_empty());
        assert!(summary.handle.starts_with("Sessionless#"));
    })
    .await;
}
