//! The API keys provider: the manifest, the Keys and Roles bodies, the FAKE cells, the cursor,
//! the refusals decided before the store; and, over a live Postgres, the Keys pager, filters
//! and total and the Roles key counts through `Provider::panel`.

use std::collections::BTreeMap;
use std::sync::Arc;

use controlplane::{
    order, ActionPoint, Align, CellKind, DataTable, InvokeCall, Layout, Link, PanelBody, PanelQuery, Point, Primitive,
    Provider, Role, SearchQuery, Then, Tone,
};
use opsapi::Status;
use sqlx::PgPool;

use super::*;
use crate::store::{KeyRow, RoleRow, Store};
use crate::store_tests::{cleanup, db_test_lock, test_pool, unique_name};

fn key(i: usize, revoked: bool) -> KeyRow {
    KeyRow {
        name: format!("key-{i:02}"),
        prefix: format!("ak_{i:04}"),
        role: "ops".to_string(),
        revision: 1,
        revoked,
        created: "2026-11-23".to_string(),
        created_us: 1_000_000 - i as i64,
    }
}

fn table(body: &PanelBody) -> &DataTable {
    match &body.primitive {
        Primitive::DataTable(t) => t,
        other => panic!("a DataTable, got {other:?}"),
    }
}

fn provider(pool: PgPool) -> ApiKeysAdmin {
    ApiKeysAdmin::new(Arc::new(Service { store: Store { pool } }))
}

/// A provider over a pool that cannot connect: every case it serves is decided before the store.
fn dead_provider() -> ApiKeysAdmin {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_millis(250))
        .connect_lazy("postgres://gamebackend:gamebackend@127.0.0.1:1/gamebackend?sslmode=disable")
        .unwrap();
    provider(pool)
}

fn query(contribution: &str, search: Option<&str>, cursor: Option<&str>, params: &[(&str, &str)]) -> PanelQuery {
    PanelQuery {
        module: "apikeys".into(),
        contribution: contribution.into(),
        entity: None,
        search: search.map(String::from),
        cursor: cursor.map(String::from),
        before: None,
        queue_state: None,
        range: None,
        params: params.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        refs: Vec::new(),
    }
}

fn call(action: &str, input: BTreeMap<String, String>) -> InvokeCall {
    InvokeCall {
        module: "apikeys".into(),
        action: action.into(),
        entity: Some(EntityRef { kind: KEY_KIND.into(), id: "k".into() }),
        input,
        expected_revision: None,
        reason: None,
        operator: "tester".into(),
        item: None,
        request_id: None,
    }
}

#[test]
fn manifest_declares_the_page_its_two_panels_kinds_search_and_row_actions() {
    let m = manifest();
    assert_eq!(m.module, "apikeys");
    assert!(m.writes);
    assert_eq!(m.entries.len(), 1);
    let nav = &m.entries[0];
    assert_eq!((nav.section.as_str(), nav.id.as_str(), nav.label.as_str()), ("platform", "apikeys", "API keys"));
    assert_eq!((nav.icon.as_str(), nav.page.as_str()), ("key", "keys"));
    assert_eq!(nav.order, order::platform_nav::APIKEYS);
    assert!(nav.counter.is_none());

    let page = &m.pages[0];
    assert_eq!((page.id.as_str(), page.title.as_str()), ("keys", "API keys"));
    assert_eq!(page.subtitle, "Machine credentials for game servers, CI and integrations.");
    assert!(page.head_actions.is_empty());
    assert_eq!(page.layout, Layout::Stack(vec!["keys".into(), "roles".into()]));
    assert_eq!(page.params, vec!["status".to_string(), "role".to_string()]);

    let panels: Vec<_> = m.contributions.iter().map(|c| (c.id.as_str(), c.order, c.title.as_str())).collect();
    assert_eq!(panels, vec![("keys", 10, "Keys"), ("roles", 20, "Roles")]);
    for c in &m.contributions {
        assert_eq!(c.point, Point::Page("keys".into()));
        assert_eq!(c.min_role, Role::Viewer);
        assert_eq!(c.cache_ttl_s, None);
    }

    let kinds: Vec<_> = m.entity_kinds.iter().map(|k| (k.kind.as_str(), k.label.as_str(), k.icon.as_str())).collect();
    assert_eq!(kinds, vec![("apikey", "API key", "key"), ("apirole", "API role", "shield")]);
    let search: Vec<_> = m.search.iter().map(|s| (s.kind.as_str(), s.label.as_str(), s.icon.as_str())).collect();
    assert_eq!(search, vec![("apikey", "API keys", "key")]);
    assert_eq!(m.search[0].deadline_ms, controlplane::DEFAULT_SEARCH_DEADLINE_MS);

    let rotate = m.actions.iter().find(|a| a.id == "key.rotate").expect("key.rotate");
    assert_eq!(rotate.point, ActionPoint::RowAction("apikey".into()));
    assert_eq!((rotate.label.as_str(), rotate.icon.as_str(), rotate.order), ("Rotate secret…", "refresh", 10));
    assert!(!rotate.implemented);
    assert_eq!(rotate.min_role, Role::Operator);
    assert_eq!(rotate.then, Then::Invoke);

    let requests = m.actions.iter().find(|a| a.id == "key.requests").expect("key.requests");
    assert_eq!(requests.point, ActionPoint::RowAction("apikey".into()));
    assert_eq!((requests.label.as_str(), requests.icon.as_str(), requests.order), ("Requests by this key", "list", 40));
    assert!(requests.implemented);
    assert_eq!(requests.min_role, Role::Viewer);
    assert_eq!(
        requests.then,
        Then::Link(Link::Page { module: "audit".into(), page: "log".into(), params: BTreeMap::new() })
    );
    assert_eq!(m.actions.len(), 2);
}

