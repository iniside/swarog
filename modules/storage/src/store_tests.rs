use opsapi::Status;
use storageapi::{Player, ACL_PRIVATE, MAX_OBJECTS_PER_OWNER};

use crate::service_tests::{rejected, Fixture, DB_LOCK};
use crate::store::{write_error, COLUMN_CAPS};

#[tokio::test]
async fn column_caps_match_live_checks_and_reverse_mapping_excludes_normalized_value() {
    let _lock = DB_LOCK.lock().await;
    let f = Fixture::new().await;
    assert_eq!(COLUMN_CAPS.len(), 2);
    assert!(!COLUMN_CAPS.iter().any(|cap| cap.what == "value"));
    for cap in COLUMN_CAPS {
        let definition: String = sqlx::query_scalar(
            "SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conrelid = 'storage.objects'::regclass AND conname = $1",
        ).bind(cap.constraint).fetch_one(f.pool()).await.unwrap();
        assert!(definition.contains(&format!("octet_length({})", cap.what)), "{definition}");
        assert!(definition.contains(&format!("<= {}", cap.max_bytes)), "{definition}");
    }
    let checks: Vec<String> = sqlx::query_scalar(
        "SELECT conname::text FROM pg_constraint WHERE conrelid = 'storage.objects'::regclass AND contype = 'c' AND conname LIKE '%_len_check' ORDER BY conname",
    ).fetch_all(f.pool()).await.unwrap();
    assert!(!checks.is_empty());
    for name in checks {
        let (collection, key, value, expected) = if let Some(cap) = COLUMN_CAPS.iter().find(|cap| cap.constraint == name) {
            let oversized = "x".repeat(cap.max_bytes + 1);
            let (collection, key) = match cap.what {
                "collection" => (oversized, "check".into()),
                "key" => ("saves".into(), oversized),
                other => panic!("missing CHECK fixture for {other}"),
            };
            let service_error = crate::service::validate_address(&collection, &key).unwrap_err();
            (collection, key, "{}".into(), service_error.msg)
        } else if name == "storage_value_len_check" {
            ("saves".into(), "check".into(), format!("[{}]", vec!["1e100"; 1400].join(",")), "value is too large once normalized as json".into())
        } else {
            panic!("uncovered write_error length CHECK {name}");
        };
        let mut tx = f.pool().begin().await.unwrap();
        let err = f.svc.store.insert_object_tx(&mut tx, &f.owners[0], &collection, &key, &value, ACL_PRIVATE).await.unwrap_err();
        let db = err.as_database_error().unwrap();
        assert_eq!(db.code().as_deref(), Some("23514"));
        assert_eq!(db.constraint(), Some(name.as_str()));
        let mapped = write_error(err);
        assert_eq!(mapped.status, Status::Invalid);
        assert_eq!(mapped.msg, expected);
        tx.rollback().await.unwrap();
    }
    f.cleanup().await;
}

#[tokio::test]
async fn unmapped_acl_check_is_internal_and_bad_requested_uuid_is_none() {
    let _lock = DB_LOCK.lock().await;
    let f = Fixture::new().await;
    let mut tx = f.pool().begin().await.unwrap();
    let err = f.svc.store.insert_object_tx(&mut tx, &f.owners[0], "saves", "key", "{}", "invalid").await.unwrap_err();
    assert_eq!(err.as_database_error().unwrap().constraint(), Some("storage_acl_check"));
    assert_eq!(write_error(err).status, Status::Internal);
    tx.rollback().await.unwrap();
    assert!(f.svc.store.read_visible_object("bad-uuid", "saves", "key", &f.owners[0]).await.unwrap().is_none());
    assert_eq!(write_error(sqlx::Error::PoolClosed).status, Status::Internal);
    f.cleanup().await;
}

#[tokio::test]
async fn owner_cap_accepts_last_slot_rejects_next_and_does_not_block_cas() {
    let _lock = DB_LOCK.lock().await;
    let f = Fixture::new().await;
    f.put(0, "seed", "{}", 0, "").await.unwrap();
    let seeded = sqlx::query(
        "INSERT INTO storage.objects (owner_id, collection, key, value) SELECT $1::uuid, 'saves', 'bulk-' || n::text, '{}'::jsonb FROM generate_series(1, $2::bigint) n",
    ).bind(&f.owners[0]).bind(MAX_OBJECTS_PER_OWNER - 2).execute(f.pool()).await.unwrap();
    assert_eq!(seeded.rows_affected() as i64, MAX_OBJECTS_PER_OWNER - 2);
    assert_eq!(f.put(0, "last", "{}", 0, "").await.unwrap().version, 1);
    rejected(f.put(0, "extra", "{}", 0, "").await, Status::Conflict);
    assert_eq!(f.put(0, "last", "1", 1, "").await.unwrap().version, 2);
    assert_eq!(f.put(1, "separate-owner", "{}", 0, "").await.unwrap().version, 1);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM storage.objects WHERE owner_id = $1::uuid")
        .bind(&f.owners[0]).fetch_one(f.pool()).await.unwrap();
    assert_eq!(count, MAX_OBJECTS_PER_OWNER);
    f.svc.delete(f.id(0), "saves".into(), "last".into()).await.unwrap();
    assert_eq!(f.put(0, "replacement", "{}", 0, "").await.unwrap().version, 1);
    f.cleanup().await;
}

#[tokio::test]
async fn concurrent_cas_rechecks_version_after_waiting_on_the_row_lock() {
    let _lock = DB_LOCK.lock().await;
    let f = Fixture::new().await;
    f.put(0, "race", "0", 0, "").await.unwrap();
    let mut blocker = f.pool().begin().await.unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker).await.unwrap();
    sqlx::query("SELECT 1 FROM storage.objects WHERE owner_id = $1::uuid FOR UPDATE")
        .bind(&f.owners[0]).execute(&mut *blocker).await.unwrap();
    let first = f.put(0, "race", "1", 1, "");
    let second = f.put(0, "race", "2", 1, "");
    let unlock = async {
        // Both CAS statements reach the locked row before either can publish a new version.
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let waiting: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() AND query LIKE '%WITH upd AS (%' AND cardinality(pg_blocking_pids(pid)) > 0 AND pid <> pg_backend_pid()",
                ).fetch_one(f.pool()).await.unwrap();
                if waiting == 2 { break; }
                tokio::task::yield_now().await;
            }
        }).await.expect("both CAS calls must wait on the held row");
        let holds: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_locks WHERE pid = $1 AND granted)")
            .bind(pid).fetch_one(f.pool()).await.unwrap();
        assert!(holds);
        blocker.commit().await.unwrap();
    };
    let (first, second, ()) = tokio::join!(first, second, unlock);
    let winner = match (first, second) {
        (Ok(version), Err(err)) => { assert_eq!(version.version, 2); assert_eq!(err.status, Status::Conflict); "1" }
        (Err(err), Ok(version)) => { assert_eq!(version.version, 2); assert_eq!(err.status, Status::Conflict); "2" }
        other => panic!("exactly one CAS must win: {other:?}"),
    };
    let actual = f.get(0, "race", "").await.unwrap();
    assert_eq!((actual.version, actual.value.as_str()), (2, winner));
    f.cleanup().await;
}
