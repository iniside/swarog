//! The sealed OIDC signing keys: the KEK and issuer parses, seal/open, and the boot-time
//! guarantee of one active key. Tests that need an empty or stale table run on a scratch
//! schema, so the shared `accounts.signing_keys` only ever holds keys sealed with the dev KEK.

use super::*;
use crate::signing_keys::{
    ensure_active_key, insert_active_key, kek_from, oidc_issuer, open, refuse_stale_signing_keys, required_kek, seal,
    thumbprint, SigningKek, KEK_ENV, SIGNING_KEYS_DDL, SIGNING_KEYS_ONE_ACTIVE_DDL,
};
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use ring::signature::{EcdsaKeyPair, KeyPair as _, ECDSA_P256_SHA256_FIXED_SIGNING};
use std::ffi::OsString;

fn os(value: &str) -> Option<OsString> {
    Some(value.into())
}

fn leak(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

fn other_kek() -> SigningKek {
    SigningKek::parse(&STANDARD.encode([9u8; 32])).unwrap()
}

async fn scratch_schema(pool: &PgPool, label: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let schema = format!("accounts_keys_{label}_{}_{nanos}", std::process::id());
    sqlx::query(&format!("CREATE SCHEMA {schema}")).execute(pool).await.unwrap();
    schema
}

/// The table and index migrate creates, on `schema` instead of `accounts`.
async fn scratch_table(pool: &PgPool, label: &str) -> String {
    let schema = scratch_schema(pool, label).await;
    let qualified = format!("{schema}.");
    Ddl::new(pool)
        .sql(leak(SIGNING_KEYS_DDL.replace("accounts.", &qualified)))
        .index(
            leak(format!("{schema}.signing_keys_one_active_idx")),
            leak(SIGNING_KEYS_ONE_ACTIVE_DDL.replace("accounts.", &qualified)),
        )
        .run()
        .await
        .unwrap();
    schema
}

async fn drop_schema(pool: &PgPool, schema: &str) {
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE")).execute(pool).await.unwrap();
}

/// `(kid, status, public_jwk, sealed_key, nonce)`.
type KeyRow = (String, String, String, Vec<u8>, Vec<u8>);

/// Every row, by kid.
async fn key_rows(pool: &PgPool, schema: &str) -> Vec<KeyRow> {
    sqlx::query_as(&format!(
        "SELECT kid, status, public_jwk::text, sealed_key, nonce FROM {schema}.signing_keys \
         ORDER BY kid"
    ))
    .fetch_all(pool)
    .await
    .unwrap()
}

#[test]
fn the_dev_kek_parses_through_the_real_parser() {
    assert!(required_kek(os(testdb::DEV_ACCOUNTS_SIGNING_KEK)).is_ok());
}

#[test]
fn a_missing_blank_or_malformed_kek_is_refused_without_echoing_it() {
    assert!(kek_from(None).unwrap().is_none());
    let missing = required_kek(None).unwrap_err().to_string();
    assert!(missing.contains("ACCOUNTS_SIGNING_KEK is required"), "{missing}");

    let mut trailing_bits = STANDARD.encode([0u8; 32]).into_bytes();
    trailing_bits[42] = b'B';
    let trailing_bits = String::from_utf8(trailing_bits).unwrap();
    let spaced = format!(" {}", testdb::DEV_ACCOUNTS_SIGNING_KEK);
    let malformed = [
        String::new(),
        "   ".into(),
        "hunter2".into(),
        STANDARD.encode([7u8; 16]),
        STANDARD.encode([7u8; 33]),
        URL_SAFE.encode([0xfbu8; 32]),
        STANDARD_NO_PAD.encode([7u8; 32]),
        trailing_bits,
        spaced,
    ];
    for bad in &malformed {
        let err = format!("{:#}", required_kek(os(bad)).unwrap_err());
        assert!(err.contains("invalid ACCOUNTS_SIGNING_KEK"), "{bad:?}: {err}");
        if !bad.trim().is_empty() {
            assert!(!err.contains(bad.trim()), "the error echoes the KEK: {err}");
        }
    }
    assert_eq!(format!("{:?}", dev_kek()), "SigningKek(..)");
}

#[test]
fn the_issuer_defaults_to_the_monolith_and_refuses_every_other_shape() {
    assert_eq!(oidc_issuer(None).unwrap(), accountsapi::DEFAULT_OIDC_ISSUER);
    for good in [
        accountsapi::DEFAULT_OIDC_ISSUER,
        "http://127.0.0.1:8082/accounts/oidc",
        "https://auth.example.com/accounts/oidc",
    ] {
        assert_eq!(oidc_issuer(os(good)).unwrap(), good);
    }
    for bad in [
        "",
        "hunter2",
        "/accounts/oidc",
        "ftp://auth.example.com/accounts/oidc",
        "http://auth.example.com/accounts/oidc/",
        "http://auth.example.com/accounts",
        "http://auth.example.com/accounts/oidc/x",
        "http://auth.example.com/accounts/oidc?x=1",
        "http://auth.example.com/accounts/oidc?",
        "http://auth.example.com/accounts/oidc#top",
        "http://user:pass@auth.example.com/accounts/oidc",
        "HTTP://auth.example.com/accounts/oidc",
        "http://auth.example.com:80/accounts/oidc",
    ] {
        let err = oidc_issuer(os(bad)).unwrap_err().to_string();
        assert!(err.contains("invalid ACCOUNTS_OIDC_ISSUER"), "{bad:?}: {err}");
    }
}

#[test]
fn a_sealed_key_opens_only_with_its_kek_kid_nonce_and_ciphertext() {
    let kek = dev_kek();
    let (nonce, sealed) = seal(&kek, "kid-a", b"pkcs8 der").unwrap();
    assert_eq!(open(&kek, "kid-a", &nonce, &sealed).as_deref(), Some(&b"pkcs8 der"[..]));
    assert!(open(&kek, "kid-b", &nonce, &sealed).is_none(), "another kid (AAD) opened it");
    assert!(open(&other_kek(), "kid-a", &nonce, &sealed).is_none(), "another KEK opened it");
    let mut moved_nonce = nonce.clone();
    moved_nonce[0] ^= 1;
    assert!(open(&kek, "kid-a", &moved_nonce, &sealed).is_none(), "another nonce opened it");
    assert!(open(&kek, "kid-a", &nonce[..11], &sealed).is_none(), "a short nonce opened it");
    for at in [0, sealed.len() - 1] {
        let mut flipped = sealed.clone();
        flipped[at] ^= 1;
        assert!(open(&kek, "kid-a", &nonce, &flipped).is_none(), "byte {at} flipped and opened");
    }
    let (again, _) = seal(&kek, "kid-a", b"pkcs8 der").unwrap();
    assert_ne!(nonce, again, "every seal draws a fresh nonce");
}

/// Missing at migrate (register did not run) fails before any I/O, on a pool that cannot
/// connect.
#[tokio::test(flavor = "multi_thread")]
async fn migrate_without_a_kek_fails_naming_the_variable_before_any_io() {
    assert!(
        std::env::var_os(KEK_ENV).is_none(),
        "the accounts suite runs with {KEK_ENV} unset; tests pass it through \
         Accounts::with_signing_kek"
    );
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://nobody@127.0.0.1:1/nowhere")
        .unwrap();
    let err = Accounts::new().migrate(&Context::with_db(pool)).await.unwrap_err().to_string();
    assert!(err.contains("ACCOUNTS_SIGNING_KEK is required"), "{err}");
}

/// First boot inserts one active key whose JWK is the opened private key's public half and
/// whose kid is its RFC 7638 thumbprint; a second boot adds none.
#[tokio::test(flavor = "multi_thread")]
async fn first_boot_inserts_one_active_key_and_a_second_boot_adds_none() {
    let Some(pool) = test_pool().await else { return };
    let schema = scratch_table(&pool, "boot").await;
    let first = ensure_active_key(&pool, &schema, &dev_kek()).await;
    let after_first = key_rows(&pool, &schema).await;
    let second = ensure_active_key(&pool, &schema, &dev_kek()).await;
    let after_second = key_rows(&pool, &schema).await;
    drop_schema(&pool, &schema).await;

    first.unwrap();
    second.unwrap();
    assert_eq!(after_first, after_second, "a second boot changed the keys");
    let [(kid, status, jwk, sealed, nonce)] = &after_first[..] else {
        panic!("first boot left {} rows", after_first.len())
    };
    assert_eq!(status, "active");
    let jwk: serde_json::Value = serde_json::from_str(jwk).unwrap();
    let mut members: Vec<&str> = jwk.as_object().unwrap().keys().map(String::as_str).collect();
    members.sort_unstable();
    assert_eq!(members, ["alg", "crv", "kid", "kty", "use", "x", "y"]);
    assert_eq!((&jwk["kty"], &jwk["crv"], &jwk["alg"], &jwk["use"]), (
        &serde_json::json!("EC"),
        &serde_json::json!("P-256"),
        &serde_json::json!("ES256"),
        &serde_json::json!("sig"),
    ));
    let (x, y) = (jwk["x"].as_str().unwrap(), jwk["y"].as_str().unwrap());
    assert_eq!(kid, &thumbprint(x, y));
    assert_eq!(jwk["kid"], serde_json::json!(kid));

    let der = open(&dev_kek(), kid, nonce, sealed).expect("the row opens under the dev KEK");
    let pair = EcdsaKeyPair::from_pkcs8(
        &ECDSA_P256_SHA256_FIXED_SIGNING,
        &der,
        &ring::rand::SystemRandom::new(),
    )
    .unwrap();
    let point = pair.public_key().as_ref();
    assert_eq!(URL_SAFE_NO_PAD.encode(&point[1..33]), x);
    assert_eq!(URL_SAFE_NO_PAD.encode(&point[33..]), y);
}

/// Two boots without the migrate lock (bare test migrates) both read "no active key"; the
/// one whose insert lands second finds the winner's row and keeps it.
#[tokio::test(flavor = "multi_thread")]
async fn a_boot_that_loses_the_insert_race_keeps_the_winners_key() {
    let Some(pool) = test_pool().await else { return };
    let schema = scratch_table(&pool, "race").await;
    ensure_active_key(&pool, &schema, &dev_kek()).await.unwrap();
    let winner = key_rows(&pool, &schema).await;
    let loser = insert_active_key(&pool, &schema, &dev_kek()).await;
    let reopened = ensure_active_key(&pool, &schema, &dev_kek()).await;
    let after = key_rows(&pool, &schema).await;
    drop_schema(&pool, &schema).await;

    loser.unwrap();
    reopened.unwrap();
    assert_eq!(winner.len(), 1);
    assert_eq!(after, winner);
}

/// A KEK that did not seal the active key fails boot with the remedy and writes no row.
#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_kek_fails_boot_with_the_remedy_and_writes_no_row() {
    let Some(pool) = test_pool().await else { return };
    let schema = scratch_table(&pool, "wrongkek").await;
    ensure_active_key(&pool, &schema, &dev_kek()).await.unwrap();
    let before = key_rows(&pool, &schema).await;
    let refused = ensure_active_key(&pool, &schema, &other_kek()).await;
    let after = key_rows(&pool, &schema).await;
    drop_schema(&pool, &schema).await;

    let err = refused.expect_err("a wrong KEK must fail boot").to_string();
    assert!(
        err.contains(&format!(
            "Remedy: restore the previous ACCOUNTS_SIGNING_KEK, or DROP TABLE \
             {schema}.signing_keys (invalidates issued tokens)."
        )),
        "{err}"
    );
    assert_eq!(before, after);
}

