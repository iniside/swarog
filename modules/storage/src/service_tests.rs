use base64::Engine;
use opsapi::{Error, Identity, Status};
use sqlx::PgPool;
use storageapi::{Player, ACL_PRIVATE, ACL_PUBLIC_READ, MAX_COLLECTION_BYTES, MAX_KEY_BYTES, MAX_VALUE_BYTES};

use crate::service::{self, Service};

pub(crate) static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) struct Fixture {
    pub(crate) svc: Service,
    pub(crate) owners: Vec<String>,
}

impl Fixture {
    pub(crate) async fn new() -> Self {
        assert!(!testdb::skip_allowed(), "storage tests require strict testdb");
        let pool = testdb::test_pool().await.expect("live Postgres required");
        let svc = Service::new(pool);
        svc.store.migrate().await.unwrap();
        let owners = sqlx::query_scalar("SELECT gen_random_uuid()::text FROM generate_series(1, 2)")
            .fetch_all(&svc.store.pool).await.unwrap();
        Self { svc, owners }
    }

    pub(crate) fn pool(&self) -> &PgPool { &self.svc.store.pool }
    pub(crate) fn id(&self, n: usize) -> Identity { Identity::player(&self.owners[n]) }

    pub(crate) async fn put(&self, n: usize, key: &str, value: &str, version: i64, acl: &str)
        -> Result<storageapi::ObjectVersion, Error>
    {
        self.svc.put(self.id(n), "saves".into(), key.into(), value.into(), version, acl.into()).await
    }

    pub(crate) async fn get(&self, n: usize, key: &str, owner: &str)
        -> Result<storageapi::StorageObject, Error>
    {
        self.svc.get(self.id(n), "saves".into(), key.into(), owner.into()).await
    }

    pub(crate) async fn cleanup(self) {
        for owner in &self.owners {
            sqlx::query("DELETE FROM storage.objects WHERE owner_id = $1::uuid")
                .bind(owner).execute(self.pool()).await.unwrap();
            sqlx::query("DELETE FROM storage.purged_players WHERE player_id = $1::uuid")
                .bind(owner).execute(self.pool()).await.unwrap();
        }
        self.pool().close().await;
    }
}

pub(crate) fn rejected<T: std::fmt::Debug>(result: Result<T, Error>, status: Status) -> Error {
    let err = result.unwrap_err();
    assert_eq!(err.status, status, "{}", err.msg);
    err
}

#[tokio::test]
async fn private_visibility_uses_database_uuid_equality_and_hides_absence() {
    let _lock = DB_LOCK.lock().await;
    let f = Fixture::new().await;
    f.put(0, "private", "{}", 0, "").await.unwrap();
    let own = f.get(0, "private", "").await.unwrap();
    assert_eq!(own.acl, ACL_PRIVATE);
    for owner in [&f.owners[0], &f.owners[0].to_uppercase()] {
        assert_eq!(f.get(0, "private", owner).await.unwrap(), own);
    }
    for (key, owner) in [("private", f.owners[0].as_str()), ("missing", f.owners[0].as_str()), ("private", "invalid-uuid")] {
        let err = rejected(f.get(1, key, owner).await, Status::NotFound);
        assert_eq!(err.msg, service::NOT_FOUND);
    }
    f.cleanup().await;
}

#[tokio::test]
async fn public_read_does_not_authorize_cas_delete_or_listing() {
    let _lock = DB_LOCK.lock().await;
    let f = Fixture::new().await;
    f.put(0, "public", "{\"original\":true}", 0, ACL_PUBLIC_READ).await.unwrap();
    let original = f.get(1, "public", &f.owners[0]).await.unwrap();
    rejected(f.put(1, "public", "null", 1, "").await, Status::NotFound);
    rejected(f.svc.delete(f.id(1), "saves".into(), "public".into()).await, Status::NotFound);
    assert!(f.svc.list(f.id(1), "saves".into(), "".into(), 100).await.unwrap().items.is_empty());
    assert_eq!(f.get(0, "public", "").await.unwrap(), original);
    f.cleanup().await;
}

