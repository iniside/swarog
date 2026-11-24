//! `core.events` maps each [`VerbError`] to its status, and an unknown subscription is
//! the verb's own `NotFound` with no pre-read. Live-Postgres tests fail without a
//! database unless `TESTDB_ALLOW_SKIP` is set (`testdb`).

use std::sync::Arc;

use controlplane::ControlPlane;

use super::*;

async fn test_pool() -> Option<PgPool> {
    let pool = testdb::test_pool().await?;
    asyncevents::Plane::new(pool.clone(), testdb::dsn())
        .unwrap()
        .migrate()
        .await
        .expect("migrate the asyncevents plane");
    Some(pool)
}

fn unique(prefix: &str) -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    format!("{prefix}.{}-{}", std::process::id(), nanos)
}

async fn insert_sub(pool: &PgPool, id: &str, state: &str) {
    sqlx::query(
        "INSERT INTO asyncevents.subscriptions \
           (subscription_id, topic, contract_version, state, \
            cursor_generation, cursor_xid, cursor_tie, consecutive_failures, next_attempt_at, \
            last_error, spec_hash, start_kind, updated_at) \
         VALUES ($1, $1, 1, $2, 0, '0'::xid8, 0, 3, NULL, 'boom', 'h', 'genesis', now())",
    )
    .bind(id)
    .bind(state)
    .execute(pool)
    .await
    .unwrap();
}

async fn delete_sub(pool: &PgPool, id: &str) {
    let _ = sqlx::query("DELETE FROM asyncevents.subscriptions WHERE subscription_id = $1").bind(id).execute(pool).await;
}

fn invoke_call(action: &str, id: &str) -> InvokeCall {
    InvokeCall {
        module: MODULE.to_string(),
        action: action.to_string(),
        entity: Some(EntityRef { kind: KIND.to_string(), id: id.to_string() }),
        input: BTreeMap::new(),
        expected_revision: None,
        reason: Some("operator reason".to_string()),
        operator: "tester".to_string(),
        item: Some(poison_item(id, "0123abcd-0000-0000-0000-000000000000")),
        request_id: None,
    }
}

fn panel_query(contribution: &str, id: &str) -> PanelQuery {
    PanelQuery {
        module: MODULE.to_string(),
        contribution: contribution.to_string(),
        entity: Some(EntityRef { kind: KIND.to_string(), id: id.to_string() }),
        search: None,
        cursor: None,
        before: None,
        queue_state: None,
        range: None,
        params: BTreeMap::new(),
        refs: Vec::new(),
    }
}

/// Every subscription verb `core.events` invokes.
const VERBS: [&str; 5] = [SUB_PAUSE, SUB_RESUME, SUB_RETRY, POISON_RESUME, POISON_SKIP];

#[test]
fn each_verb_error_maps_to_its_status() {
    let id = || "s.v1".to_string();
    let cases = [
        (VerbError::NotFound { id: id() }, opsapi::Status::NotFound),
        (VerbError::Retired { id: id() }, opsapi::Status::Invalid),
        (VerbError::Healthy { id: id() }, opsapi::Status::Invalid),
        (VerbError::NothingToSkip { id: id() }, opsapi::Status::Invalid),
        (VerbError::Db(anyhow::anyhow!("down").context("read")), opsapi::Status::Internal),
    ];
    for (error, status) in cases {
        let message = format!("{error:#}");
        let mapped = verb_error(error);
        assert_eq!(mapped.status, status, "{message}");
        assert_eq!(mapped.msg, message, "the status carries the verb's message");
    }
}

/// An unknown id reaches the provider as the verb's own `NotFound`: its message is the
/// plane's, so no provider-side lookup answered it first.
#[tokio::test]
async fn unknown_subscription_is_the_verbs_not_found() {
    let Some(pool) = test_pool().await else { return };
    let provider = EventsProvider::new(pool);
    let id = unique("core-events.missing");
    let want = VerbError::NotFound { id: id.clone() }.to_string();
    for action in VERBS {
        let err = provider.invoke(invoke_call(action, &id)).await.expect_err(action);
        assert_eq!((err.status, err.msg.as_str()), (opsapi::Status::NotFound, want.as_str()), "{action}");
    }
    for contribution in [HEADER, DETAILS] {
        let err = provider.panel(panel_query(contribution, &id)).await.expect_err(contribution);
        assert_eq!((err.status, err.msg.as_str()), (opsapi::Status::NotFound, want.as_str()), "{contribution}");
    }
}

/// A retired subscription reaches the provider as `Invalid` from every verb.
#[tokio::test]
async fn retired_subscription_is_invalid() {
    let Some(pool) = test_pool().await else { return };
    let id = unique("core-events.retired");
    insert_sub(&pool, &id, "retired").await;
    let provider = EventsProvider::new(pool.clone());
    let want = VerbError::Retired { id: id.clone() }.to_string();
    for action in VERBS {
        let err = provider.invoke(invoke_call(action, &id)).await.expect_err(action);
        assert_eq!((err.status, err.msg.as_str()), (opsapi::Status::Invalid, want.as_str()), "{action}");
    }
    delete_sub(&pool, &id).await;
}

/// A pool whose every query fails, so a plane call through it is `Internal`.
fn unreachable_pool() -> PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_millis(250))
        .connect_lazy("postgres://gamebackend:gamebackend@127.0.0.1:1/gamebackend?sslmode=disable")
        .expect("lazy pool from a well-formed DSN")
}

