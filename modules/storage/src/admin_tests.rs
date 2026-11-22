//! The control-plane provider: the size formatter, the Cloud saves tab's meta and body, the
//! tile, the purge attention item and the manifest; and, over a live Postgres, the tab's
//! keyset pages and totals through `Provider::panel`.

use std::sync::Arc;

use controlplane::{
    order, ActionPoint, Align, CellKind, DataTable, Emphasis, EntityRef, Palette, PanelBody, PanelQuery, Point,
    Primitive, Provider, Role, Then,
};
use opsapi::Status;
use storageapi::ACL_PRIVATE;

use crate::admin;
use crate::service::Service;
use crate::service_tests::{rejected, Fixture, DB_LOCK};
use crate::store::AdminObjectRow;

fn query(contribution: &str, kind: &str, id: &str, cursor: Option<&str>) -> PanelQuery {
    PanelQuery {
        module: "storage".into(),
        contribution: contribution.into(),
        entity: Some(EntityRef { kind: kind.into(), id: id.into() }),
        search: None,
        cursor: cursor.map(String::from),
        before: None,
        queue_state: None,
        range: None,
        params: Default::default(),
        refs: Vec::new(),
    }
}

fn table(body: &PanelBody) -> &DataTable {
    match &body.primitive {
        Primitive::DataTable(t) => t,
        other => panic!("the Cloud saves tab is a DataTable, got {other:?}"),
    }
}

fn object(collection: &str, key: &str, version: i64, bytes: i64) -> AdminObjectRow {
    AdminObjectRow {
        collection: collection.to_string(),
        key: key.to_string(),
        version,
        bytes,
        updated_at: "2026-03-04T10:00:00.000000Z".to_string(),
        updated_ago: "4 min ago".to_string(),
        checksum: format!("sha256:{}", "ab".repeat(32)),
    }
}

#[test]
fn format_bytes_is_decimal_with_one_place_and_never_shows_1000_of_a_unit() {
    let cases: [(i64, &str, &str); 14] = [
        (-5, "0", "B"),
        (0, "0", "B"),
        (999, "999", "B"),
        (1000, "1.0", "KB"),
        (1049, "1.0", "KB"),
        (1050, "1.1", "KB"),
        (18_400_000, "18.4", "MB"),
        (999_949, "999.9", "KB"),
        // Rounds to 1000.0 KB, so it moves up a unit.
        (999_950, "1.0", "MB"),
        (999_950_000, "1.0", "GB"),
        (1_820_000_000_000, "1.8", "TB"),
        (999_949_999_999_999, "999.9", "TB"),
        // TB is the last unit: past it the value grows instead.
        (1_234_000_000_000_000, "1234.0", "TB"),
        (i64::MAX, "9223372.0", "TB"),
    ];
    for (bytes, value, unit) in cases {
        assert_eq!(admin::format_bytes(bytes), (value.to_string(), unit), "{bytes}");
    }
    assert_eq!(admin::size_text(18_400_000), "18.4 MB");
}

#[test]
fn meta_counts_against_the_object_cap_with_the_owner_size() {
    assert_eq!(admin::meta_text(0, 0), "0 of 10,000 slots · 0 B");
    assert_eq!(admin::meta_text(1, 512), "1 of 10,000 slots · 512 B");
    assert_eq!(admin::meta_text(1234, 41_200_000), "1,234 of 10,000 slots · 41.2 MB");
}

#[test]
fn saves_body_maps_columns_cells_row_download_and_meta() {
    let rows = [object("saves", "autosave", 214, 18_400_000), object("settings", "ui", 1, 64)];
    let body = admin::saves_body(&rows, 2, 18_400_064, None);
    assert_eq!(body.meta.as_deref(), Some("2 of 10,000 slots · 18.4 MB"));
    let t = table(&body);
    let cols: Vec<_> = t.columns.iter().map(|c| (c.key.as_str(), c.label.as_str(), c.kind, c.align)).collect();
    assert_eq!(cols, vec![
        ("slot", "Slot", CellKind::Mono, Align::Left),
        ("size", "Size", CellKind::Bytes, Align::Right),
        ("updated", "Updated", CellKind::Text, Align::Left),
        ("version", "Version", CellKind::Ref, Align::Left),
        ("checksum", "Checksum", CellKind::MonoMuted, Align::Left),
    ]);
    // No sort arrow: the mockup draws none.
    assert!(t.columns.iter().all(|c| c.sorted.is_none() && !c.sortable));
    assert_eq!((t.page.total, t.page.next_cursor.as_deref(), t.page.size), (None, None, 25));
    assert!(t.filters.is_empty() && t.search_placeholder.is_none() && !t.selectable);
    let noun = t.item_noun.as_ref().expect("a counted noun");
    assert_eq!((noun.one.as_str(), noun.many.as_str()), ("slot", "slots"));
    let first = &t.rows[0];
    assert!(first.entity.is_none(), "rows are not clickable");
    assert_eq!(first.item.as_deref(), Some("saves/autosave"));
    assert_eq!(first.actions, vec![admin::DOWNLOAD.to_string()]);
    let cells: Vec<_> = first.cells.iter().map(|c| (c.kind, c.value.as_str())).collect();
    let checksum = format!("sha256:{}", "ab".repeat(32));
    assert_eq!(cells, vec![
        (CellKind::Mono, "saves/autosave"),
        (CellKind::Bytes, "18.4 MB"),
        (CellKind::Text, "4 min ago"),
        (CellKind::Ref, "v214"),
        (CellKind::MonoMuted, checksum.as_str()),
    ]);
    assert_eq!(t.rows[1].cells[1].value, "64 B");

    let next = admin::saves_body(&rows, 30, 0, Some("c".into()));
    assert_eq!(table(&next).page.next_cursor.as_deref(), Some("c"));
    assert_eq!(table(&next).page.total, None);
}

