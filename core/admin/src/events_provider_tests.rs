//! `core.events` maps each [`VerbError`] to its status, and an unknown subscription is
//! the verb's own `NotFound` with no pre-read. Live-Postgres tests skip without a
//! database (`testdb`).

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

/// A database failure reaches the provider as `Internal`.
#[tokio::test]
async fn database_failure_is_internal() {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_millis(250))
        .connect_lazy("postgres://gamebackend:gamebackend@127.0.0.1:1/gamebackend?sslmode=disable")
        .expect("lazy pool from a well-formed DSN");
    let provider = EventsProvider::new(pool);
    for action in VERBS {
        let err = provider.invoke(invoke_call(action, "core-events.unreachable")).await.expect_err(action);
        assert_eq!(err.status, opsapi::Status::Internal, "{action}: {}", err.msg);
    }
}