/// A database failure reaches the provider as `Internal`.
#[tokio::test]
async fn database_failure_is_internal() {
    let provider = EventsProvider::new(unreachable_pool());
    for action in VERBS {
        let err = provider.invoke(invoke_call(action, "core-events.unreachable")).await.expect_err(action);
        assert_eq!(err.status, opsapi::Status::Internal, "{action}: {}", err.msg);
    }
}

/// `poison.skip` without a previewed event (no item, or an item naming no event) is
/// `Invalid` before any plane call: the pool is unreachable, so a call to `skip` would
/// answer `Internal` instead.
#[tokio::test]
async fn skip_without_a_previewed_event_is_invalid_without_calling_skip() {
    let provider = EventsProvider::new(unreachable_pool());
    let id = "core-events.unreachable";
    for item in [None, Some(format!("{id}#"))] {
        let mut call = invoke_call(POISON_SKIP, id);
        call.item = item.clone();
        let err = provider.invoke(call).await.expect_err("skip without a previewed event");
        assert_eq!((err.status, err.msg.as_str()), (opsapi::Status::Invalid, SKIP_NEEDS_ITEM), "{item:?}");
    }
}

#[test]
fn consumer_is_the_subscription_id_prefix() {
    assert_eq!(consumer("audit.player-deleted.v1"), "audit");
    assert_eq!(consumer("solo"), "solo");
}

#[test]
fn manifest_declares_only_backed_tiles_and_row_actions() {
    let manifest = manifest();
    assert_eq!(manifest.pages[0].tiles, [KPI_SUBSCRIPTIONS, KPI_MAX_LAG]);
    let actions: Vec<&str> = manifest.actions.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(actions, [POISON_RESUME, POISON_SKIP, POISON_COPY, SUB_PAUSE, SUB_RESUME, SUB_RETRY]);
}

/// Copy payload is `Then::Copy` on the poison panel's tools, so it copies that panel's code
/// block (the previewed payload) in the browser; an invoke of it never reaches the provider.
#[tokio::test]
async fn copy_payload_copies_the_poison_panel_and_has_no_invoke() {
    let manifest = manifest();
    let copy = manifest.actions.iter().find(|a| a.id == POISON_COPY).expect("Copy payload is declared");
    assert_eq!(copy.then, Then::Copy);
    assert_eq!(copy.point, ActionPoint::PanelTool(POISON.to_string()));
    assert!(copy.implemented, "Copy payload is drawn enabled");
    let provider: Arc<dyn Provider> = Arc::new(EventsProvider::new(unreachable_pool()));
    let face = controlplane::LocalFace::new(vec![provider]);
    let err = face.invoke(invoke_call(POISON_COPY, "core-events.copy")).await.expect_err("a copy has no invoke");
    assert_eq!(err.status, opsapi::Status::Invalid);
    assert!(err.msg.contains("has no invoke"), "{}", err.msg);
}

#[tokio::test]
async fn subscription_row_shows_the_stored_cursor_and_the_id_prefix() {
    let Some(pool) = test_pool().await else { return };
    let id = unique("core-events.cursor");
    insert_sub(&pool, &id, "active").await;
    sqlx::query(
        "UPDATE asyncevents.subscriptions SET cursor_generation = 4, cursor_xid = '77'::xid8, cursor_tie = 9 \
         WHERE subscription_id = $1",
    )
    .bind(&id)
    .execute(&pool)
    .await
    .unwrap();
    let provider = EventsProvider::new(pool.clone());
    let query = PanelQuery { search: Some(id.clone()), ..panel_query(SUBSCRIPTIONS, &id) };
    let body = provider.panel(query).await.unwrap();
    delete_sub(&pool, &id).await;
    let Primitive::DataTable(table) = body.primitive else { panic!("the subscriptions panel is a table") };
    let labels: Vec<&str> = table.columns.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, ["Subscription", "Topic", "Consumer", "Checkpoint", "Lag", "State"]);
    let cells: Vec<&str> = table.rows[0].cells.iter().map(|c| c.value.as_str()).collect();
    assert_eq!(&cells[..5], [id.as_str(), &format!("{id} v1"), "core-events", "4/77/9", "0"]);
}

fn property<'a>(items: &'a [(String, Cell)], label: &str) -> &'a str {
    items.iter().find(|(l, _)| l == label).map(|(_, c)| c.value.as_str()).unwrap_or_else(|| panic!("no {label} property"))
}

#[tokio::test]
async fn details_consumer_is_the_id_prefix() {
    let Some(pool) = test_pool().await else { return };
    let id = unique("core-events.details");
    insert_sub(&pool, &id, "active").await;
    let provider = EventsProvider::new(pool.clone());
    let body = provider.panel(panel_query(DETAILS, &id)).await;
    delete_sub(&pool, &id).await;
    let Primitive::PropertyList(list) = body.unwrap().primitive else { panic!("the details panel is a property list") };
    assert_eq!(property(&list.items, "Consumer"), "core-events");
}

/// The poison panel shows the first paused subscription, which on a shared database may
/// be another test's, so the Consumer is checked against the prefix of the heading it names.
#[tokio::test]
async fn poison_consumer_is_the_id_prefix() {
    let Some(pool) = test_pool().await else { return };
    let id = unique("core-events.poison");
    insert_sub(&pool, &id, "paused").await;
    let provider = EventsProvider::new(pool.clone());
    let body = provider.panel(panel_query(POISON, &id)).await;
    delete_sub(&pool, &id).await;
    let body = body.unwrap();
    let heading = body.heading.expect("a paused subscription is named");
    let Primitive::PropertyList(list) = body.primitive else { panic!("the poison panel is a property list") };
    let prefix = heading.split('.').next().unwrap();
    assert_eq!(property(&list.items, "Consumer"), prefix);
}
