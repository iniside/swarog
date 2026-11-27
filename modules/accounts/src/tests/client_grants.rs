//! The client a login names (`Service::issue_session_tx`, `Service::refresh_allowed_tx`)
//! and the token family every mint keeps (`accounts.token_families`), which the presence
//! readers count.

use super::*;

use accountsapi::grant_types::{AUTHORIZATION_CODE, GUEST, PASSWORD, REFRESH_TOKEN};

fn assert_invalid_client(err: Error, what: &str) {
    assert_eq!(err.status, opsapi::Status::Unauthorized, "{what}: {err:?}");
    assert_eq!(err.msg, "invalid client", "{what}");
}

async fn family_of(pool: &PgPool, refresh_token: &str) -> String {
    sqlx::query_scalar("SELECT family_id::text FROM accounts.refresh_tokens WHERE token = $1")
        .bind(refresh_token)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn family_rows(pool: &PgPool, family_id: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM accounts.token_families WHERE id = $1::uuid")
        .bind(family_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn token_used(pool: &PgPool, refresh_token: &str) -> bool {
    sqlx::query_scalar("SELECT used_at IS NOT NULL FROM accounts.refresh_tokens WHERE token = $1")
        .bind(refresh_token)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn online_until(svc: &Service, player_id: &str) -> String {
    let rows = svc.store.players_by_id(&[player_id.to_string()]).await.unwrap();
    rows.into_iter().next().unwrap().online_until
}

#[tokio::test]
async fn an_unknown_client_is_refused_on_every_login_op() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let unknown = format!("t-unknown-{}", suffix());
    let email = format!("nocl-{}@test.local", suffix().to_lowercase());

    let register = svc.register(email.clone(), "pw".into(), "NoCl".into(), unknown.clone()).await;
    let guest = svc.create_guest(unknown.clone()).await;
    let reg = svc.register(email.clone(), "pw".into(), "NoCl".into(), test_client()).await.unwrap();
    let login = svc.login(email, "pw".into(), unknown).await;
    cleanup_player(&pool, &reg.player_id).await;

    assert_invalid_client(register.unwrap_err(), "register");
    assert_invalid_client(guest.unwrap_err(), "create_guest");
    assert_invalid_client(login.unwrap_err(), "login");
}

/// Every login op's `clientId` cap at its edge, driven through the op on a pool that cannot
/// connect: at exactly the cap the op gets past the guard to the store (Internal) or the
/// provider registry (Unavailable); one byte over, the guard alone answers `invalid client`.
#[test]
fn every_login_op_admits_a_client_id_of_exactly_the_cap_and_refuses_one_byte_more() {
    use crate::conformance::{self as c, client_id_outcome, ClientIdOp};
    let cap = accountsapi::MAX_CLIENT_ID_BYTES;
    let ops = [
        (ClientIdOp::Register, c::conformance_register_client_id_rejected as fn(usize) -> bool),
        (ClientIdOp::Login, c::conformance_login_client_id_rejected),
        (ClientIdOp::LoginFederated, c::conformance_login_federated_client_id_rejected),
        (ClientIdOp::CreateGuest, c::conformance_create_guest_client_id_rejected),
    ];
    for (op, rejected) in ops {
        let at_cap = client_id_outcome(op, cap).expect_err("a dead pool mints nothing").status;
        assert!(
            matches!(at_cap, opsapi::Status::Internal | opsapi::Status::Unavailable),
            "{op:?} at the cap: {at_cap:?}"
        );
        let over = client_id_outcome(op, cap + 1).expect_err("one byte over the cap");
        assert_invalid_client(over, &format!("{op:?} over the cap"));
        assert!(!rejected(cap) && rejected(cap + 1), "{op:?}: the conformance probe");
    }
}

#[tokio::test]
async fn a_client_without_the_password_grant_cannot_register_or_log_in() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client = insert_client(&pool, &[REFRESH_TOKEN]).await;
    let email = format!("nogrant-{}@test.local", suffix().to_lowercase());

    let refused_register = svc.register(email.clone(), "pw".into(), "NoGrant".into(), client.clone()).await;
    let after_refusal = svc.login(email.clone(), "pw".into(), test_client()).await;
    let reg = svc.register(email.clone(), "pw".into(), "NoGrant".into(), test_client()).await.unwrap();
    let refused_login = svc.login(email, "pw".into(), client.clone()).await;
    cleanup_player(&pool, &reg.player_id).await;
    delete_client(&pool, &client).await;

    assert_invalid_client(refused_register.unwrap_err(), "register");
    assert_eq!(
        after_refusal.unwrap_err().status,
        opsapi::Status::Unauthorized,
        "a refused registration creates no account"
    );
    assert_invalid_client(refused_login.unwrap_err(), "login");
}

#[tokio::test]
async fn refresh_is_refused_once_the_client_drops_refresh_token_and_keeps_the_token() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client = insert_client(&pool, &[PASSWORD, REFRESH_TOKEN]).await;
    let email = format!("dropref-{}@test.local", suffix().to_lowercase());
    let reg = svc.register(email, "pw".into(), "DropRef".into(), client.clone()).await.unwrap();

    set_client_grants(&pool, &client, &[PASSWORD]).await;
    let refused = svc.refresh(reg.refresh_token.clone()).await;
    let used_after_refusal = token_used(&pool, &reg.refresh_token).await;
    set_client_grants(&pool, &client, &[PASSWORD, REFRESH_TOKEN]).await;
    let restored = svc.refresh(reg.refresh_token.clone()).await.map(|_| ());
    cleanup_player(&pool, &reg.player_id).await;
    delete_client(&pool, &client).await;

    assert_invalid_client(refused.unwrap_err(), "refresh");
    assert!(!used_after_refusal, "a refused refresh leaves its token unconsumed");
    restored.unwrap();
}

#[tokio::test]
async fn a_revoked_familys_row_is_gone_and_no_longer_counts_as_online() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("revfam-{}@test.local", suffix().to_lowercase());
    let reg = svc.register(email, "pw".into(), "RevFam".into(), test_client()).await.unwrap();
    let family = family_of(&pool, &reg.refresh_token).await;
    let online_before = online_until(&svc, &reg.player_id).await;
    let rows_before = family_rows(&pool, &family).await;

    let mut tx = pool.begin().await.unwrap();
    svc.store.kill_family_tx(&mut tx, &family).await.unwrap();
    tx.commit().await.unwrap();
    let rows_after = family_rows(&pool, &family).await;
    let online_after = online_until(&svc, &reg.player_id).await;
    cleanup_player(&pool, &reg.player_id).await;

    assert_eq!(rows_before, 1, "every mint keeps its family row");
    assert!(!online_before.is_empty(), "a live family counts as online");
    assert_eq!(rows_after, 0, "a revoked family's row is deleted");
    assert_eq!(online_after, "", "a revoked family no longer counts as online");
}

