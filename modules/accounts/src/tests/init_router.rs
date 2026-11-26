//! The router `init` mounts, served from `ctx.take_router()`: `register`, then the init body with
//! both browser flows parsed by `ProviderConfig::from_vars`, so no test touches process env.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr};

use super::hosted_login::client;
use super::*;
use crate::providers::ProviderConfig;

/// Epic's and Google's browser flows, enabled by their client secrets, on the default endpoints.
fn both_flows() -> ProviderConfig {
    let vars: BTreeMap<String, String> = [
        ("EPIC_CLIENT_ID", "epic-client"),
        ("EPIC_CLIENT_SECRET", "epic-secret"),
        ("GOOGLE_CLIENT_IDS", "google-client"),
        ("GOOGLE_CLIENT_SECRET", "google-secret"),
        ("GOOGLE_OAUTH_CLIENT_ID", "google-client"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    let config = ProviderConfig::from_vars(&vars).unwrap();
    assert!(config.epic.as_ref().is_some_and(|epic| epic.oauth.is_some()), "the secret enables Epic's flow");
    assert!(config.google.as_ref().is_some_and(|google| google.oauth.is_some()), "the secret enables Google's flow");
    config
}

#[tokio::test(flavor = "multi_thread")]
async fn init_mounts_both_callbacks_outside_the_bucket_and_the_page_headers() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let transport = asyncevents::testing::transport(pool.clone());
    let ctx = Context::with_db_and_transport(pool.clone(), transport.handle());
    let accounts = Accounts::new();
    accounts.register(&ctx).unwrap();
    accounts.init_with(&ctx, both_flows(), false, Vec::new()).unwrap();
    let app = ctx.take_router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>())
            .await
            .unwrap();
    });
    let bucket = accounts.web_limiter.get().expect("init sets the sign-in bucket");
    let drain = || while bucket.allow(IpAddr::V4(Ipv4Addr::LOCALHOST)) {};

    drain();
    let epic_start = client().post(format!("{base}/accounts/epic/start")).send().await.unwrap();
    let mut callbacks = Vec::new();
    for path in ["/accounts/oidc/google/callback", "/accounts/epic/callback"] {
        drain();
        let resp = client().get(format!("{base}{path}?code=c&state=no-such-state")).send().await.unwrap();
        callbacks.push((path, resp));
    }

    assert_eq!(epic_start.status(), 429, "init mounts the Epic start on the sign-in bucket");
    for (path, resp) in &callbacks {
        assert_eq!(resp.status(), 400, "{path} reaches its handler with the bucket empty: unknown state");
        assert!(!resp.headers().contains_key("content-security-policy"), "{path} carries no page headers");
        assert_eq!(
            resp.headers().get("cache-control").and_then(|v| v.to_str().ok()),
            Some("no-store"),
            "{path} answers its error uncached"
        );
    }
}
