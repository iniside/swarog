//! Step 1 (`922cc63`) + its follow-up (`a84538d`): handle minting, the `Directory`
//! capability, and `MeView.handle`. A child of `tests` so it reuses that module's
//! live-DB harness (`test_pool`/`wired`/`suffix`/`cleanup_player`).

use super::*;
use accountsapi::Directory as _;
use rand::RngCore as _;

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

    let me_a = svc.me(Identity::player(&a.player_id)).await.unwrap();
    let me_b = svc.me(Identity::player(&b.player_id)).await.unwrap();
    assert_ne!(me_a.handle, me_b.handle, "same display name must mint distinct handles");
    assert!(me_a.handle.starts_with(&format!("{name}#")));
    assert!(me_b.handle.starts_with(&format!("{name}#")));

    let found_a = svc.find_by_handle(me_a.handle.clone()).await.unwrap().unwrap();
    let found_b = svc.find_by_handle(me_b.handle.clone()).await.unwrap().unwrap();
    assert_eq!(found_a.player_id, a.player_id);
    assert_eq!(found_b.player_id, b.player_id);

    cleanup_player(&pool, &a.player_id).await;
    cleanup_player(&pool, &b.player_id).await;
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

    let got = svc
        .players_by_id(vec![p.player_id.clone(), unknown])
        .await
        .unwrap();
    assert_eq!(got.len(), 1, "the unknown id must be omitted, not erroring the batch");
    assert_eq!(got[0].player_id, p.player_id);

    cleanup_player(&pool, &p.player_id).await;
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

    let got = svc
        .players_by_id(vec![p.player_id.clone(), "not-a-uuid-at-all".into()])
        .await
        .expect("a malformed id must not error the whole batch");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].player_id, p.player_id);

    cleanup_player(&pool, &p.player_id).await;
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

/// `MAX_DISPLAY_NAME_BYTES` is enforced on the name input path (item 6). Ported from
/// the existing cap test's over-cap fixture (Step 1's `register_rejects_effective_
/// display_cap_before_argon_or_db`), pinned instead against the shared authority the
/// `Directory`/handle work now reads.
#[tokio::test]
async fn register_rejects_display_name_over_the_shared_cap() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let over_cap = format!("{}a", "n".repeat(accountsapi::MAX_DISPLAY_NAME_BYTES));
    assert_eq!(over_cap.len(), accountsapi::MAX_DISPLAY_NAME_BYTES + 1);
    let e = svc
        .register(format!("cap-{}@test.local", suffix()), "pw".into(), over_cap)
        .await
        .unwrap_err();
    assert_eq!(e.status, opsapi::Status::Invalid);
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

    // p.token was minted by register() itself and is still live.
    let live = svc.players_by_id(vec![p.player_id.clone()]).await.unwrap();
    assert_eq!(live.len(), 1);
    assert!(!live[0].online_until.is_empty(), "a freshly minted session must be live");

    // Delete the live session and insert one explicitly expired.
    sqlx::query("DELETE FROM accounts.sessions WHERE player_id = $1::uuid")
        .bind(&p.player_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO accounts.sessions (token, player_id, family_id, expires_at) \
         VALUES ($1, $2::uuid, $3::uuid, now() - interval '1 hour')",
    )
    .bind(store::new_token())
    .bind(&p.player_id)
    .bind(random_uuid())
    .execute(&pool)
    .await
    .unwrap();

    let expired = svc.players_by_id(vec![p.player_id.clone()]).await.unwrap();
    assert_eq!(expired.len(), 1, "the player row itself must still be returned");
    assert!(
        expired[0].online_until.is_empty(),
        "an expired-only session must not count as online"
    );

    cleanup_player(&pool, &p.player_id).await;
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

    let me = svc.me(Identity::player(&p.player_id)).await.unwrap();
    assert!(me.handle.starts_with("Rounder#"));

    let found = svc.find_by_handle(me.handle.clone()).await.unwrap();
    assert_eq!(found.map(|f| f.player_id), Some(p.player_id.clone()));

    cleanup_player(&pool, &p.player_id).await;
}

/// `player_summary` — the branch the `a84538d` follow-up introduced — must succeed for
/// a player with NO session row at all. `me`'s own callers can never reach this (a
/// caller of `me` always holds the live session it authenticated with), so nothing
/// else in the suite exercises it. If `Store::player_summary`'s `LEFT JOIN` were ever
/// turned into an inner join, the player row would vanish and this assertion's `Some`
/// would fail — only this test would catch it (item 9).
#[tokio::test]
async fn player_summary_succeeds_with_no_session_row() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let p = svc
        .register(format!("nosess-{}@test.local", suffix()), "pw".into(), "Sessionless".into())
        .await
        .unwrap();
    // register() mints a live session as a side effect; delete it so the player has
    // NO row in accounts.sessions at all — the branch `me`'s callers never reach.
    sqlx::query("DELETE FROM accounts.sessions WHERE player_id = $1::uuid")
        .bind(&p.player_id)
        .execute(&pool)
        .await
        .unwrap();

    let summary = svc.store.player_summary(&p.player_id).await.unwrap();
    assert!(summary.is_some(), "an inner join would drop the sessionless player entirely");
    let summary = summary.unwrap();
    assert_eq!(summary.player_id, p.player_id);
    assert!(summary.online_until.is_empty());
    assert!(summary.handle.starts_with("Sessionless#"));

    cleanup_player(&pool, &p.player_id).await;
}