#[tokio::test]
async fn a_refresh_moves_the_familys_access_expiry() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("touch-{}@test.local", suffix().to_lowercase());
    let reg = svc.register(email, "pw".into(), "Touch".into(), test_client()).await.unwrap();
    let family = family_of(&pool, &reg.refresh_token).await;
    sqlx::query(
        "UPDATE accounts.token_families SET access_expires_at = now() - interval '1 minute' \
          WHERE id = $1::uuid",
    )
    .bind(&family)
    .execute(&pool)
    .await
    .unwrap();
    let online_lapsed = online_until(&svc, &reg.player_id).await;

    let refreshed = svc.refresh(reg.refresh_token.clone()).await.map(|_| ());
    let online_refreshed = online_until(&svc, &reg.player_id).await;
    cleanup_player(&pool, &reg.player_id).await;

    assert_eq!(online_lapsed, "", "a family whose access lapsed is not online");
    refreshed.unwrap();
    assert!(!online_refreshed.is_empty(), "the refresh's access token extends the family");
}

#[tokio::test]
async fn deleting_a_client_ends_the_access_tokens_it_was_issued() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client = insert_client(&pool, &[PASSWORD, REFRESH_TOKEN]).await;
    let email = format!("delcl-{}@test.local", suffix().to_lowercase());
    let reg = svc.register(email, "pw".into(), "DelCl".into(), client.clone()).await.unwrap();
    let before = svc.tokens.verify_access(&reg.token).await.unwrap().map(|c| c.sub);

    let deleted = svc.store.delete_client(&client).await.unwrap();
    let after = svc.tokens.verify_access(&reg.token).await.unwrap();
    let refreshed = svc.refresh(reg.refresh_token.clone()).await;
    cleanup_player(&pool, &reg.player_id).await;

    assert_eq!(before, Some(reg.player_id.clone()), "the bearer verifies while its client exists");
    assert!(deleted);
    assert!(after.is_none(), "a deleted client's bearer no longer verifies");
    assert_eq!(refreshed.unwrap_err().status, opsapi::Status::Unauthorized);
}