#[tokio::test]
async fn cas_preserves_create_conflicts_and_monotone_versions_when_restoring_values() {
    let _lock = DB_LOCK.lock().await;
    let f = Fixture::new().await;
    assert_eq!(f.put(0, "save", "1", 0, ACL_PUBLIC_READ).await.unwrap().version, 1);
    rejected(f.put(0, "save", "99", 0, "").await, Status::Conflict);
    assert_eq!(f.get(0, "save", "").await.unwrap().value, "1");
    assert_eq!(f.put(0, "save", "2", 1, "").await.unwrap().version, 2);
    rejected(f.put(0, "save", "99", 1, ACL_PRIVATE).await, Status::Conflict);
    assert_eq!(f.put(0, "save", "1", 2, "").await.unwrap().version, 3);
    let restored = f.get(0, "save", "").await.unwrap();
    assert_eq!((restored.value.as_str(), restored.version, restored.acl.as_str()), ("1", 3, ACL_PUBLIC_READ));
    rejected(f.put(0, "missing", "null", 5, "").await, Status::NotFound);
    f.cleanup().await;
}

#[tokio::test]
async fn delete_recreate_resets_version_and_accepts_the_documented_version_one_aba() {
    let _lock = DB_LOCK.lock().await;
    let f = Fixture::new().await;
    f.put(0, "save", "1", 0, "").await.unwrap();
    f.put(0, "save", "2", 1, "").await.unwrap();
    f.svc.delete(f.id(0), "saves".into(), "save".into()).await.unwrap();
    rejected(f.svc.delete(f.id(0), "saves".into(), "save".into()).await, Status::NotFound);
    assert_eq!(f.put(0, "save", "99", 0, "").await.unwrap().version, 1);
    rejected(f.put(0, "save", "88", 2, "").await, Status::Conflict);
    assert_eq!(f.put(0, "save", "77", 1, "").await.unwrap().version, 2);
    assert_eq!(f.get(0, "save", "").await.unwrap().value, "77");
    f.cleanup().await;
}

#[tokio::test]
async fn versions_above_i32_survive_put_get_and_list() {
    let _lock = DB_LOCK.lock().await;
    let f = Fixture::new().await;
    f.put(0, "save", "0", 0, "").await.unwrap();
    let version = i64::from(i32::MAX) + 1;
    sqlx::query("UPDATE storage.objects SET version = $2 WHERE owner_id = $1::uuid")
        .bind(&f.owners[0]).bind(version).execute(f.pool()).await.unwrap();
    assert_eq!(f.put(0, "save", "1", version, "").await.unwrap().version, version + 1);
    assert_eq!(f.get(0, "save", "").await.unwrap().version, version + 1);
    assert_eq!(f.svc.list(f.id(0), "saves".into(), "".into(), 1).await.unwrap().items[0].version, version + 1);
    f.cleanup().await;
}

#[tokio::test]
async fn validation_rejects_before_touching_a_closed_pool() {
    let pool = sqlx::postgres::PgPoolOptions::new().connect_lazy(testdb::DEFAULT_DSN).unwrap();
    pool.close().await;
    let svc = Service::new(pool);
    let id = Identity::player("aabbccdd-1234-5678-90ab-aabbccddeeff");
    for (collection, key, value, version, acl) in [
        ("saves".into(), "save".into(), "{".into(), 0, ""),
        ("saves".into(), "save".into(), "null".into(), -1, ""),
        ("saves".into(), "save".into(), "null".into(), 0, "public_write"),
        ("a".repeat(MAX_COLLECTION_BYTES + 1), "save".into(), "null".into(), 0, ""),
        ("saves".into(), "a".repeat(MAX_KEY_BYTES + 1), "null".into(), 0, ""),
        ("saves".into(), "save".into(), format!("\"{}\"", "a".repeat(MAX_VALUE_BYTES)), 0, ""),
    ] {
        rejected(svc.put(id.clone(), collection, key, value, version, acl.into()).await, Status::Invalid);
    }
    for illegal in ["", "a/b", "a%b", "a|b", "ą", "a b"] {
        rejected(svc.put(id.clone(), "saves".into(), illegal.into(), "null".into(), 0, "".into()).await, Status::Invalid);
        rejected(svc.put(id.clone(), illegal.into(), "save".into(), "null".into(), 0, "".into()).await, Status::Invalid);
        rejected(svc.get(id.clone(), "saves".into(), illegal.into(), "".into()).await, Status::Invalid);
        rejected(svc.delete(id.clone(), "saves".into(), illegal.into()).await, Status::Invalid);
        rejected(svc.list(id.clone(), illegal.into(), "".into(), 1).await, Status::Invalid);
    }
}