#[test]
fn tile_draws_the_byte_sum_the_cap_delta_and_a_stable_chart2_spark() {
    let body = admin::tile_body(1_820_000_000_000);
    let Primitive::KpiTile(tile) = &body.primitive else { panic!("a KPI tile, got {:?}", body.primitive) };
    assert_eq!((tile.label.as_str(), tile.value.as_str(), tile.unit.as_deref()), ("Cloud saves used", "1.8", Some("TB")));
    let delta = tile.delta.as_ref().expect("the cap delta");
    assert_eq!((delta.value.as_str(), delta.period.as_str(), delta.tone), ("", "10,000 slots / player", None));
    assert_eq!(tile.spark, Palette::Chart2);
    assert_eq!(tile.series.len(), 7);
    assert_eq!(tile.series, admin::fake_spark(), "the fake series is a pure function");
    assert!(tile.link.is_none() && tile.capacity.is_none());

    let zero = admin::tile_body(0);
    let Primitive::KpiTile(zero) = &zero.primitive else { panic!("a KPI tile") };
    assert_eq!((zero.value.as_str(), zero.unit.as_deref()), ("0", Some("B")));
}

#[test]
fn purges_body_is_empty_at_zero_and_one_item_otherwise() {
    let empty = admin::purges_body(0, "");
    assert!(matches!(&empty.primitive, Primitive::AttentionItems(items) if items.is_empty()));
    let some = admin::purges_body(2, "2026-01-02T03:04:05Z");
    match &some.primitive {
        Primitive::AttentionItems(items) => {
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].title, "Storage purges pending");
            assert!(items[0].text.contains("2 players"), "{}", items[0].text);
        }
        other => panic!("an attention list, got {other:?}"),
    }
}

#[test]
fn manifest_is_readonly_with_every_action_disabled_at_the_shared_orders() {
    let m = admin::manifest();
    assert!(!m.writes);
    assert!(m.entity_kinds.is_empty() && m.pages.is_empty() && m.search.is_empty());
    let find = |id: &str| m.contributions.iter().find(|c| c.id == id).expect(id);
    let tab = find(admin::SAVES);
    assert_eq!(tab.order, order::player_tab::STORAGE);
    assert!(matches!(&tab.point, Point::EntityTab { kind, tab, tab_label, params, .. }
        if kind == "player" && tab == "saves" && tab_label == "Cloud saves" && params.is_empty()));
    assert_eq!(tab.title, "Cloud saves");
    assert_eq!(
        tab.readonly.as_deref(),
        Some("Read-only: storage exposes saves for inspection and download; edits and deletes are not remotely supported."),
    );
    assert_eq!(tab.cache_ttl_s, None);
    let tile = find(admin::TILE_USED);
    assert_eq!((tile.order, &tile.point, tile.cache_ttl_s), (order::dashboard_tile::STORAGE, &Point::DashboardTile, Some(300)));
    let purges = find(admin::ATTENTION_PURGES);
    assert_eq!((purges.order, &purges.point), (order::dashboard_attention::STORAGE_PURGES, &Point::DashboardAttention));
    // No tab count: the mockup's tab draws none.
    assert!(!m.contributions.iter().any(|c| matches!(c.point, Point::EntityTabCount { .. })));
    assert!(m.contributions.iter().all(|c| c.min_role == Role::Viewer));
    assert!(m.contributions.iter().filter(|c| c.id != admin::SAVES).all(|c| c.readonly.is_none()));

    assert!(m.actions.iter().all(|a| !a.implemented && a.then == Then::Invoke && !a.prepare && a.min_role == Role::Operator));
    let shape: Vec<_> = m
        .actions
        .iter()
        .map(|a| (a.id.as_str(), &a.point, a.label.as_str(), a.icon.as_str(), a.order, a.danger, a.emphasis, a.icon_only))
        .collect();
    let saves = ActionPoint::PanelTool("saves".into());
    let row = ActionPoint::TableRow("saves".into());
    let player = ActionPoint::RowAction("player".into());
    assert_eq!(shape, vec![
        (admin::DOWNLOAD_ALL, &saves, "Download all", "download", 0, false, Emphasis::Default, false),
        (admin::DELETE, &saves, "Delete", "trash", 1, true, Emphasis::Default, false),
        (admin::DOWNLOAD, &row, "Download", "download", 0, false, Emphasis::Ghost, true),
        (
            admin::DOWNLOAD_PLAYER,
            &player,
            "Download cloud saves",
            "download",
            order::player_row_action::STORAGE,
            false,
            Emphasis::Default,
            false,
        ),
    ]);
    // `writes: false` refuses every action, the row menu's included.
    assert!(m.actions.iter().all(|a| m.write_refusal(&a.id).is_some()));
}

