//! `POST /accounts/oidc/revoke` and the discovery document, against the live database. A child
//! of `tests` so it reuses that module's live-DB harness.

use accountsapi::grant_types::{AUTHORIZATION_CODE, REFRESH_TOKEN};

use super::hosted_login::{client, registered, serve_default};
use super::token::{
    code_family, exchange, families, family_revocations, issue, json, set, token_client, SECRET,
};
use super::*;
use crate::web::authorize::AUTHORIZE;
use crate::web::device::DEVICE_AUTHORIZATION;
use crate::web::revoke::REVOKE;
use crate::web::token::TOKEN;

/// The audience of a client whose tokens the Swarog verifiers refuse (Lore's).
const LORE: &str = "lore";

async fn post_revoke(base: &str, form: &[(&str, &str)], basic: Option<(&str, &str)>) -> reqwest::Response {
    let mut req = client().post(format!("{base}{REVOKE}")).form(form);
    if let Some((id, secret)) = basic {
        req = req.basic_auth(id, Some(secret));
    }
    req.send().await.unwrap()
}

/// Revokes `token` as public client `client_id`, asserting the RFC 7009 200.
async fn revoke_ok(base: &str, client_id: &str, token: &str, case: &str) {
    let resp = post_revoke(base, &[("token", token), ("client_id", client_id)], None).await;
    assert_eq!(resp.status(), 200, "{case}");
    assert_eq!(resp.headers()["cache-control"], "no-store", "{case}");
}

/// One login of `pid` as `client_id` (confidential under [`SECRET`] when `secret`): the
/// access token, the refresh token and the family.
async fn login(
    base: &str,
    svc: &Arc<Service>,
    pool: &PgPool,
    client_id: &str,
    pid: &str,
    secret: bool,
) -> (String, String, String) {
    let code = issue(svc, client_id, pid, "openid", None).await;
    let form = set(exchange(client_id, &code), "client_secret", secret.then_some(SECRET));
    let resp = client().post(format!("{base}{TOKEN}")).form(&form).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body = json(resp).await;
    (
        body["access_token"].as_str().unwrap().to_string(),
        body["refresh_token"].as_str().unwrap().to_string(),
        code_family(pool, &code).await,
    )
}

async fn set_audience(pool: &PgPool, client_id: &str, audience: &str) {
    sqlx::query("UPDATE accounts.oidc_clients SET audience = $2 WHERE client_id = $1")
        .bind(client_id)
        .bind(audience)
        .execute(pool)
        .await
        .unwrap();
}

/// Revoking a refresh token deletes its family and writes the family revocation, and leaves
/// the player's other logins alone; the rotation then refuses it.
#[tokio::test(flavor = "multi_thread")]
async fn revoking_a_refresh_token_writes_its_family_revocation() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let base = serve_default(svc.clone()).await;
    let client_id = token_client(&pool, &[AUTHORIZATION_CODE, REFRESH_TOKEN], false).await;
    let (_, pid) = registered(&svc, "revoke-refresh").await;
    let (_, refresh, family) = login(&base, &svc, &pool, &client_id, &pid, false).await;
    let (_, _, other_family) = login(&base, &svc, &pool, &client_id, &pid, false).await;

    revoke_ok(&base, &client_id, &refresh, "refresh token").await;
    assert_eq!(families(&pool, &family).await, 0);
    assert_eq!(family_revocations(&pool, &family).await, 1);
    assert_eq!(families(&pool, &other_family).await, 1, "another login of the player went with it");
    assert_eq!(family_revocations(&pool, &other_family).await, 0);

    revoke_ok(&base, &client_id, &refresh, "the same token again").await;
    assert_eq!(family_revocations(&pool, &family).await, 1, "a repeat wrote a second revocation");
    let form = [("grant_type", REFRESH_TOKEN), ("refresh_token", refresh.as_str()), ("client_id", client_id.as_str())];
    let resp = client().post(format!("{base}{TOKEN}")).form(&form).send().await.unwrap();
    assert_eq!(resp.status(), 400, "a revoked refresh token rotated");

    cleanup_player(&pool, &pid).await;
    delete_client(&pool, &client_id).await;
}

