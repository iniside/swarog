//! One password account per email spelling: `register` and `login` both pass the email
//! through `normalize_email` before the cap, the lookup and the insert.

use super::*;

#[test]
fn normalize_email_trims_and_lowercases_ascii_only() {
    assert_eq!(normalize_email("  A@X.io\t"), "a@x.io");
    assert_eq!(normalize_email("Ä@x.io"), "Ä@x.io", "non-ASCII letters keep their case");
}

#[tokio::test]
async fn mixed_case_register_then_lower_case_login_and_the_reverse() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let tag = suffix().to_ascii_lowercase();

    let upper = svc.register(format!("A-{tag}@X.io"), "pw".into(), "Up".into()).await.unwrap();
    let upper_login = svc.login(format!("a-{tag}@x.io"), "pw".into()).await;

    let lower = svc.register(format!("b-{tag}@x.io"), "pw".into(), "Low".into()).await.unwrap();
    let lower_login = svc.login(format!(" B-{tag}@X.IO "), "pw".into()).await;
    cleanup_player(&pool, &upper.player_id).await;
    cleanup_player(&pool, &lower.player_id).await;

    assert_eq!(upper_login.unwrap().player_id, upper.player_id, "A@X.io registered, a@x.io logs in");
    assert_eq!(lower_login.unwrap().player_id, lower.player_id, "b@x.io registered, B@X.IO logs in");
}

#[tokio::test]
async fn a_second_register_in_another_case_is_a_conflict() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let tag = suffix().to_ascii_lowercase();
    let first = svc.register(format!("dup-{tag}@x.io"), "pw".into(), "Dup".into()).await.unwrap();
    let second = svc.register(format!("DUP-{tag}@X.io"), "pw".into(), "Dup".into()).await;
    cleanup_player(&pool, &first.player_id).await;

    let e = second.map(|_| ()).unwrap_err();
    assert_eq!(e.status, opsapi::Status::Conflict, "one account per email in any case");
}
