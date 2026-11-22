use opsapi::Status;
use storageapi::{Player, ACL_PRIVATE, MAX_OBJECTS_PER_OWNER};

use crate::service::{decode_admin_cursor, encode_admin_cursor};
use crate::service_tests::{rejected, Fixture, DB_LOCK};
use crate::store::{age_secs_sql, ago_text_sql, write_error, AdminObjectRow, COLUMN_CAPS};

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

/// Plants `(collection, key, updated_at)` rows for owner `n` with a fixed `updated_at`,
/// so the order under test never depends on when the statement ran.
async fn plant(f: &Fixture, n: usize, rows: &[(&str, &str, &str)]) {
    for (collection, key, at) in rows {
        sqlx::query(
            "INSERT INTO storage.objects (owner_id, collection, key, value, updated_at) \
             VALUES ($1::uuid, $2, $3, '{}'::jsonb, $4::timestamptz)",
        )
        .bind(&f.owners[n]).bind(collection).bind(key).bind(at)
        .execute(f.pool()).await.unwrap();
    }
}

/// Walks every page through the opaque cursor, as the tab will.
async fn walk(f: &Fixture, owner: &str, limit: usize) -> (Vec<AdminObjectRow>, usize) {
    let (mut all, mut pages, mut cursor) = (Vec::new(), 0, String::new());
    loop {
        let after = decode_admin_cursor(&cursor).unwrap();
        let after = after.as_ref().map(|(u, c, k)| (u.as_str(), c.as_str(), k.as_str()));
        let page = f.svc.store.admin_page(owner, after, limit).await.unwrap();
        assert!(page.rows.len() <= limit);
        pages += 1;
        all.extend(page.rows);
        match page.next_cursor {
            Some(next) => cursor = next,
            None => return (all, pages),
        }
    }
}

#[tokio::test]
async fn admin_page_walks_keyset_pages_through_ties_and_never_shows_another_owner() {
    let _lock = DB_LOCK.lock().await;
    let f = Fixture::new().await;
    let tie = "2020-01-03 00:00:00.000001+00";
    plant(&f, 0, &[
        ("b", "k1", tie), ("a", "k2", tie), ("a", "k1", tie), ("b", "k0", tie),
        ("a", "old", "2020-01-01 00:00:00+00"), ("c", "new", "2020-01-04 00:00:00+00"),
        ("a", "mid", "2020-01-02 12:00:00.5+00"),
    ]).await;
    // Another owner's rows sit newest and between the first owner's, sharing the tie.
    plant(&f, 1, &[("a", "k0", tie), ("a", "zz", "2020-01-05 00:00:00+00")]).await;
    let expected = [
        ("c", "new"), ("a", "k1"), ("a", "k2"), ("b", "k0"), ("b", "k1"), ("a", "mid"), ("a", "old"),
    ];
    let keys = |rows: &[AdminObjectRow]| -> Vec<(String, String)> {
        rows.iter().map(|r| (r.collection.clone(), r.key.clone())).collect()
    };
    let want: Vec<(String, String)> = expected.iter().map(|(c, k)| (c.to_string(), k.to_string())).collect();
    // 7 rows by 3: two full pages and a partial one, the tie cut across the first boundary.
    let (rows, pages) = walk(&f, &f.owners[0], 3).await;
    assert_eq!(keys(&rows), want);
    assert_eq!(pages, 3);
    assert_eq!(rows[1].updated_at, "2020-01-03T00:00:00.000001Z");
    assert!(rows.iter().all(|r| r.version == 1 && r.updated_ago.ends_with(" d ago")));
    // A full last page hands out no cursor: 7 rows by 7 is one page.
    let (rows, pages) = walk(&f, &f.owners[0], 7).await;
    assert_eq!((keys(&rows), pages), (want, 1));
    // The other owner sees only its own two rows, newest first.
    let (rows, _) = walk(&f, &f.owners[1], 25).await;
    assert_eq!(keys(&rows), vec![("a".to_string(), "zz".to_string()), ("a".to_string(), "k0".to_string())]);
    let empty = f.svc.store.admin_page("not-a-uuid", None, 25).await.unwrap();
    assert!(empty.rows.is_empty() && empty.next_cursor.is_none());
    f.cleanup().await;
}