#[tokio::test]
async fn all_operations_classify_absent_and_malformed_caller_before_sql() {
    let pool = sqlx::postgres::PgPoolOptions::new().connect_lazy(testdb::DEFAULT_DSN).unwrap();
    pool.close().await;
    let svc = Service::new(pool);
    for (id, status) in [(Identity::none(), Status::Unauthorized), (Identity::player("dev-player"), Status::Internal)] {
        rejected(svc.put(id.clone(), "saves".into(), "key".into(), "{}".into(), 0, "".into()).await, status);
        rejected(svc.get(id.clone(), "saves".into(), "key".into(), "".into()).await, status);
        rejected(svc.delete(id.clone(), "saves".into(), "key".into()).await, status);
        rejected(svc.list(id, "saves".into(), "".into(), 1).await, status);
    }
}

#[tokio::test]
async fn exact_input_caps_are_accepted_and_jsonb_normalization_is_visible() {
    let _lock = DB_LOCK.lock().await;
    let f = Fixture::new().await;
    let collection = "a".repeat(MAX_COLLECTION_BYTES);
    let key = "z".repeat(MAX_KEY_BYTES);
    let value = format!("\"{}\"", "x".repeat(MAX_VALUE_BYTES - 2));
    f.svc.put(f.id(0), collection.clone(), key.clone(), value.clone(), 0, "".into()).await.unwrap();
    assert_eq!(f.svc.get(f.id(0), collection, key, "".into()).await.unwrap().value, value);
    let original = " { \"z\" : 1e2, \"a\" : \"\\u0061\" } ";
    f.put(0, "normalized", original, 0, "").await.unwrap();
    let actual = f.get(0, "normalized", "").await.unwrap().value;
    assert_ne!(actual, original);
    assert_eq!(serde_json::from_str::<serde_json::Value>(&actual).unwrap(), serde_json::json!({"a": "a", "z": 100}));
    f.cleanup().await;
}

#[tokio::test]
async fn normalized_values_may_exceed_input_cap_but_backstop_errors_are_invalid() {
    let _lock = DB_LOCK.lock().await;
    let f = Fixture::new().await;
    let expanded = format!("[{}]", vec!["1e100"; 700].join(","));
    assert!(expanded.len() < MAX_VALUE_BYTES);
    f.put(0, "expanded", &expanded, 0, "").await.unwrap();
    assert!(f.get(0, "expanded", "").await.unwrap().value.len() > MAX_VALUE_BYTES);
    let too_large = format!("[{}]", vec!["1e100"; 1400].join(","));
    assert!(too_large.len() < MAX_VALUE_BYTES);
    service::validate_value(&too_large).unwrap();
    let err = rejected(f.put(0, "expanded", &too_large, 1, "").await, Status::Invalid);
    assert_eq!(err.msg, "value is too large once normalized as json");
    let err = rejected(f.put(0, "oversized", &too_large, 0, "").await, Status::Invalid);
    assert_eq!(err.msg, "value is too large once normalized as json");
    assert_eq!(f.get(0, "expanded", "").await.unwrap().version, 1);
    f.cleanup().await;
}