/// A table of another shape is refused with the drop remedy before anything writes to it;
/// a missing table and the table migrate creates pass.
#[tokio::test(flavor = "multi_thread")]
async fn a_stale_signing_keys_table_is_refused_with_the_drop_remedy() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let stale = scratch_schema(&pool, "stale").await;
    let missing = refuse_stale_signing_keys(&pool, &stale).await;
    sqlx::query(&format!(
        "CREATE TABLE {stale}.signing_keys (kid text PRIMARY KEY, private_pem text NOT NULL)"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let refused = refuse_stale_signing_keys(&pool, &stale).await;
    let (stale_rows,): (i64,) =
        sqlx::query_as(&format!("SELECT count(*) FROM {stale}.signing_keys"))
            .fetch_one(&pool)
            .await
            .unwrap();
    drop_schema(&pool, &stale).await;
    let fresh = scratch_table(&pool, "fresh").await;
    let current = refuse_stale_signing_keys(&pool, &fresh).await;
    drop_schema(&pool, &fresh).await;

    missing.unwrap();
    let err = refused.expect_err("a stale signing_keys must refuse boot").to_string();
    assert!(err.contains(&format!("DROP TABLE {stale}.signing_keys;")), "{err}");
    assert_eq!(stale_rows, 0);
    current.unwrap();
    refuse_stale_signing_keys(&pool, "accounts").await.unwrap();
}
