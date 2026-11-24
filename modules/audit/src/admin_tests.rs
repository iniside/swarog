//! The audit provider: the manifest (nav, page, the untitled table, the disabled Export
//! and the refusing `invoke`), the table's columns, cells, toolbar and pager, and the
//! filter params reaching the store through `Provider::panel` against the local DB.

use std::collections::BTreeMap;

use controlplane::{
    order, ActionPoint, CellKind, DataTable, FilterKind, Layout, PanelBody, PanelQuery, Point, Primitive, Provider,
    Role, SortDir, Then,
};
use opsapi::Status;
use serde_json::{json, Value};
use sqlx::PgPool;

use super::*;
use crate::store::{LogCursor, LogPage, LogRow, SYSTEM_ACTOR};
use crate::{Audit, Context, Module, DURABLE_TOPICS};

fn table(body: &PanelBody) -> &DataTable {
    match &body.primitive {
        Primitive::DataTable(t) => t,
        other => panic!("expected a data table, got {other:?}"),
    }
}

fn query(contribution: &str) -> PanelQuery {
    PanelQuery {
        module: MODULE.into(),
        contribution: contribution.into(),
        entity: None,
        search: None,
        cursor: None,
        before: None,
        queue_state: None,
        range: None,
        params: Default::default(),
        refs: Vec::new(),
    }
}

fn log_query(search: &str, params: &[(&str, &str)]) -> PanelQuery {
    PanelQuery {
        search: Some(search.to_string()),
        params: params.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        ..query(LOG)
    }
}

fn call(action: &str) -> InvokeCall {
    InvokeCall {
        module: MODULE.into(),
        action: action.into(),
        entity: None,
        input: Default::default(),
        expected_revision: None,
        reason: None,
        operator: "tester".into(),
        item: None,
        request_id: None,
    }
}

fn row(id: i64, actor: &str, topic: &str) -> LogRow {
    LogRow {
        id,
        at_us: 1_000 + id,
        time: "10:00:00".to_string(),
        actor: actor.to_string(),
        topic: topic.to_string(),
        action: "wallet.credit".to_string(),
        target: "player:p1".to_string(),
        detail: "ok".to_string(),
    }
}

#[test]
fn manifest_declares_the_audit_log_page() {
    let m = manifest();
    assert_eq!(m.module, MODULE);
    assert!(!m.writes);
    assert!(m.search.is_empty());
    assert!(m.entity_kinds.is_empty());

    assert_eq!(m.entries.len(), 1);
    let nav = &m.entries[0];
    assert_eq!(
        (nav.section.as_str(), nav.id.as_str(), nav.label.as_str(), nav.icon.as_str(), nav.page.as_str()),
        ("platform", "audit", "Audit log", "list", "log")
    );
    assert_eq!(nav.order, order::platform_nav::AUDIT);
    assert!(nav.counter.is_none());

    assert_eq!(m.pages.len(), 1);
    let page = &m.pages[0];
    assert_eq!((page.id.as_str(), page.title.as_str(), page.subtitle.as_str()), (PAGE, TITLE, SUBTITLE));
    assert_eq!(page.head_actions, [LOG_EXPORT]);
    assert_eq!(page.layout, Layout::Stack(vec![LOG.to_string()]));
    assert_eq!(page.params, [PARAM_ACTOR, PARAM_TOPIC, PARAM_TIME]);
    assert!(page.tiles.is_empty());

    // H3: one untitled panel, so the portal draws no panel head.
    assert_eq!(m.contributions.len(), 1);
    let c = &m.contributions[0];
    assert_eq!((c.id.as_str(), c.title.as_str(), c.order), (LOG, "", 10));
    assert_eq!(c.point, Point::Page(PAGE.to_string()));
    assert_eq!((c.min_role, c.cache_ttl_s, c.readonly.as_deref()), (Role::Viewer, None, None));

    assert_eq!(m.actions.len(), 1);
    let a = &m.actions[0];
    assert_eq!((a.id.as_str(), a.label.as_str(), a.icon.as_str(), a.order), (LOG_EXPORT, "Export", "download", 10));
    assert_eq!(a.point, ActionPoint::PageHead(PAGE.to_string()));
    assert_eq!(a.then, Then::Invoke);
    assert!(!a.implemented && !a.danger && !a.trailing_icon);
}