#[tokio::test]
async fn admin_page_checksum_and_size_describe_the_read_back_text_and_used_bytes_sums_them() {
    use sha2::{Digest, Sha256};
    let _lock = DB_LOCK.lock().await;
    let f = Fixture::new().await;
    let before = f.svc.store.admin_used_bytes().await.unwrap();
    let sent = r#"{"b": [1, 2.50],   "a": "zażółć \/ x"}"#;
    f.put(0, "one", sent, 0, "").await.unwrap();
    f.put(0, "two", "[true, null, 1e3]", 0, "").await.unwrap();
    let page = f.svc.store.admin_page(&f.owners[0], None, 25).await.unwrap();
    assert_eq!(page.rows.len(), 2);
    let mut total = 0;
    for row in &page.rows {
        let text = f.get(0, &row.key, &f.owners[0]).await.unwrap().value;
        let hex: String = Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(row.checksum, format!("sha256:{hex}"), "{text}");
        assert_eq!(row.bytes, text.len() as i64, "{text}");
        assert_eq!(row.collection, "saves");
        assert!(row.updated_ago.ends_with(" s ago") || row.updated_ago.ends_with(" min ago"));
        total += row.bytes;
    }
    // The normalized text differs from what was sent, so the size is not the caller's.
    assert_ne!(page.rows.iter().find(|r| r.key == "one").unwrap().bytes, sent.len() as i64);
    assert_eq!(f.svc.store.admin_used_bytes().await.unwrap() - before, total);
    f.cleanup().await;
}

#[tokio::test]
async fn ago_text_buckets_fixed_instants_and_clamps_the_future() {
    let _lock = DB_LOCK.lock().await;
    let f = Fixture::new().await;
    let sql = format!(
        "SELECT {} FROM (SELECT {} AS s) a",
        ago_text_sql("a.s"),
        age_secs_sql("$2::timestamptz", "$1::timestamptz"),
    );
    let now = "2026-01-10 12:00:00+00";
    for (at, want) in [
        ("2026-01-10 12:00:00+00", "0 s ago"),
        ("2026-01-10 11:59:00.1+00", "59 s ago"),
        ("2026-01-10 11:59:00+00", "1 min ago"),
        ("2026-01-10 11:00:01+00", "59 min ago"),
        ("2026-01-10 11:00:00+00", "1 h ago"),
        ("2026-01-09 12:00:01+00", "23 h ago"),
        ("2026-01-09 12:00:00+00", "1 d ago"),
        ("2026-01-07 11:59:55+00", "3 d ago"),
        ("2026-01-10 12:00:10+00", "0 s ago"),
    ] {
        let got: String = sqlx::query_scalar(&sql).bind(now).bind(at).fetch_one(f.pool()).await.unwrap();
        assert_eq!(got, want, "{at}");
    }
    f.cleanup().await;
}

#[test]
fn admin_cursor_round_trips_and_rejects_what_cannot_reach_a_bind() {
    let at = "2026-01-10T12:00:00.000001Z";
    let enc = encode_admin_cursor(at, "saves", "slot-1");
    assert_eq!(
        decode_admin_cursor(&enc).unwrap(),
        Some((at.to_string(), "saves".to_string(), "slot-1".to_string())),
    );
    assert_eq!(decode_admin_cursor("").unwrap(), None);
    let b64 = |s: &str| {
        use base64::Engine;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s)
    };
    for bad in [
        "!!!".to_string(),
        b64(&format!("{at}|saves")),
        b64("2026-02-30T12:00:00.000001Z|saves|k"),
        b64(&format!("{at}|sa/ves|k")),
        b64(&format!("{at}|saves|k|extra")),
        b64(&format!("{at}|{}|k", "c".repeat(65))),
        b64(&format!("{at}|saves|")),
    ] {
        assert_eq!(decode_admin_cursor(&bad).unwrap_err().status, Status::Invalid, "{bad}");
    }
}