#[tokio::test]
async fn an_external_login_needs_only_an_existing_client() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client = insert_client(&pool, &[REFRESH_TOKEN]).await;
    let sub = format!("epic-{}", suffix());

    let session = svc.external_login("epic", &sub, "epic:grantless", None, &client).await.map(|(s, _)| s);
    if let Ok(s) = &session {
        cleanup_player(&pool, &s.player_id).await;
    }
    delete_client(&pool, &client).await;

    session.expect("a client listing neither password nor guest still serves an external login");
}

/// A confidential client (one storing a secret digest) is refused on every op-path login and
/// on refresh even when it lists the grant; only an authorization_code mint admits it.
#[tokio::test]
async fn a_confidential_client_is_refused_on_every_op_path_and_admitted_for_authorization_code() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client = insert_client(&pool, &[PASSWORD, GUEST, AUTHORIZATION_CODE, REFRESH_TOKEN]).await;
    let email = format!("conf-{}@test.local", suffix().to_lowercase());
    let reg = svc.register(email.clone(), "pw".into(), "Conf".into(), client.clone()).await.unwrap();
    sqlx::query("UPDATE accounts.oidc_clients SET secret_digest = 'x' WHERE client_id = $1")
        .bind(&client)
        .execute(&pool)
        .await
        .unwrap();

    let other = format!("conf-{}@test.local", suffix().to_lowercase());
    let register = svc.register(other, "pw".into(), "Conf".into(), client.clone()).await;
    let login = svc.login(email, "pw".into(), client.clone()).await;
    let guest = svc.create_guest(client.clone()).await;
    let sub = format!("epic-{}", suffix());
    let external = svc.external_login("epic", &sub, "epic:conf", None, &client).await.map(|(s, _)| s);
    let refreshed = svc.refresh(reg.refresh_token.clone()).await;
    let used = token_used(&pool, &reg.refresh_token).await;
    let mut conn = pool.acquire().await.unwrap();
    let code = svc.store.client_allows_tx(&mut conn, &client, Some(AUTHORIZATION_CODE)).await.unwrap();
    let password = svc.store.client_allows_tx(&mut conn, &client, Some(PASSWORD)).await.unwrap();
    drop(conn);
    let minted = [
        register.as_ref().ok().map(|s| s.player_id.clone()),
        guest.as_ref().ok().map(|s| s.player_id.clone()),
        external.as_ref().ok().map(|s| s.player_id.clone()),
        Some(reg.player_id.clone()),
    ];
    for player in minted.into_iter().flatten() {
        cleanup_player(&pool, &player).await;
    }
    delete_client(&pool, &client).await;

    assert_invalid_client(register.unwrap_err(), "register");
    assert_invalid_client(login.unwrap_err(), "login");
    assert_invalid_client(guest.unwrap_err(), "create_guest");
    assert_invalid_client(external.unwrap_err(), "external_login");
    assert_invalid_client(refreshed.unwrap_err(), "refresh");
    assert!(!used, "a refused refresh leaves its token unconsumed");
    assert!(code.is_some_and(|g| g.refresh_token), "an authorization_code mint admits it, with refresh");
    assert!(password.is_none(), "a password mint does not");
}