/// H7: the pager carries the exact total, 25 rows a page and the next row's keyset
/// position; the foot counts entries.
#[test]
fn page_info_carries_the_total_and_the_cursor() {
    let next = LogCursor { at_us: 1_732_400_000_000_000, id: 42 };
    let body = log_body(LogPage { rows: vec![row(1, "marta.k", "admin.action")], next: Some(next) }, 1204, Vec::new());
    let t = table(&body);
    assert_eq!(t.page.total, Some(1204));
    assert_eq!(t.page.size, 25);
    assert_eq!(t.page.next_cursor.as_deref(), Some("1732400000000000.42"));
    assert_eq!(parse_cursor(t.page.next_cursor.as_deref().unwrap()).unwrap(), next);
    let noun = t.item_noun.as_ref().unwrap();
    assert_eq!((noun.one.as_str(), noun.many.as_str()), ("entry", "entries"));
    assert_eq!(t.search_placeholder.as_deref(), Some(SEARCH_PLACEHOLDER));
    assert!(!t.selectable);

    let last = log_body(LogPage { rows: Vec::new(), next: None }, 0, Vec::new());
    assert_eq!((table(&last).page.next_cursor.as_deref(), table(&last).page.total), (None, Some(0)));

    for bad in ["", "12", ".1", "1.", "a.1", "1.b", "-1.2", "1.2.3", "99999999999999999999.1"] {
        assert_eq!(parse_cursor(bad).unwrap_err().status, Status::Invalid, "{bad:?}");
    }
}

/// H9: only Time carries a sort arrow (newest first); only Detail wraps.
#[test]
fn columns_sort_on_time_only() {
    let body = log_body(LogPage { rows: Vec::new(), next: None }, 0, Vec::new());
    let t = table(&body);
    let keys: Vec<&str> = t.columns.iter().map(|c| c.key.as_str()).collect();
    assert_eq!(keys, ["time", "actor", "topic", "action", "target", "detail"]);
    let sorted: Vec<Option<SortDir>> = t.columns.iter().map(|c| c.sorted).collect();
    assert_eq!(sorted, [Some(SortDir::Desc), None, None, None, None, None]);
    assert!(t.columns.iter().all(|c| !c.sortable));
    let wraps: Vec<bool> = t.columns.iter().map(|c| c.wrap).collect();
    assert_eq!(wraps, [false, false, false, false, false, true]);
}

/// H10: an operator's actor is a user cell linked to the page narrowed to that login;
/// `system` and a missing login are plain tags.
#[test]
fn operator_actor_links_to_the_actor_filtered_page() {
    let rows = vec![row(3, "marta.k", "admin.action"), row(2, SYSTEM_ACTOR, "wallet.changed"), row(1, MISSING, "admin.action")];
    let body = log_body(LogPage { rows, next: None }, 3, Vec::new());
    let t = table(&body);

    let kinds: Vec<CellKind> = t.rows[0].cells.iter().map(|c| c.kind).collect();
    assert_eq!(kinds, [CellKind::Mono, CellKind::User, CellKind::Ref, CellKind::Mono, CellKind::Mono, CellKind::Dim]);
    let values: Vec<&str> = t.rows[0].cells.iter().map(|c| c.value.as_str()).collect();
    assert_eq!(values, ["10:00:00", "marta.k", "admin.action", "wallet.credit", "player:p1", "ok"]);
    let want = Link::Page {
        module: "audit".to_string(),
        page: "log".to_string(),
        params: BTreeMap::from([("actor".to_string(), "marta.k".to_string())]),
    };
    assert_eq!(t.rows[0].cells[1].link.as_ref(), Some(&want));

    for r in &t.rows[1..] {
        assert_eq!(r.cells[1].kind, CellKind::Ref);
        assert!(r.cells[1].link.is_none());
    }
    // No row opens anything: no entity, no row actions.
    assert!(t.rows.iter().all(|r| r.entity.is_none() && r.actions.is_empty() && r.item.is_none()));
    assert!(t.rows.iter().flat_map(|r| &r.cells).filter(|c| c.kind != CellKind::User).all(|c| c.link.is_none()));
}