#[test]
fn a_key_row_draws_name_prefix_role_created_last_used_and_status() {
    let body = keys_body(vec![key(1, false), key(2, true)], vec!["ops".into()], Some(2), 1);
    let t = table(&body);
    let labels: Vec<_> = t.columns.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, vec!["Name", "Key", "Role", "Created", "Last used", "Status"]);
    let row = &t.rows[0];
    assert_eq!(row.entity, Some(EntityRef { kind: "apikey".into(), id: "key-01".into() }));
    let c = &row.cells;
    assert_eq!((c[0].kind, c[0].value.as_str()), (CellKind::Strong, "key-01"));
    assert_eq!((c[1].kind, c[1].value.as_str()), (CellKind::MonoMuted, "ak_0001…"));
    assert_eq!((c[2].kind, c[2].value.as_str()), (CellKind::Ref, "ops"));
    assert_eq!((c[3].kind, c[3].value.as_str(), c[3].row), (CellKind::Dim, "2026-11-23", true));
    assert_eq!(c[3].more.len(), 1);
    assert_eq!((c[3].more[0].kind, c[3].more[0].value.as_str()), (CellKind::User, fake_creator("key-01")));
    assert_eq!(c[4], fake_last_used("key-01"));
    assert_eq!((c[5].kind, c[5].value.as_str(), c[5].tone), (CellKind::Badge, "active", None));
    assert_eq!(t.rows[1].cells[5].value, "revoked");
    assert_eq!(body.meta.as_deref(), Some("1 active"));
    assert_eq!(t.search_placeholder.as_deref(), Some("Name or key prefix"));
    assert_eq!(t.item_noun, Some(controlplane::Noun::new("key", "keys")));
    let filters: Vec<_> = t.filters.iter().map(|f| (f.key.as_str(), f.kind.clone())).collect();
    assert_eq!(
        filters,
        vec![
            ("status", controlplane::FilterKind::Select(vec!["active".into(), "revoked".into()])),
            ("role", controlplane::FilterKind::Select(vec!["ops".into()])),
        ]
    );
}

#[test]
fn fake_cells_are_stable_per_name_and_drawn_from_their_lists() {
    let mut seen_now = false;
    let mut seen_dim = false;
    for i in 0..64 {
        let name = format!("key-{i}");
        assert_eq!(fake_creator(&name), fake_creator(&name));
        assert!(FAKE_CREATORS.contains(&fake_creator(&name)));
        let last = fake_last_used(&name);
        assert_eq!(last, fake_last_used(&name));
        match last.kind {
            CellKind::Badge => {
                assert_eq!((last.value.as_str(), last.tone), ("now", Some(Tone::Ok)));
                seen_now = true;
            }
            CellKind::Dim => {
                assert!(FAKE_LAST_USED.contains(&last.value.as_str()));
                assert_eq!(last.tone, None);
                seen_dim = true;
            }
            other => panic!("last used is a badge or dim text, got {other:?}"),
        }
    }
    assert!(seen_now && seen_dim, "64 names draw both shapes");
}

#[test]
fn a_page_of_25_has_no_cursor_and_a_26th_row_sets_it_to_the_25th() {
    let full: Vec<KeyRow> = (0..PAGE_SIZE).map(|i| key(i, false)).collect();
    let body = keys_body(full, Vec::new(), Some(25), 25);
    let t = table(&body);
    assert_eq!(t.rows.len(), 25);
    assert_eq!(t.page.next_cursor, None);
    assert_eq!((t.page.size, t.page.total), (25, Some(25)));

    let over: Vec<KeyRow> = (0..=PAGE_SIZE).map(|i| key(i, false)).collect();
    let body = keys_body(over, Vec::new(), None, 26);
    let t = table(&body);
    assert_eq!(t.rows.len(), 25);
    assert_eq!(t.page.next_cursor, Some(cursor_of(&key(24, false))));
    assert_eq!(t.page.total, None);
}