/// An access token of the client revokes its family, whatever its audience: the Swarog
/// verifier refuses a Lore token, so the revoke-side check takes the client's own audience.
#[tokio::test(flavor = "multi_thread")]
async fn revoking_an_access_token_by_its_client_revokes_its_family() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let base = serve_default(svc.clone()).await;
    let swarog = token_client(&pool, &[AUTHORIZATION_CODE, REFRESH_TOKEN], false).await;
    let lore = token_client(&pool, &[AUTHORIZATION_CODE, REFRESH_TOKEN], true).await;
    set_audience(&pool, &lore, LORE).await;
    let (_, pid) = registered(&svc, "revoke-access").await;

    let (access, _, family) = login(&base, &svc, &pool, &swarog, &pid, false).await;
    assert!(svc.tokens.verify_access(&access).await.unwrap().is_some());
    revoke_ok(&base, &swarog, &access, "swarog access token").await;
    assert_eq!((families(&pool, &family).await, family_revocations(&pool, &family).await), (0, 1));
    assert!(svc.tokens.verify_access(&access).await.unwrap().is_none(), "the revoked token still verifies");

    let (access, refresh, family) = login(&base, &svc, &pool, &lore, &pid, true).await;
    assert!(svc.tokens.verify_access(&access).await.unwrap().is_none(), "the Swarog verifier took a Lore token");
    let resp = post_revoke(&base, &[("token", &access)], Some((&lore, SECRET))).await;
    assert_eq!(resp.status(), 200);
    assert_eq!((families(&pool, &family).await, family_revocations(&pool, &family).await), (0, 1));
    let form = [("grant_type", REFRESH_TOKEN), ("refresh_token", refresh.as_str())];
    let resp = client().post(format!("{base}{TOKEN}")).basic_auth(&lore, Some(SECRET)).form(&form).send().await.unwrap();
    assert_eq!(resp.status(), 400, "the family's refresh token outlived its access token's revocation");

    cleanup_player(&pool, &pid).await;
    delete_client(&pool, &swarog).await;
    delete_client(&pool, &lore).await;
}

/// An unknown token, another client's token and one not signed for the presenting client's
/// audience are all a 200 that revokes nothing.
#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_or_foreign_token_is_a_200_that_revokes_nothing() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let base = serve_default(svc.clone()).await;
    let owner = token_client(&pool, &[AUTHORIZATION_CODE, REFRESH_TOKEN], false).await;
    let same_audience = token_client(&pool, &[AUTHORIZATION_CODE, REFRESH_TOKEN], false).await;
    let other_audience = token_client(&pool, &[AUTHORIZATION_CODE, REFRESH_TOKEN], false).await;
    set_audience(&pool, &other_audience, LORE).await;
    let (_, pid) = registered(&svc, "revoke-foreign").await;
    let (access, refresh, family) = login(&base, &svc, &pool, &owner, &pid, false).await;

    for (presenter, case) in [(&same_audience, "same audience"), (&other_audience, "other audience")] {
        revoke_ok(&base, presenter, &refresh, case).await;
        revoke_ok(&base, presenter, &access, case).await;
    }
    for token in ["not-a-token", "a.b.c", &"x".repeat(accountsapi::MAX_ACCESS_TOKEN_BYTES + 1)] {
        revoke_ok(&base, &owner, token, "unknown").await;
    }
    assert_eq!(families(&pool, &family).await, 1, "a foreign or unknown token revoked the family");
    assert_eq!(family_revocations(&pool, &family).await, 0);
    assert!(svc.tokens.verify_access(&access).await.unwrap().is_some());

    cleanup_player(&pool, &pid).await;
    for id in [&owner, &same_audience, &other_audience] {
        delete_client(&pool, id).await;
    }
}