/// Migrate refuses a `refresh_tokens` without an ON DELETE CASCADE foreign key to its
/// schema's `token_families`, naming the table and the drop remedy; a key that does not
/// cascade does not count, and a cascading one, like the one migrate creates, passes.
#[tokio::test(flavor = "multi_thread")]
async fn boot_refusal_fires_on_tokens_without_a_family_key() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let schema = format!("accounts_unfamilied_{}_{nanos}", std::process::id());
    let exec = |sql: String| {
        let pool = pool.clone();
        async move {
            sqlx::query(&sql).execute(&pool).await.unwrap();
        }
    };
    exec(format!("CREATE SCHEMA {schema}")).await;
    exec(format!("CREATE TABLE {schema}.players (id int PRIMARY KEY)")).await;
    exec(format!("CREATE TABLE {schema}.token_families (id int PRIMARY KEY)")).await;
    exec(format!(
        "CREATE TABLE {schema}.refresh_tokens \
         (token text PRIMARY KEY, player_id int REFERENCES {schema}.players(id), family_id int)"
    ))
    .await;
    let link = |on_delete: &str| {
        format!(
            "ALTER TABLE {schema}.refresh_tokens ADD CONSTRAINT family_key \
             FOREIGN KEY (family_id) REFERENCES {schema}.token_families(id) {on_delete}"
        )
    };
    let no_refresh_key = crate::refuse_unfamilied_tokens(&pool, &schema).await;
    exec(link("")).await;
    let refresh_key_kept = crate::refuse_unfamilied_tokens(&pool, &schema).await;
    exec(format!("ALTER TABLE {schema}.refresh_tokens DROP CONSTRAINT family_key")).await;
    exec(link("ON DELETE CASCADE")).await;
    let linked = crate::refuse_unfamilied_tokens(&pool, &schema).await;
    exec(format!("DROP SCHEMA {schema} CASCADE")).await;

    for (case, refused) in [("no key", no_refresh_key), ("a key kept on delete", refresh_key_kept)] {
        let err = refused.expect_err(case).to_string();
        let names = format!(
            "{schema}.refresh_tokens has no ON DELETE CASCADE foreign key to {schema}.token_families"
        );
        assert!(err.contains(&names), "{err}");
        assert!(err.contains(&format!("DROP SCHEMA {schema} CASCADE;")), "{err}");
    }
    linked.expect("a fully linked set passes");
    crate::refuse_unfamilied_tokens(&pool, "accounts").await.unwrap();
}

/// Migrate refuses an `oidc_clients` without any one column the client code reads, naming
/// the column and the drop remedy; a table holding them all passes.
#[tokio::test]
async fn boot_refusal_fires_on_a_clients_table_missing_a_column() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let schema = format!("accounts_staleclients_{}_{nanos}", std::process::id());
    let exec = |sql: String| {
        let pool = pool.clone();
        async move {
            sqlx::query(&sql).execute(&pool).await.unwrap();
        }
    };
    let columns = [
        ("redirect_uris", "text[]"),
        ("root_domains", "text[]"),
        ("scopes", "text[]"),
        ("secret_digest", "text"),
        ("device_code_ttl_secs", "integer"),
    ];
    let table = |without: Option<&str>| {
        let kept: String = columns
            .iter()
            .filter(|(c, _)| Some(*c) != without)
            .map(|(c, t)| format!(", {c} {t}"))
            .collect();
        format!("CREATE TABLE {schema}.oidc_clients (client_id text PRIMARY KEY{kept})")
    };
    exec(format!("CREATE SCHEMA {schema}")).await;
    let mut refusals = Vec::new();
    for (missing, _) in columns {
        exec(table(Some(missing))).await;
        refusals.push((missing, crate::refuse_stale_clients(&pool, &schema).await));
        exec(format!("DROP TABLE {schema}.oidc_clients")).await;
    }
    exec(table(None)).await;
    let complete = crate::refuse_stale_clients(&pool, &schema).await;
    exec(format!("DROP SCHEMA {schema} CASCADE")).await;

    for (missing, refused) in refusals {
        let err = refused.expect_err(missing).to_string();
        assert!(err.contains(&format!("{schema}.oidc_clients has no {missing} column")), "{err}");
        assert!(err.contains(&format!("DROP SCHEMA {schema} CASCADE;")), "{err}");
    }
    complete.expect("a table holding every column passes");
    crate::refuse_stale_clients(&pool, "accounts").await.unwrap();
}
