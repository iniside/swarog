//! Browser sessions of the hosted login pages against the live database. A child of
//! `tests` so it reuses that module's live-DB harness.

use super::*;

async fn browser_rows(pool: &PgPool, player_id: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM accounts.browser_sessions WHERE player_id = $1::uuid")
        .bind(player_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_browser_token_resolves_its_player_but_never_authenticates_an_op() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("browser-{}@test.local", suffix());
    let pid = svc.register(email, "pw".into(), "Browser".into(), test_client()).await.unwrap().player_id;
    let minted = svc.store.create_browser_session(&pid).await.unwrap().unwrap();
    let read = svc.store.browser_session(&minted.token).await.unwrap();
    let as_bearer = svc.store.player_by_session(&minted.token).await.unwrap();
    let stored_raw: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM accounts.browser_sessions WHERE token_digest = convert_to($1, 'UTF8')",
    )
    .bind(&minted.token)
    .fetch_one(&pool)
    .await
    .unwrap();
    let ended = svc.store.delete_browser_session(&minted.token).await.unwrap();
    let after_logout = svc.store.browser_session(&minted.token).await.unwrap();
    cleanup_player(&pool, &pid).await;

    let read = read.expect("a fresh browser session resolves");
    assert_eq!(read.player_id, pid);
    assert_eq!(read.csrf_token, minted.csrf_token);
    assert_ne!(minted.csrf_token, minted.token);
    assert!(as_bearer.is_none(), "a browser token is not a bearer session");
    assert_eq!(stored_raw, 0, "only the digest is stored, never the token");
    assert!(ended, "logout deletes the session");
    assert!(after_logout.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_or_over_cap_token_is_absent() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let over_cap = "a".repeat(accountsapi::MAX_SESSION_TOKEN_BYTES + 1);
    assert!(svc.store.browser_session(&store::new_token()).await.unwrap().is_none());
    assert!(svc.store.browser_session(&over_cap).await.unwrap().is_none());
    assert!(!svc.store.delete_browser_session(&over_cap).await.unwrap());
}

/// Killing a player's families (the deletion path's session revocation) ends its browser
/// sessions too, in the same transaction, and keeps another player's.
#[tokio::test(flavor = "multi_thread")]
async fn killing_a_players_families_ends_its_browser_sessions() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let pid = svc
        .register(format!("browser-kill-{}@test.local", suffix()), "pw".into(), "BrKill".into(), test_client())
        .await
        .unwrap()
        .player_id;
    let other = svc
        .register(format!("browser-keep-{}@test.local", suffix()), "pw".into(), "BrKeep".into(), test_client())
        .await
        .unwrap()
        .player_id;
    let minted = svc.store.create_browser_session(&pid).await.unwrap().unwrap();
    svc.store.create_browser_session(&other).await.unwrap().unwrap();
    let mut tx = pool.begin().await.unwrap();
    let killed = svc.store.kill_player_families_tx(&mut tx, &pid).await.unwrap();
    tx.commit().await.unwrap();
    let left = browser_rows(&pool, &pid).await;
    let kept = browser_rows(&pool, &other).await;
    let read = svc.store.browser_session(&minted.token).await.unwrap();
    cleanup_player(&pool, &pid).await;
    cleanup_player(&pool, &other).await;

    assert_eq!(killed, Some(3), "token, access session and browser session");
    assert_eq!(left, 0);
    assert!(read.is_none());
    assert_eq!(kept, 1, "another player's browser session is untouched");
}

/// Deleting the player removes its browser sessions, and a session minted for a player
/// that no longer exists is refused instead of left behind.
#[tokio::test(flavor = "multi_thread")]
async fn deleting_the_player_removes_its_browser_sessions_and_refuses_new_ones() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let pid = svc
        .register(format!("browser-del-{}@test.local", suffix()), "pw".into(), "BrDel".into(), test_client())
        .await
        .unwrap()
        .player_id;
    svc.store.create_browser_session(&pid).await.unwrap().unwrap();
    let mut tx = pool.begin().await.unwrap();
    let deleted = svc.store.delete_player_tx(&mut tx, &pid).await.unwrap();
    tx.commit().await.unwrap();
    let left = browser_rows(&pool, &pid).await;
    let late = svc.store.create_browser_session(&pid).await.unwrap();
    let malformed = svc.store.create_browser_session("not-a-uuid").await.unwrap();
    cleanup_player(&pool, &pid).await;

    assert!(deleted);
    assert_eq!(left, 0, "the player's browser sessions are gone with it");
    assert!(late.is_none(), "no session for a deleted player");
    assert!(malformed.is_none(), "an unparseable id is no player");
}

/// The schema's own cascade, without the revocation `delete_player_tx` runs first.
#[tokio::test(flavor = "multi_thread")]
async fn a_raw_player_delete_cascades_to_its_browser_sessions() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let pid = svc
        .register(format!("browser-cascade-{}@test.local", suffix()), "pw".into(), "BrCasc".into(), test_client())
        .await
        .unwrap()
        .player_id;
    svc.store.create_browser_session(&pid).await.unwrap().unwrap();
    sqlx::query("DELETE FROM accounts.players WHERE id = $1::uuid")
        .bind(&pid)
        .execute(&pool)
        .await
        .unwrap();
    let left = browser_rows(&pool, &pid).await;
    cleanup_player(&pool, &pid).await;

    assert_eq!(left, 0);
}

/// A browser session insert racing the player's deletion: the insert's foreign key check
/// waits on the deleting transaction's row lock, then fails, so no session outlives the
/// player.
#[tokio::test(flavor = "multi_thread")]
async fn a_browser_session_racing_deletion_is_refused() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let pid = svc
        .register(format!("browser-race-{}@test.local", suffix()), "pw".into(), "BrRace".into(), test_client())
        .await
        .unwrap()
        .player_id;
    let mut gate = pool.begin().await.unwrap();
    let gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *gate)
        .await
        .unwrap();
    assert!(svc.store.delete_player_tx(&mut gate, &pid).await.unwrap());
    let task = {
        let svc = Arc::clone(&svc);
        let pid = pid.clone();
        tokio::spawn(async move { svc.store.create_browser_session(&pid).await })
    };
    let mut waited = false;
    for _ in 0..200 {
        let blocked: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid)))",
        )
        .bind(gate_pid)
        .fetch_one(&pool)
        .await
        .unwrap();
        if blocked {
            waited = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    gate.commit().await.unwrap();
    let minted = task.await.unwrap().unwrap();
    let left = browser_rows(&pool, &pid).await;
    cleanup_player(&pool, &pid).await;

    assert!(waited, "the insert must wait on the deleting transaction");
    assert!(minted.is_none(), "the racing insert is refused");
    assert_eq!(left, 0);
}