#[test]
fn an_empty_keys_table_is_still_the_table_with_a_zero_meta() {
    let body = keys_body(Vec::new(), Vec::new(), Some(0), 0);
    assert!(table(&body).rows.is_empty());
    assert_eq!(body.meta.as_deref(), Some("0 active"));
}

#[test]
fn the_cursor_round_trips_and_a_malformed_one_is_invalid() {
    let row = KeyRow { name: "ci.deploy".into(), ..key(1, false) };
    let raw = cursor_of(&row);
    assert_eq!(parse_cursor(&raw).unwrap(), (row.created_us, "ci.deploy"));
    let long = format!("1.{}", "n".repeat(MAX_NAME_BYTES + 1));
    let at_cap = format!("1.{}", "n".repeat(MAX_NAME_BYTES));
    assert!(parse_cursor(&at_cap).is_ok());
    let over_micros = format!("{}.n", (1i64 << 53) + 1);
    for bad in ["", "1", ".n", "1.", "x1.n", "-1.n", long.as_str(), over_micros.as_str()] {
        assert_eq!(parse_cursor(bad).unwrap_err().status, Status::Invalid, "{bad:?}");
    }
}

#[test]
fn roles_draw_one_tag_per_method_or_full_a_right_aligned_count_meta_and_foot() {
    let rows = vec![
        RoleRow { name: "ci".into(), policy: "leaderboard.topScores, match.report".into(), revision: 1, keys: 0 },
        RoleRow { name: "server".into(), policy: "full".into(), revision: 1, keys: 3 },
    ];
    let body = roles_body(&rows);
    let t = table(&body);
    let labels: Vec<_> = t.columns.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, vec!["Role", "Scopes", "Keys"]);
    assert!(t.columns[1].wrap);
    assert_eq!(t.columns[2].align, Align::Right);
    let ci = &t.rows[0];
    assert_eq!(ci.entity, Some(EntityRef { kind: "apirole".into(), id: "ci".into() }));
    assert_eq!((ci.cells[0].kind, ci.cells[0].value.as_str()), (CellKind::Strong, "ci"));
    let scopes = &ci.cells[1];
    assert_eq!((scopes.kind, scopes.value.as_str()), (CellKind::Ref, "leaderboard.topScores"));
    assert_eq!(scopes.more.iter().map(|c| (c.kind, c.value.as_str())).collect::<Vec<_>>(), vec![(CellKind::Ref, "match.report")]);
    assert_eq!((ci.cells[2].kind, ci.cells[2].value.as_str()), (CellKind::Number, "0"));
    let full = &t.rows[1].cells[1];
    assert_eq!((full.value.as_str(), full.more.len()), ("full", 0));
    assert_eq!(t.rows[1].cells[2].value, "3");
    assert_eq!((t.page.next_cursor.clone(), t.page.total), (None, None));
    assert_eq!(body.meta.as_deref(), Some("2"));
    assert_eq!(body.foot.as_deref(), Some("Policies list the op methods modules serve (e.g. leaderboard.topScores)."));
    assert_eq!(roles_body(&[]).meta.as_deref(), Some("0"));
}

#[test]
fn a_search_hit_is_the_name_the_prefix_and_role_and_the_status() {
    let hit = search_hit(&key(3, true));
    assert_eq!((hit.title.as_str(), hit.subtitle.as_str()), ("key-03", "ak_0003… · role ops"));
    assert_eq!(hit.badge.as_deref(), Some("revoked"));
    assert_eq!(hit.link, Link::Page { module: "apikeys".into(), page: "keys".into(), params: BTreeMap::new() });
}

#[tokio::test]
async fn refusals_are_decided_before_the_store() {
    let p = dead_provider();
    let e = p.invoke(call("key.rotate", BTreeMap::new())).await.unwrap_err();
    assert_eq!(e.status, Status::Forbidden);
    let e = p.invoke(call("key.nope", BTreeMap::new())).await.unwrap_err();
    assert_eq!(e.status, Status::NotFound);
    let big = BTreeMap::from([("x".to_string(), "v".repeat(controlplane::MAX_INPUT_VALUE_BYTES + 1))]);
    let e = p.invoke(call("key.rotate", big)).await.unwrap_err();
    assert_eq!(e.status, Status::Invalid);

    let e = p.panel(query("nope", None, None, &[])).await.unwrap_err();
    assert_eq!(e.status, Status::NotFound);
    let long = "n".repeat(MAX_NEEDLE_BYTES + 1);
    let e = p.panel(query("keys", Some(&long), None, &[])).await.unwrap_err();
    assert_eq!(e.status, Status::Invalid);
    let e = p.panel(query("keys", None, None, &[("status", "lost")])).await.unwrap_err();
    assert_eq!(e.status, Status::Invalid);

    for (kind, q) in [("apirole", "x"), ("apikey", "  "), ("apikey", long.as_str())] {
        let req = SearchQuery { module: "apikeys".into(), kind: kind.into(), q: q.into(), limit: 10 };
        assert!(p.search(req).await.unwrap().is_empty(), "{kind} {q:?}");
    }
}