/// The toolbar: Topic over the logged topics, Actor over the store's options then the
/// FAKE actors (A5), and the inert Time chip (A6).
#[test]
fn toolbar_offers_topic_actor_and_time() {
    let body = log_body(LogPage { rows: Vec::new(), next: None }, 0, actor_options(vec!["marta.k".into(), SYSTEM_ACTOR.into()]));
    let t = table(&body);
    let keys: Vec<&str> = t.filters.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(keys, [PARAM_TOPIC, PARAM_ACTOR, PARAM_TIME]);
    let labels: Vec<&str> = t.filters.iter().map(|f| f.label.as_str()).collect();
    assert_eq!(labels, ["Topic", "Actor", "Time"]);
    let topics: Vec<String> = DURABLE_TOPICS.iter().map(|s| s.to_string()).collect();
    assert_eq!(t.filters[0].kind, FilterKind::Select(topics));
    let actors: Vec<String> =
        ["marta.k", "system", "scheduler", "apikey:ak_c4a8"].iter().map(|s| s.to_string()).collect();
    assert_eq!(t.filters[1].kind, FilterKind::Select(actors));
    assert_eq!(t.filters[2].kind, FilterKind::Range);
}

#[tokio::test(flavor = "multi_thread")]
async fn invoke_refuses_every_action() {
    let p = AuditAdmin::new(PgPool::connect_lazy("postgres://localhost/unused").unwrap());
    assert_eq!(p.invoke(call(LOG_EXPORT)).await.unwrap_err().status, Status::Forbidden);
    assert_eq!(p.invoke(call("log.delete")).await.unwrap_err().status, Status::NotFound);
    assert_eq!(p.panel(query("nope")).await.unwrap_err().status, Status::NotFound);
}

// --- live Postgres ----------------------------------------------------------

async fn test_pool() -> Option<PgPool> {
    let pool = testdb::test_pool().await?;
    Audit::new().migrate(&Context::with_db(pool.clone())).await.expect("migrate audit schema");
    Some(pool)
}

/// A per-test-unique marker, placed in every fixture so a search finds only them.
async fn marker(pool: &PgPool) -> String {
    sqlx::query_scalar("SELECT 'mk' || replace(gen_random_uuid()::text, '-', '')").fetch_one(pool).await.unwrap()
}

/// Inserts one ledger row `age_s` seconds old and returns its id.
async fn insert(pool: &PgPool, topic: &str, payload: &Value, age_s: f64) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO audit.log (topic, payload, at) \
         VALUES ($1, $2::jsonb, now() - make_interval(secs => $3)) RETURNING id",
    )
    .bind(topic)
    .bind(payload.to_string())
    .bind(age_s)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn delete(pool: &PgPool, ids: &[i64]) {
    sqlx::query("DELETE FROM audit.log WHERE id = ANY($1)").bind(ids).execute(pool).await.unwrap();
}

/// A case's page params and the targets it draws, newest first.
type Case<'a> = (Vec<(&'a str, &'a str)>, Vec<&'a str>);

fn ids_and_total(body: &PanelBody) -> (Vec<String>, Option<u64>) {
    let t = table(body);
    (t.rows.iter().map(|r| r.cells[4].value.clone()).collect(), t.page.total)
}

