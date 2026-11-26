//! The sealed OIDC signing keys: the KEK and issuer parses, seal/open, and what
//! `migrate_signing_keys` guarantees at boot. Tests that need an empty or stale table run it
//! on a scratch schema, so the shared `accounts.signing_keys` only ever holds keys sealed with
//! the dev KEK.

use super::*;
use crate::signing_keys::{
    kek_from, migrate_signing_keys, oidc_issuer, open, required_kek, seal, thumbprint, NewKey,
    SigningKek,
};
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use ring::signature::{EcdsaKeyPair, KeyPair as _, ECDSA_P256_SHA256_FIXED_SIGNING};
use std::ffi::OsString;

fn os(value: &str) -> Option<OsString> {
    Some(value.into())
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

/// A scratch schema after one boot under the dev KEK: the table, its index and one active key.
async fn booted_schema(pool: &PgPool, label: &str) -> String {
    let schema = scratch_schema(pool, label).await;
    migrate_signing_keys(pool, &schema, &dev_kek()).await.unwrap();
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

/// Asserts `refused` is the does-not-open failure naming `kid`, with its remedy.
fn assert_open_remedy(refused: anyhow::Result<()>, schema: &str, kid: &str) {
    let err = format!("{:#}", refused.expect_err("a key that does not open must fail boot"));
    assert!(err.contains(&format!("signing key {kid} in {schema}.signing_keys")), "{err}");
    assert!(
        err.contains(&format!(
            "Remedy: restore the previous ACCOUNTS_SIGNING_KEK, or DROP TABLE \
             {schema}.signing_keys (invalidates issued tokens)."
        )),
        "{err}"
    );
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

/// Without a KEK from register, migrate requires the variable's value and names it; a KEK
/// register parsed wins over it.
#[test]
fn migrate_without_a_kek_fails_naming_the_variable() {
    let err = Accounts::new().migrate_kek(None).unwrap_err().to_string();
    assert!(err.contains("ACCOUNTS_SIGNING_KEK is required"), "{err}");
    assert!(Accounts::with_signing_kek(dev_kek()).migrate_kek(None).is_ok());
}

/// First boot inserts one active key whose JWK is the opened private key's public half and
/// whose kid is its RFC 7638 thumbprint; a second boot adds none.
#[tokio::test(flavor = "multi_thread")]
async fn first_boot_inserts_one_active_key_and_a_second_boot_adds_none() {
    let Some(pool) = test_pool().await else { return };
    let schema = scratch_schema(&pool, "boot").await;
    let first = migrate_signing_keys(&pool, &schema, &dev_kek()).await;
    let after_first = key_rows(&pool, &schema).await;
    let second = migrate_signing_keys(&pool, &schema, &dev_kek()).await;
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

/// Waits until `want` INSERTs into `{schema}.signing_keys` queue behind a held table lock.
async fn wait_for_parked_inserts(pool: &PgPool, schema: &str, want: i64) {
    let parked = async {
        loop {
            let (n,): (i64,) = sqlx::query_as(
                "SELECT count(*) FROM pg_locks \
                 WHERE relation = $1::regclass AND mode = 'RowExclusiveLock' AND NOT granted",
            )
            .bind(format!("{schema}.signing_keys"))
            .fetch_one(pool)
            .await
            .unwrap();
            if n == want {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(60), parked)
        .await
        .expect("the boots never queued their INSERTs behind the held lock");
}

/// Two boots under different KEKs both read a table without an active key, and a held SHARE
/// lock parks both INSERTs until both have read. The boot whose insert lands second inserts
/// nothing, re-reads and fails to open the winner's key: one boot fails with the remedy and
/// the winner's key is the only row.
#[tokio::test(flavor = "multi_thread")]
async fn a_boot_that_loses_the_insert_race_opens_the_winners_key() {
    let Some(pool) = test_pool().await else { return };
    let schema = booted_schema(&pool, "race").await;
    sqlx::query(&format!("DELETE FROM {schema}.signing_keys")).execute(&pool).await.unwrap();
    let mut gate = pool.begin().await.unwrap();
    sqlx::query(&format!("LOCK TABLE {schema}.signing_keys IN SHARE MODE"))
        .execute(&mut *gate)
        .await
        .unwrap();
    let boot = |kek: SigningKek| {
        let (pool, schema) = (pool.clone(), schema.clone());
        tokio::spawn(async move { migrate_signing_keys(&pool, &schema, &kek).await })
    };
    let (dev, other) = (boot(dev_kek()), boot(other_kek()));
    wait_for_parked_inserts(&pool, &schema, 2).await;
    gate.commit().await.unwrap();
    let outcomes = [(dev_kek(), dev.await.unwrap()), (other_kek(), other.await.unwrap())];
    let after = key_rows(&pool, &schema).await;
    drop_schema(&pool, &schema).await;

    let [(kid, status, _, sealed, nonce)] = &after[..] else {
        panic!("the race left {} rows: {outcomes:?}", after.len())
    };
    assert_eq!(status, "active");
    let (winners, losers): (Vec<_>, Vec<_>) = outcomes.into_iter().partition(|(_, o)| o.is_ok());
    let ([(winner, _)], [(_, lost)]) = (&winners[..], &losers[..]) else {
        panic!("want one boot to win and one to fail: {winners:?} {losers:?}")
    };
    assert!(open(winner, kid, nonce, sealed).is_some(), "the row is not the winner's key");
    let err = format!("{:#}", lost.as_ref().unwrap_err());
    assert!(err.contains(&format!("signing key {kid} in {schema}.signing_keys")), "{err}");
    assert!(err.contains("Remedy: restore the previous ACCOUNTS_SIGNING_KEK"), "{err}");
}

/// A KEK that did not seal the active key fails boot with the remedy and writes no row.
#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_kek_fails_boot_with_the_remedy_and_writes_no_row() {
    let Some(pool) = test_pool().await else { return };
    let schema = booted_schema(&pool, "wrongkek").await;
    let before = key_rows(&pool, &schema).await;
    let refused = migrate_signing_keys(&pool, &schema, &other_kek()).await;
    let after = key_rows(&pool, &schema).await;
    drop_schema(&pool, &schema).await;

    assert_open_remedy(refused, &schema, &before[0].0);
    assert_eq!(before, after);
}

/// A table whose only key is retiring, under a KEK that did not seal it, fails boot before
/// any insert: a new active key sealed under the wrong KEK would stop the restored KEK from
/// booting.
#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_kek_over_only_a_retiring_key_fails_before_inserting() {
    let Some(pool) = test_pool().await else { return };
    let schema = booted_schema(&pool, "retiringonly").await;
    sqlx::query(&format!("UPDATE {schema}.signing_keys SET status = 'retiring'"))
        .execute(&pool)
        .await
        .unwrap();
    let before = key_rows(&pool, &schema).await;
    let refused = migrate_signing_keys(&pool, &schema, &other_kek()).await;
    let after = key_rows(&pool, &schema).await;
    drop_schema(&pool, &schema).await;

    assert_open_remedy(refused, &schema, &before[0].0);
    assert_eq!(before, after);
}

/// A key that decrypts under the KEK but is not PKCS#8 fails boot with the remedy.
#[tokio::test(flavor = "multi_thread")]
async fn a_key_that_decrypts_but_is_not_pkcs8_fails_boot() {
    let Some(pool) = test_pool().await else { return };
    let schema = booted_schema(&pool, "corrupt").await;
    let kid = key_rows(&pool, &schema).await.remove(0).0;
    let (nonce, sealed) = seal(&dev_kek(), &kid, b"not pkcs8").unwrap();
    sqlx::query(&format!("UPDATE {schema}.signing_keys SET sealed_key = $1, nonce = $2"))
        .bind(&sealed)
        .bind(&nonce)
        .execute(&pool)
        .await
        .unwrap();
    let before = key_rows(&pool, &schema).await;
    let refused = migrate_signing_keys(&pool, &schema, &dev_kek()).await;
    let after = key_rows(&pool, &schema).await;
    drop_schema(&pool, &schema).await;

    assert_open_remedy(refused, &schema, &kid);
    assert_eq!(before, after);
}

/// A retiring key sealed under another KEK fails boot even beside an active key that opens.
#[tokio::test(flavor = "multi_thread")]
async fn a_retiring_key_under_another_kek_fails_boot() {
    let Some(pool) = test_pool().await else { return };
    let schema = booted_schema(&pool, "staleretiring").await;
    let stale = NewKey::generate(&other_kek()).unwrap();
    sqlx::query(&format!(
        "INSERT INTO {schema}.signing_keys (kid, alg, sealed_key, nonce, public_jwk, status) \
         VALUES ($1, 'ES256', $2, $3, $4::jsonb, 'retiring')"
    ))
    .bind(&stale.kid)
    .bind(&stale.sealed_key)
    .bind(&stale.nonce)
    .bind(stale.public_jwk.to_string())
    .execute(&pool)
    .await
    .unwrap();
    let before = key_rows(&pool, &schema).await;
    let refused = migrate_signing_keys(&pool, &schema, &dev_kek()).await;
    let after = key_rows(&pool, &schema).await;
    drop_schema(&pool, &schema).await;

    assert_open_remedy(refused, &schema, &stale.kid);
    assert_eq!(before, after);
}

/// A table of another shape, missing columns or carrying an extra one, is refused with the
/// drop remedy before the DDL or the insert touch it.
#[tokio::test(flavor = "multi_thread")]
async fn a_signing_keys_table_of_another_shape_is_refused_with_the_drop_remedy() {
    let Some(pool) = test_pool().await else { return };
    let shapes = [
        ("narrow", "kid text PRIMARY KEY, private_pem text NOT NULL"),
        (
            "wide",
            "kid text PRIMARY KEY, alg text NOT NULL, sealed_key bytea NOT NULL, \
             nonce bytea NOT NULL, public_jwk jsonb NOT NULL, status text NOT NULL, \
             created_at timestamptz NOT NULL DEFAULT now(), rotated_by text",
        ),
    ];
    for (label, columns) in shapes {
        let schema = scratch_schema(&pool, label).await;
        sqlx::query(&format!("CREATE TABLE {schema}.signing_keys ({columns})"))
            .execute(&pool)
            .await
            .unwrap();
        let refused = migrate_signing_keys(&pool, &schema, &dev_kek()).await;
        let (rows,): (i64,) =
            sqlx::query_as(&format!("SELECT count(*) FROM {schema}.signing_keys"))
                .fetch_one(&pool)
                .await
                .unwrap();
        drop_schema(&pool, &schema).await;

        let err = format!("{:#}", refused.expect_err("a stale signing_keys must refuse boot"));
        assert!(
            err.contains(&format!(
                "has another shape than this build creates. Remedy: DROP TABLE \
                 {schema}.signing_keys; and reboot"
            )),
            "{label}: {err}"
        );
        assert_eq!(rows, 0, "{label}");
    }
}