/// A confidential client authenticates here as at `/token`: no secret is 401 and revokes
/// nothing; a request without a token or a form is 400.
#[tokio::test(flavor = "multi_thread")]
async fn a_confidential_client_without_its_secret_is_401() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let base = serve_default(svc.clone()).await;
    let client_id = token_client(&pool, &[AUTHORIZATION_CODE, REFRESH_TOKEN], true).await;
    let (_, pid) = registered(&svc, "revoke-conf").await;
    let (_, refresh, family) = login(&base, &svc, &pool, &client_id, &pid, true).await;

    let resp = post_revoke(&base, &[("token", &refresh), ("client_id", &client_id)], None).await;
    assert!(resp.headers().get("www-authenticate").is_none(), "a Basic challenge without Basic");
    assert_eq!(resp.status(), 401);
    assert_eq!(json(resp).await["error"], "invalid_client");
    let resp = post_revoke(&base, &[("token", &refresh)], Some((&client_id, "cs_wrong"))).await;
    assert_eq!(resp.status(), 401);
    assert!(resp.headers()["www-authenticate"].to_str().unwrap().starts_with("Basic "));
    let wrong = [("token", refresh.as_str()), ("client_id", client_id.as_str()), ("client_secret", "cs_wrong")];
    assert_eq!(post_revoke(&base, &wrong, None).await.status(), 401);
    assert_eq!(families(&pool, &family).await, 1, "an unauthenticated request revoked the family");

    let resp = post_revoke(&base, &[("client_id", &client_id), ("client_secret", SECRET)], None).await;
    assert_eq!(resp.status(), 400);
    assert_eq!(json(resp).await["error"], "invalid_request");
    let raw = client()
        .post(format!("{base}{REVOKE}"))
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(raw.status(), 400, "a JSON body");

    let resp = post_revoke(&base, &[("token", &refresh)], Some((&client_id, SECRET))).await;
    assert_eq!(resp.status(), 200);
    assert_eq!((families(&pool, &family).await, family_revocations(&pool, &family).await), (0, 1));

    cleanup_player(&pool, &pid).await;
    delete_client(&pool, &client_id).await;
}

/// A database outage is 503, never a client error and never a silent 200.
#[tokio::test]
async fn a_database_outage_is_503() {
    let svc = dead_service_at("postgres://gamebackend:gamebackend@127.0.0.1:1/gamebackend", Arc::new(ArgonVerifier));
    let base = serve_default(svc).await;
    let resp = post_revoke(&base, &[("token", "t"), ("client_id", "any")], None).await;
    assert_eq!(resp.status(), 503);
    assert_eq!(resp.headers()["cache-control"], "no-store");
    assert_eq!(json(resp).await["error"], "temporarily_unavailable");
}

/// The discovery document lists the issuer's endpoints under the issuer and what they
/// support; the endpoint paths are the routes the hosted router serves.
#[tokio::test]
async fn discovery_lists_the_endpoints_under_the_issuer_and_what_they_support() {
    let pool = PgPool::connect_lazy(DEFAULT_DSN).unwrap();
    let origin = "https://issuer.example";
    let issuer = format!("{origin}/accounts/oidc");
    let app = crate::discovery::router(&issuer, test_tokens(&pool).keys());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let url = format!("http://{addr}{}", crate::discovery::DISCOVERY);
    let doc = json(client().get(url).send().await.unwrap()).await;

    let auth_methods = serde_json::json!(["none", "client_secret_basic", "client_secret_post"]);
    let expected = serde_json::json!({
        "issuer": issuer,
        "jwks_uri": format!("{origin}{}", crate::discovery::JWKS),
        "authorization_endpoint": format!("{origin}{AUTHORIZE}"),
        "token_endpoint": format!("{origin}{TOKEN}"),
        "revocation_endpoint": format!("{origin}{REVOKE}"),
        "device_authorization_endpoint": format!("{origin}{DEVICE_AUTHORIZATION}"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token", "urn:ietf:params:oauth:grant-type:device_code"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": auth_methods,
        "revocation_endpoint_auth_methods_supported": auth_methods,
        "id_token_signing_alg_values_supported": ["ES256"],
        "subject_types_supported": ["public"],
        "scopes_supported": ["openid", "profile", "offline_access"],
    });
    assert_eq!(doc, expected);
}
