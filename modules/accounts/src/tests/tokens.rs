//! Access tokens: the signer, `verify_access` and the boot size check.

use super::*;
use crate::jwt::Tokens;

async fn random_uuid(pool: &PgPool) -> String {
    sqlx::query_scalar("SELECT gen_random_uuid()::text").fetch_one(pool).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_signed_token_round_trips_and_one_past_exp_and_leeway_is_refused() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let tokens = &svc.tokens;
    let (sub, sid) = (random_uuid(&pool).await, random_uuid(&pool).await);
    let now = Tokens::unix_now();

    let fresh = tokens.claims(&sub, accountsapi::SWAROG_AUDIENCE, TEST_CLIENT_ID, &sid, now);
    let token = tokens.sign(&fresh).await.unwrap();
    assert_eq!(tokens.verify_access(&token).await.unwrap(), Some(fresh.clone()));

    let stale = tokens.claims(&sub, accountsapi::SWAROG_AUDIENCE, TEST_CLIENT_ID, &sid, now - 3600);
    let expired = tokens.sign(&stale).await.unwrap();
    assert_eq!(tokens.verify_access(&expired).await.unwrap(), None, "a token an hour old verified");

    let snapshot = tokens.snapshot().await.unwrap();
    let leeway = accountsapi::ACCESS_EXP_LEEWAY.as_secs() as i64;
    assert_eq!(Tokens::verify_against(&snapshot, &token, fresh.exp + leeway), Some(fresh.clone()));
    assert_eq!(Tokens::verify_against(&snapshot, &token, fresh.exp + leeway + 1), None);

    let mut tampered = token.clone();
    let last = if tampered.ends_with('A') { "B" } else { "A" };
    tampered.replace_range(tampered.len() - 1.., last);
    assert_eq!(Tokens::verify_against(&snapshot, &tampered, now), None, "a bad signature verified");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_boot_size_check_passes_the_default_issuer_and_refuses_a_long_one() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    svc.tokens.check_token_size().await.unwrap();

    let long = Tokens::new(
        pool.clone(),
        "accounts",
        format!("https://{}.example/accounts/oidc", "a".repeat(600)),
        Arc::new(OnceLock::from(dev_kek())),
    );
    let err = long.check_token_size().await.unwrap_err().to_string();
    assert!(err.contains("ACCOUNTS_OIDC_ISSUER"), "{err}");
}