/// Over a live Postgres: 27 objects for one owner and one for another; the tab walks two
/// pages (25, then 2) newest write first, carries the owner's totals in its meta on both,
/// never shows the other owner's object, and refuses a malformed cursor and a malformed id.
#[tokio::test]
async fn saves_tab_pages_one_owner_newest_first_with_the_owner_totals() {
    let _lock = DB_LOCK.lock().await;
    let f = Fixture::new().await;
    let owner = f.owners[0].clone();
    for i in 0..27 {
        sqlx::query(
            "INSERT INTO storage.objects (owner_id, collection, key, value, updated_at) \
             VALUES ($1::uuid, 'saves', $2, '{\"n\": 1}'::jsonb, now() - make_interval(mins => $3))",
        )
        .bind(&owner)
        .bind(format!("slot-{i:02}"))
        .bind(i)
        .execute(f.pool())
        .await
        .unwrap();
    }
    f.put(1, "foreign", "{}", 0, ACL_PRIVATE).await.unwrap();
    let provider = admin::StorageAdmin::new(Arc::new(Service::new(f.pool().clone())));

    let first = provider.panel(query(admin::SAVES, "player", &owner, None)).await.unwrap();
    let bytes = 27 * "{\"n\": 1}".len();
    let meta = format!("27 of 10,000 slots · {bytes} B");
    assert_eq!(first.meta.as_deref(), Some(meta.as_str()));
    let t = table(&first);
    assert_eq!(t.rows.len(), 25);
    assert_eq!(t.rows[0].item.as_deref(), Some("saves/slot-00"));
    assert!(t.rows[0].cells[2].value.ends_with(" s ago"), "{}", t.rows[0].cells[2].value);
    assert_eq!(t.rows[1].cells[2].value, "1 min ago");
    assert_eq!(t.rows[0].cells[3].value, "v1");
    let cursor = t.page.next_cursor.clone().expect("a second page");

    let second = provider.panel(query(admin::SAVES, "player", &owner, Some(&cursor))).await.unwrap();
    assert_eq!(second.meta.as_deref(), Some(meta.as_str()));
    let t2 = table(&second);
    let items: Vec<_> = t2.rows.iter().map(|r| r.item.clone().unwrap()).collect();
    assert_eq!(items, vec!["saves/slot-25", "saves/slot-26"]);
    assert_eq!(t2.page.next_cursor, None);

    rejected(provider.panel(query(admin::SAVES, "player", &owner, Some("!!"))).await, Status::Invalid);
    rejected(provider.panel(query(admin::SAVES, "player", "not-a-uuid", None)).await, Status::Invalid);
    rejected(provider.panel(query(admin::SAVES, "group", &owner, None)).await, Status::Invalid);

    // An owner with nothing stored: no rows, a zero meta.
    let nobody: String = sqlx::query_scalar("SELECT gen_random_uuid()::text").fetch_one(f.pool()).await.unwrap();
    let empty = provider.panel(query(admin::SAVES, "player", &nobody, None)).await.unwrap();
    assert_eq!(empty.meta.as_deref(), Some("0 of 10,000 slots · 0 B"));
    assert!(table(&empty).rows.is_empty());

    assert_eq!(f.svc.store.admin_owner_totals("not-a-uuid").await.unwrap(), (0, 0));
    f.cleanup().await;
}

/// The tile and the purge item read the live tables through `Provider::panel`.
#[tokio::test]
async fn tile_and_purges_item_read_the_live_tables() {
    let _lock = DB_LOCK.lock().await;
    let f = Fixture::new().await;
    f.put(0, "slot", "{\"a\": 1}", 0, ACL_PRIVATE).await.unwrap();
    let provider = admin::StorageAdmin::new(Arc::new(Service::new(f.pool().clone())));
    let used = f.svc.store.admin_used_bytes().await.unwrap();
    assert!(used > 0);
    let tile = provider.panel(query(admin::TILE_USED, "player", &f.owners[0], None)).await.unwrap();
    assert_eq!(tile, admin::tile_body(used));

    sqlx::query("INSERT INTO storage.purged_players (player_id, handle) VALUES ($1::uuid, 'Gone#0001')")
        .bind(&f.owners[1])
        .execute(f.pool())
        .await
        .unwrap();
    let item = provider.panel(query(admin::ATTENTION_PURGES, "player", &f.owners[0], None)).await.unwrap();
    assert!(matches!(&item.primitive, Primitive::AttentionItems(items) if items.len() == 1));
    rejected(provider.panel(query("nope", "player", &f.owners[0], None)).await, Status::NotFound);
    f.cleanup().await;
}
