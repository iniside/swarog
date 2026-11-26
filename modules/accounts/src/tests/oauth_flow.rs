//! The neutral OAuth flow's provider scoping and the boot refusal on a stale state table,
//! against the live database. A child of `tests` so it reuses that module's helpers.

use super::*;
use crate::oauth_flow::{ClientConfig, Intent, OAuthFlow};
use crate::providers::GOOGLE_SPEC;

fn google_flow(pool: PgPool) -> OAuthFlow {
    let verifier = Arc::new(
        OidcVerifier::new(
            "http://localhost/jwks",
            IssuerMatch::prefix("issuer", "https://issuer.example").unwrap(),
            vec!["aud".to_string()],
        )
        .unwrap(),
    );
    let client = ClientConfig {
        client_id: "cid".into(),
        client_secret: "sec".into(),
        redirect_uri: format!("http://localhost{}", GOOGLE_SPEC.callback_path),
        authorize_url: "http://localhost/authorize".into(),
        token_url: "http://localhost/token".into(),
    };
    OAuthFlow::new(&GOOGLE_SPEC, client, verifier, pool, false).unwrap()
}

/// A state is redeemable only by the provider that minted it: the other provider's
/// `take_state` is a miss that leaves the row in place, in both directions.
#[tokio::test(flavor = "multi_thread")]
async fn take_state_refuses_and_keeps_another_providers_state() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let epic = oauth_fixture(pool.clone(), "http://localhost/accounts/epic/callback", "http://localhost/token");
    let google = google_flow(pool.clone());
    let binding = store::new_token();

    let browser = || Intent::Browser { return_to: "/accounts/oidc/".into(), replaces: None };
    let google_state = google.new_state(browser(), binding.clone()).await.unwrap();
    assert_eq!(epic.take_state(&google_state, Some(&binding)).await.unwrap(), None);
    assert_eq!(google.take_state(&google_state, Some(&binding)).await.unwrap(), Some(browser()));

    let epic_state = epic.new_state(login_intent(), binding.clone()).await.unwrap();
    assert_eq!(google.take_state(&epic_state, Some(&binding)).await.unwrap(), None);
    assert_eq!(epic.take_state(&epic_state, Some(&binding)).await.unwrap(), Some(login_intent()));
}

/// D4: only Epic has `login` and `link` states, and the table refuses them for any other
/// provider: both inserts fail on the named `oauth_states_epic_only` CHECK, and without
/// it both succeed.
#[tokio::test(flavor = "multi_thread")]
async fn oauth_states_refuse_login_and_link_for_a_provider_other_than_epic() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let google = google_flow(pool.clone());
    let binding = store::new_token();

    for intent in [login_intent(), Intent::Link { session_token: "tok".into() }] {
        let label = format!("{intent:?}");
        let err = google.new_state(intent, binding.clone()).await.expect_err(&label);
        let db = err.downcast_ref::<sqlx::Error>().and_then(|e| e.as_database_error());
        let constraint = db.and_then(|e| e.constraint());
        assert_eq!(constraint, Some("oauth_states_epic_only"), "{label}: {err:#}");
    }
}

/// Migrate refuses an `oauth_states` without `provider`, naming the drop remedy; the
/// table migrate itself creates passes the same check.
#[tokio::test(flavor = "multi_thread")]
async fn boot_refusal_fires_on_oauth_states_without_provider() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let schema = format!("accounts_refusal_{}_{nanos}", std::process::id());
    sqlx::query(&format!("CREATE SCHEMA {schema}")).execute(&pool).await.unwrap();
    sqlx::query(&format!(
        "CREATE TABLE {schema}.oauth_states (state text PRIMARY KEY, intent text NOT NULL)"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let refused = crate::refuse_stale_oauth_states(&pool, &schema).await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE")).execute(&pool).await.unwrap();

    let err = refused.expect_err("an oauth_states without provider must refuse boot").to_string();
    assert!(err.contains("has no provider column"), "{err}");
    assert!(err.contains(&format!("DROP TABLE {schema}.oauth_states;")), "{err}");
    crate::refuse_stale_oauth_states(&pool, "accounts").await.unwrap();
}