/// The search, topic and actor params reach the store and narrow both the page and the
/// total; a FAKE actor narrows to zero rows and keeps the toolbar (H8); an unknown topic
/// is refused.
#[tokio::test(flavor = "multi_thread")]
async fn filter_params_narrow_the_page_and_the_total() {
    let Some(pool) = test_pool().await else { return };
    let m = marker(&pool).await;
    let op = format!("op-{m}");
    let (t1, t2, t3) = (format!("player:a-{m}"), format!("player:b-{m}"), format!("group:c-{m}"));
    let (t1, t2, t3) = (t1.as_str(), t2.as_str(), t3.as_str());
    let r1 = insert(&pool, "admin.action", &json!({"actor": op, "action": "wallet.credit", "target": t1, "detail": "ok"}), 30.0).await;
    let r2 = insert(&pool, "admin.action", &json!({"actor": "someone-else", "action": "x", "target": t2, "detail": "ok"}), 20.0).await;
    let r3 = insert(&pool, "group.created", &json!({"group_id": format!("c-{m}"), "name": "g", "creator_id": "c", "join_policy": "open"}), 10.0).await;
    let p = AuditAdmin::new(pool.clone());

    let cases: Vec<Case> = vec![
        (vec![], vec![t3, t2, t1]),
        (vec![(PARAM_TOPIC, "admin.action")], vec![t2, t1]),
        (vec![(PARAM_TOPIC, "group.created")], vec![t3]),
        (vec![(PARAM_ACTOR, op.as_str())], vec![t1]),
        (vec![(PARAM_ACTOR, SYSTEM_ACTOR)], vec![t3]),
        (vec![(PARAM_TOPIC, ""), (PARAM_ACTOR, "")], vec![t3, t2, t1]),
        (vec![(PARAM_TIME, "today")], vec![t3, t2, t1]),
        (vec![(PARAM_ACTOR, FAKE_ACTORS[0])], vec![]),
        (vec![(PARAM_ACTOR, FAKE_ACTORS[1])], vec![]),
    ];
    let mut got = Vec::new();
    for (params, _) in &cases {
        got.push(p.panel(log_query(&m, params)).await);
    }
    let unknown = p.panel(log_query(&m, &[(PARAM_TOPIC, "scheduler.fired")])).await;
    delete(&pool, &[r1, r2, r3]).await;

    for ((params, want), body) in cases.iter().zip(&got) {
        let body = body.as_ref().unwrap();
        let (targets, total) = ids_and_total(body);
        assert_eq!(targets, *want, "{params:?}");
        assert_eq!(total, Some(want.len() as u64), "{params:?}");
        assert_eq!(table(body).filters.len(), 3, "{params:?}");
    }
    assert_eq!(unknown.unwrap_err().status, Status::Invalid);
}

/// The pager's cursor reaches the store: the second page holds the rest, the total is
/// the same on both pages, and the last page has no next cursor.
#[tokio::test(flavor = "multi_thread")]
async fn cursor_walks_to_the_next_page() {
    let Some(pool) = test_pool().await else { return };
    let m = marker(&pool).await;
    let mut ids = Vec::new();
    for i in 0..26 {
        let payload = json!({"actor": "pager", "action": "a", "target": format!("t{i:02}-{m}"), "detail": "ok"});
        ids.push(insert(&pool, "admin.action", &payload, 100.0 - i as f64).await);
    }
    let p = AuditAdmin::new(pool.clone());
    let first = p.panel(log_query(&m, &[])).await;
    let cursor = first.as_ref().ok().and_then(|b| table(b).page.next_cursor.clone());
    let second = p.panel(PanelQuery { cursor: cursor.clone(), ..log_query(&m, &[]) }).await;
    delete(&pool, &ids).await;

    let (first_targets, first_total) = ids_and_total(first.as_ref().unwrap());
    assert_eq!(first_targets.len(), 25);
    assert_eq!(first_targets[0], format!("t25-{m}"));
    assert_eq!(first_total, Some(26));
    assert!(cursor.is_some());
    let second = second.unwrap();
    let (second_targets, second_total) = ids_and_total(&second);
    assert_eq!(second_targets, [format!("t00-{m}")]);
    assert_eq!(second_total, Some(26));
    assert!(table(&second).page.next_cursor.is_none());

    let bad = p.panel(PanelQuery { cursor: Some("x.1".into()), ..log_query(&m, &[]) }).await;
    assert_eq!(bad.unwrap_err().status, Status::Invalid);
}