#[test]
fn cursor_calendar_alphabet_and_cap_rejections_never_reset_to_first_page() {
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    assert_eq!(service::decode_cursor("").unwrap(), None);
    let time = "2024-02-29T23:59:59.123456Z";
    let encoded = service::encode_cursor(time, "save._-09AZaz");
    assert_eq!(service::decode_cursor(&encoded).unwrap(), Some((time.into(), "save._-09AZaz".into())));
    for raw in ["no-separator", "2026-02-30T00:00:00.000000Z|a", "1900-02-29T00:00:00.000000Z|a", "0000-01-01T00:00:00.000000Z|a", "2024-01-01T24:00:00.000000Z|a", "2024-01-01T00:60:00.000000Z|a", "2024-01-01T00:00:60.000000Z|a", "2024-01-01T00:00:00.000000Z|", "2024-01-01T00:00:00.000000Z|a/b", "2024-01-01T00:00:00.000000Z|a%b", "2024-01-01T00:00:00.000000Z|a|b"] {
        assert_eq!(rejected(service::decode_cursor(&b64.encode(raw)), Status::Invalid).msg, service::MALFORMED_CURSOR);
    }
    for encoded in ["%%%".into(), b64.encode([0xff]), format!("{encoded}="), service::encode_cursor(time, &"a".repeat(MAX_KEY_BYTES + 1))] {
        rejected(service::decode_cursor(&encoded), Status::Invalid);
    }
    assert_eq!(rejected(service::decode_cursor(&"%".repeat(storageapi::MAX_CURSOR_BYTES + 1)), Status::Invalid).msg, service::cursor_too_long_message());
}

#[test]
fn page_limit_has_unspecified_clamped_and_negative_branches() {
    assert_eq!(service::resolve_limit(0).unwrap(), storageapi::DEFAULT_PAGE_LIMIT);
    assert_eq!(service::resolve_limit(i64::MAX).unwrap(), storageapi::MAX_PAGE_LIMIT);
    assert_eq!(service::resolve_limit(1).unwrap(), 1);
    rejected(service::resolve_limit(-1), Status::Invalid);
}

#[tokio::test]
async fn tied_keysets_page_without_duplicates_and_cross_owner_cursor_replay_stays_scoped() {
    let _lock = DB_LOCK.lock().await;
    let f = Fixture::new().await;
    for n in 0..2 {
        for key in ["a", "b", "c"] { f.put(n, key, "{}", 0, ACL_PUBLIC_READ).await.unwrap(); }
        sqlx::query("UPDATE storage.objects SET created_at = '2026-01-01T00:00:00Z' WHERE owner_id = $1::uuid")
            .bind(&f.owners[n]).execute(f.pool()).await.unwrap();
    }
    f.svc.put(f.id(0), "other".into(), "excluded".into(), "{}".into(), 0, "".into()).await.unwrap();
    let first = f.svc.list(f.id(0), "saves".into(), "".into(), 2).await.unwrap();
    assert_eq!(first.items.iter().map(|v| v.key.as_str()).collect::<Vec<_>>(), ["c", "b"]);
    assert!(!first.next_cursor.is_empty());
    f.svc.delete(f.id(1), "saves".into(), "a".into()).await.unwrap();
    f.put(1, "aa", "{}", 0, "").await.unwrap();
    sqlx::query("UPDATE storage.objects SET created_at = '2026-01-01T00:00:00Z' WHERE owner_id = $1::uuid")
        .bind(&f.owners[1]).execute(f.pool()).await.unwrap();
    let second = f.svc.list(f.id(0), "saves".into(), first.next_cursor.clone(), 2).await.unwrap();
    assert_eq!(second.items.iter().map(|v| v.key.as_str()).collect::<Vec<_>>(), ["a"]);
    assert!(second.next_cursor.is_empty());
    let replay = f.svc.list(f.id(1), "saves".into(), first.next_cursor, 2).await.unwrap();
    assert_eq!(replay.items.iter().map(|v| v.key.as_str()).collect::<Vec<_>>(), ["aa"]);
    assert!(replay.next_cursor.is_empty());
    f.cleanup().await;
}