#[tokio::test]
async fn keys_page_by_25_narrow_by_filters_and_drop_the_total_and_roles_count_keys() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let base = unique_name(&pool).await;
    let store = Store { pool: pool.clone() };
    let (role_a, role_b) = (format!("{base}-a"), format!("{base}-b"));
    store.create_role(&role_a, "full").await.unwrap();
    store.create_role(&role_b, "match.report").await.unwrap();
    for i in 0..PAGE_SIZE {
        store.create_key(&format!("{base}-k{i:02}"), &role_a).await.unwrap();
    }
    let revoked = format!("{base}-r");
    store.create_key(&revoked, &role_b).await.unwrap();
    let revision = store.keys_page(Some(&revoked), None, None, None, 1).await.unwrap()[0].revision;
    store.revoke_key(&revoked, revision).await.unwrap();
    let p = provider(pool.clone());

    let first = p.panel(query("keys", Some(&base), None, &[])).await.unwrap();
    let t = table(&first);
    assert_eq!(t.rows.len(), 25);
    assert_eq!(t.page.total, None, "a search narrows, so no total");
    let cursor = t.page.next_cursor.clone().expect("a 26th key exists");
    let second = p.panel(query("keys", Some(&base), Some(&cursor), &[])).await.unwrap();
    let t2 = table(&second);
    assert_eq!(t2.rows.len(), 1);
    assert_eq!(t2.page.next_cursor, None);
    let mut names: Vec<_> = t.rows.iter().chain(&t2.rows).map(|r| r.cells[0].value.clone()).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), 26, "every key once");

    let only_revoked = p.panel(query("keys", Some(&base), None, &[("status", "revoked")])).await.unwrap();
    let rows: Vec<_> = table(&only_revoked).rows.iter().map(|r| r.cells[0].value.clone()).collect();
    assert_eq!(rows, vec![revoked.clone()]);
    let by_role = p.panel(query("keys", None, None, &[("role", role_b.as_str())])).await.unwrap();
    let t = table(&by_role);
    assert_eq!(t.rows.iter().map(|r| r.cells[0].value.clone()).collect::<Vec<_>>(), vec![revoked.clone()]);
    assert_eq!(t.page.total, None, "a filter narrows, so no total");
    let gone = p.panel(query("keys", None, None, &[("role", &format!("{base}-none"))])).await.unwrap();
    let t = table(&gone);
    assert!(t.rows.is_empty(), "a role that no longer exists matches no keys");
    assert_eq!(t.page.total, None, "a filter narrows, so no total");
    let long = "r".repeat(MAX_NAME_BYTES + 1);
    let e = p.panel(query("keys", None, None, &[("role", long.as_str())])).await.unwrap_err();
    assert_eq!(e.status, Status::Invalid);

    let all = p.panel(query("keys", None, None, &[("status", "any")])).await.unwrap();
    let (total, active) = store.key_counts().await.unwrap();
    assert_eq!(table(&all).page.total, Some(total as u64));
    assert_eq!(all.meta, Some(format!("{active} active")));
    let filters = &table(&all).filters;
    let controlplane::FilterKind::Select(roles) = &filters[1].kind else { panic!("role select") };
    assert!(roles.contains(&role_a) && roles.contains(&role_b));

    let roles = p.panel(query("roles", None, None, &[])).await.unwrap();
    let counts: Vec<_> = table(&roles)
        .rows
        .iter()
        .filter(|r| r.cells[0].value.starts_with(&base))
        .map(|r| (r.cells[0].value.clone(), r.cells[2].value.clone()))
        .collect();
    assert_eq!(counts, vec![(role_a.clone(), "25".to_string()), (role_b.clone(), "1".to_string())]);

    let hits = p
        .search(SearchQuery { module: "apikeys".into(), kind: "apikey".into(), q: revoked.clone(), limit: 50 })
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].badge.as_deref(), Some("revoked"));
    let capped = p
        .search(SearchQuery { module: "apikeys".into(), kind: "apikey".into(), q: base.clone(), limit: 50 })
        .await
        .unwrap();
    assert_eq!(capped.len(), 25);

    cleanup(&pool, &base).await;
}
