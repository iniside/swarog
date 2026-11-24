//! `parse_cursor`: a hand-edited queue cursor answers `Invalid` here, never a database error
//! from `store::list_tx`, whose float8 µs product is exact only up to 2^53.

use opsapi::Status;

use super::{parse_cursor, MAX_CURSOR_MICROS};

const ID: &str = "0190b5a2-7c3e-7d4a-9f1e-123456789abc";

fn status_of(raw: &str) -> Status {
    parse_cursor(raw).expect_err(raw).status
}

#[test]
fn parse_cursor_accepts_the_bound_itself() {
    assert_eq!(MAX_CURSOR_MICROS, 1 << 53);
    let raw = format!("{}.{ID}", 1_i64 << 53);
    assert_eq!(parse_cursor(&raw).unwrap(), (1 << 53, ID));
}

#[test]
fn parse_cursor_refuses_micros_past_the_bound() {
    assert_eq!(status_of(&format!("{}.{ID}", (1_i64 << 53) + 1)), Status::Invalid);
    assert_eq!(status_of(&format!("{}.{ID}", i64::MAX)), Status::Invalid);
}

#[test]
fn parse_cursor_refuses_a_malformed_cursor() {
    for raw in [
        "",
        ID,
        "123",
        &format!(".{ID}"),
        &format!("-1.{ID}"),
        &format!("12a.{ID}"),
        "123.not-a-uuid",
        // Past i64::MAX: digits only, so the i64 parse is what refuses it.
        &format!("99999999999999999999.{ID}"),
    ] {
        assert_eq!(status_of(raw), Status::Invalid, "cursor {raw:?}");
    }
}


// ============================================================================
// The provider over its service: the panels' data mapping and every REAL action's
// success and failure path. The cursor rule is above; the input and id caps are the
// conformance probes' (`crate::conformance`).
// ============================================================================

use std::collections::BTreeMap;
use std::sync::Arc;

use controlplane::{
    InvokeCall, InvokeOutcome, Link, PanelQuery, PrepareCall, Primitive, Provider, Then, WorkQueue,
    PAGE_PARAM_QUEUE_STATE, PAGE_PARAM_SEARCH,
};

use super::{
    next_attempt, row_actions, shown_state, tab_counts, MailAdmin, FILTER_TEMPLATE, PAGE_SIZE, TEST_KEY_PREFIX,
};
use crate::config::{MailConfig, DEFAULT_MAX_ATTEMPTS, DEFAULT_RETENTION_DAYS, DEFAULT_SEND_TIMEOUT_MS};
use crate::service::{NewMail, Service};
use crate::store::{
    Disposition, OutboxListRow, OutboxStats, Store, STATE_CANCELLED, STATE_PARKED, STATE_PENDING, STATE_SENT,
};
use crate::tests::{
    cleanup, enqueue, id_of, mail_of, quarantine_foreign_pending, row_of, test_pool, unique_key, wired, DB_LOCK,
};
use crate::worker::bounded_tx;

fn admin(svc: Arc<Service>) -> MailAdmin {
    let cfg = MailConfig {
        provider: None,
        send_timeout: std::time::Duration::from_millis(DEFAULT_SEND_TIMEOUT_MS),
        max_attempts: DEFAULT_MAX_ATTEMPTS,
        retention_days: DEFAULT_RETENTION_DAYS,
    };
    MailAdmin::new(svc, &cfg)
}

/// A provider whose store is unreachable: a branch that answers on it never read the store.
fn offline() -> MailAdmin {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_millis(250))
        .connect_lazy("postgres://gamebackend:gamebackend@127.0.0.1:1/gamebackend?sslmode=disable")
        .unwrap();
    admin(Arc::new(Service::new(pool)))
}

fn call(action: &str, input: &[(&str, &str)], item: Option<&str>, request_id: Option<&str>) -> InvokeCall {
    InvokeCall {
        module: "mail".to_string(),
        action: action.to_string(),
        entity: None,
        input: input.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        expected_revision: None,
        reason: None,
        operator: "admin_tests".to_string(),
        item: item.map(str::to_string),
        request_id: request_id.map(str::to_string),
    }
}

fn prepare(item: Option<&str>) -> PrepareCall {
    PrepareCall {
        module: "mail".to_string(),
        action: "cancel".to_string(),
        entity: None,
        item: item.map(str::to_string),
        operator: "admin_tests".to_string(),
    }
}

fn query(contribution: &str, state: Option<&str>, search: Option<&str>) -> PanelQuery {
    PanelQuery {
        module: "mail".to_string(),
        contribution: contribution.to_string(),
        entity: None,
        search: search.map(str::to_string),
        cursor: None,
        before: None,
        queue_state: state.map(str::to_string),
        range: None,
        params: BTreeMap::new(),
        refs: Vec::new(),
    }
}

fn listed(state: &str, attempts: i32, last_error: Option<&str>) -> OutboxListRow {
    OutboxListRow {
        id: ID.to_string(),
        recipient: "a@example.com".to_string(),
        kind: "verification".to_string(),
        state: state.to_string(),
        attempts,
        last_error: last_error.map(str::to_string),
        next_attempt_in_secs: None,
        created_at_micros: 0,
    }
}

#[test]
fn a_row_shows_its_tab_and_only_that_tabs_actions() {
    let failed = Some("mail provider unavailable: transient error (421)");
    for (state, attempts, last_error, shown, actions) in [
        (STATE_PENDING, 0, None, "pending", vec!["cancel"]),
        (STATE_PENDING, 1, None, "pending", vec!["cancel"]),
        (STATE_PENDING, 1, failed, "retrying", vec!["retry-now", "park", "cancel"]),
        (STATE_PARKED, 3, failed, "parked", vec!["resume", "cancel-parked"]),
        (STATE_SENT, 1, None, "sent", vec![]),
        (STATE_CANCELLED, 0, None, "cancelled", vec![]),
    ] {
        let row = listed(state, attempts, last_error);
        assert_eq!(shown_state(&row), shown, "{state} after {attempts} attempts, last error {last_error:?}");
        assert_eq!(row_actions(shown_state(&row)), actions, "{shown}");
    }
}

#[test]
fn next_attempt_is_relative_and_now_once_due() {
    for (secs, want) in [
        (-5, "now"),
        (0, "now"),
        (59, "in 59 s"),
        (60, "in 1 min"),
        (3_599, "in 59 min"),
        (3_600, "in 1 h"),
        (86_399, "in 23 h"),
        (86_400, "in 1 d"),
    ] {
        assert_eq!(next_attempt(secs), want, "{secs} s");
    }
}

#[test]
fn the_attention_tab_counts_retrying_and_parked() {
    let stats = OutboxStats { pending: 1, retrying: 2, parked: 4, sent_24h: 8 };
    assert_eq!(tab_counts(&stats), [6, 1, 2, 4, 8]);
}

/// Every refusal decided before the store: the offline provider proves none read it.
#[tokio::test]
async fn refusals_decided_before_the_store() {
    let p = offline();
    let status = |r: Result<InvokeOutcome, opsapi::Error>| r.expect_err("refused").status;
    for action in ["pause-sending", "retry-now", "park", "cancel-parked"] {
        assert_eq!(status(p.invoke(call(action, &[], Some(ID), None)).await), Status::Forbidden, "{action}");
    }
    assert_eq!(status(p.invoke(call("nope", &[], None, None)).await), Status::NotFound);
    assert_eq!(status(p.invoke(call("cancel", &[], None, None)).await), Status::Invalid);
    assert_eq!(status(p.invoke(call("resume", &[], None, None)).await), Status::Invalid);
    assert_eq!(status(p.invoke(call("test-send", &[("to", "a@example.com")], None, None)).await), Status::Invalid);
    match p.invoke(call("test-send", &[("to", "not an address")], None, Some("r1"))).await.unwrap() {
        InvokeOutcome::FieldErrors(errors) => assert_eq!(errors.keys().collect::<Vec<_>>(), ["to"]),
        other => panic!("a bad recipient is a field error, got {other:?}"),
    }
    let panel = p.panel(query("outbox", Some("failed"), None)).await.expect_err("unknown state");
    assert_eq!(panel.status, Status::Invalid);
    assert_eq!(p.panel(query("nope", None, None)).await.expect_err("unknown").status, Status::NotFound);
    let mut other = prepare(Some(ID));
    other.action = "resume".to_string();
    assert_eq!(p.prepare(other).await.expect_err("unprepared").status, Status::NotFound);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_test_send_queues_once_per_request_and_refuses_a_second_recipient() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let p = admin(wired(&pool).await);
    let request_id = unique_key(&pool, "test-send").await;
    let key = format!("{TEST_KEY_PREFIX}{request_id}");
    let _cleanup = cleanup(&pool, &[&key]);
    let send = |to: &'static str| p.invoke(call("test-send", &[("to", to)], None, Some(&request_id)));

    match send(" ops@example.com ").await.unwrap() {
        InvokeOutcome::Ok { message, .. } => assert_eq!(message, "Test message queued for ops@example.com"),
        other => panic!("first submit queues, got {other:?}"),
    }
    assert_eq!(row_of(&pool, &key).await.0, STATE_PENDING);
    let (recipient, kind): (String, String) =
        sqlx::query_as("SELECT recipient, kind FROM mail.outbox WHERE idempotency_key = $1")
            .bind(&key)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((recipient.as_str(), kind.as_str()), ("ops@example.com", "admin.test"), "stored trimmed");
    assert!(matches!(send("ops@example.com").await.unwrap(), InvokeOutcome::NoOp { .. }), "a repeat is a no-op");
    let conflict = send("other@example.com").await.expect_err("another recipient under one request");
    assert_eq!(conflict.status, Status::Conflict);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_prepares_from_the_row_then_moves_a_pending_row_once() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let svc = wired(&pool).await;
    let p = admin(svc.clone());
    let key = unique_key(&pool, "admin-cancel").await;
    let _cleanup = cleanup(&pool, &[&key]);
    enqueue(&svc, &pool, &mail_of(&key, "hello")).await;
    let id = id_of(&pool, &key).await;

    match p.prepare(prepare(Some(&id))).await.unwrap() {
        Then::Confirm(spec) => {
            let impact: Vec<_> = spec.impact.iter().map(|(k, c)| (k.as_str(), c.value.as_str())).collect();
            assert_eq!(impact, [("Recipient", "player@example.com"), ("Template", "verification")]);
        }
        other => panic!("cancel prepares a confirm, got {other:?}"),
    }
    assert!(matches!(p.invoke(call("cancel", &[], Some(&id), None)).await.unwrap(), InvokeOutcome::Ok { .. }));
    assert_eq!(row_of(&pool, &key).await.0, STATE_CANCELLED);
    assert!(matches!(p.invoke(call("cancel", &[], Some(&id), None)).await.unwrap(), InvokeOutcome::NoOp { .. }));

    sqlx::query("DELETE FROM mail.outbox WHERE idempotency_key = $1").bind(&key).execute(&pool).await.unwrap();
    assert_eq!(p.prepare(prepare(Some(&id))).await.expect_err("gone").status, Status::NotFound);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_moves_a_parked_row_once() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let svc = wired(&pool).await;
    let p = admin(svc.clone());
    let key = unique_key(&pool, "admin-resume").await;
    let _cleanup = cleanup(&pool, &[&key]);
    enqueue(&svc, &pool, &mail_of(&key, "hello")).await;
    let id = id_of(&pool, &key).await;
    // Not parked yet: a resume of a pending row moves nothing.
    assert!(matches!(p.invoke(call("resume", &[], Some(&id), None)).await.unwrap(), InvokeOutcome::NoOp { .. }));

    sqlx::query("UPDATE mail.outbox SET state = 'parked' WHERE idempotency_key = $1")
        .bind(&key)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(p.invoke(call("resume", &[], Some(&id), None)).await.unwrap(), InvokeOutcome::Ok { .. }));
    assert_eq!(row_of(&pool, &key).await.0, STATE_PENDING);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_outbox_maps_a_row_and_drops_the_total_when_narrowed() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let svc = wired(&pool).await;
    let p = admin(svc.clone());
    let key = unique_key(&pool, "admin-outbox").await;
    let _cleanup = cleanup(&pool, &[&key]);
    enqueue(&svc, &pool, &mail_of(&key, "hello")).await;
    let id = id_of(&pool, &key).await;

    let queue = |body: controlplane::PanelBody| match body.primitive {
        Primitive::WorkQueue(q) => q,
        other => panic!("the outbox is a work queue, got {other:?}"),
    };
    let narrowed: WorkQueue = queue(p.panel(query("outbox", Some("pending"), Some(&id))).await.unwrap());
    assert_eq!(narrowed.selected, "pending");
    assert_eq!(narrowed.page.total, None, "a search has no total");
    let [item] = narrowed.items.as_slice() else { panic!("the id search finds its one row") };
    assert_eq!((item.id.as_str(), item.state.as_str()), (id.as_str(), "pending"));
    assert_eq!(item.actions, ["cancel"]);
    assert_eq!(item.max_attempts, DEFAULT_MAX_ATTEMPTS as u32);
    assert_eq!(item.cells[0].value, "player@example.com");
    assert_eq!(item.cells[1].value, "verification");
    let to = BTreeMap::from([
        (PAGE_PARAM_SEARCH.to_string(), "player@example.com".to_string()),
        (PAGE_PARAM_QUEUE_STATE.to_string(), "pending".to_string()),
    ]);
    assert_eq!(
        item.cells[0].link,
        Some(Link::Page { module: "mail".to_string(), page: "outbox".to_string(), params: to })
    );

    let whole = queue(p.panel(query("outbox", None, None)).await.unwrap());
    assert_eq!(whole.selected, "attention", "no state selects the first tab");
    let counts: Vec<u64> = whole.states.iter().map(|s| s.count).collect();
    assert_eq!(whole.page.total, Some(counts[0]), "an unnarrowed page totals its tab");

    let pending = queue(p.panel(query("outbox", Some("pending"), None)).await.unwrap());
    assert_eq!(pending.selected, "pending");
    assert!(pending.states[1].count >= 1, "this test's row is pending");
    assert_eq!(pending.page.total, Some(pending.states[1].count), "the total is the SELECTED tab's count");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_template_filter_pages_past_a_full_page_by_cursor() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let svc = wired(&pool).await;
    let p = admin(svc.clone());
    // A kind no other row holds, so the filter alone selects exactly this test's rows.
    let kind = unique_key(&pool, "page").await;
    let mut keys = Vec::new();
    for _ in 0..=PAGE_SIZE {
        keys.push(unique_key(&pool, "admin-page").await);
    }
    let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
    let _cleanup = cleanup(&pool, &refs);
    let mut ids = Vec::new();
    for key in &keys {
        enqueue(&svc, &pool, &NewMail { kind: &kind, ..mail_of(key, "hello") }).await;
        ids.push(id_of(&pool, key).await);
    }

    let page = |cursor: Option<String>| {
        let mut q = query("outbox", Some("pending"), None);
        q.params.insert(FILTER_TEMPLATE.to_string(), kind.clone());
        q.cursor = cursor;
        let p = &p;
        async move {
            match p.panel(q).await.unwrap().primitive {
                Primitive::WorkQueue(q) => q,
                other => panic!("the outbox is a work queue, got {other:?}"),
            }
        }
    };
    let first = page(None).await;
    assert_eq!(first.items.len(), PAGE_SIZE, "one row past the page is held back");
    assert_eq!(first.page.total, None, "a template filter has no total");
    assert!(first.items.iter().all(|i| i.cells[1].value == kind), "only the filtered template");
    let cursor = first.page.next_cursor.clone().expect("a row past the page leaves a cursor");

    let second = page(Some(cursor)).await;
    let [last] = second.items.as_slice() else { panic!("the cursor resumes at the one row left") };
    assert_eq!(second.page.next_cursor, None, "the last page has no cursor");
    let mut seen: Vec<String> = first.items.iter().chain([last]).map(|i| i.id.clone()).collect();
    seen.sort();
    ids.sort();
    assert_eq!(seen, ids, "the two pages cover every row once");
}

/// D6: a row claimed for its first send has burnt an attempt but failed none, so it stays
/// pending in the tab predicates, the stats count and `shown_state`; only a failed attempt's
/// `last_error` makes it retrying.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_row_claimed_for_its_first_send_is_pending_until_an_attempt_fails() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let svc = wired(&pool).await;
    let p = admin(svc.clone());
    let key = unique_key(&pool, "admin-first-send").await;
    let _cleanup = cleanup(&pool, &[&key]);
    enqueue(&svc, &pool, &mail_of(&key, "hello")).await;
    quarantine_foreign_pending(&pool, &key).await;
    let id = id_of(&pool, &key).await;
    let before = svc.outbox_stats().await.unwrap();

    let mut tx = bounded_tx(&pool, std::time::Duration::from_secs(5)).await.unwrap();
    let claimed = Store.claim_due_tx(&mut tx, 30.0).await.unwrap().expect("this test's row is due");
    tx.commit().await.unwrap();
    assert_eq!((claimed.id.as_str(), claimed.attempts), (id.as_str(), 1), "the claim burnt the first attempt");

    let tab = |state: &'static str| {
        let p = &p;
        let id = id.clone();
        async move {
            match p.panel(query("outbox", Some(state), Some(&id))).await.unwrap().primitive {
                Primitive::WorkQueue(q) => q.items,
                other => panic!("the outbox is a work queue, got {other:?}"),
            }
        }
    };
    let in_flight = svc.outbox_stats().await.unwrap();
    assert_eq!(
        (in_flight.pending, in_flight.retrying),
        (before.pending, before.retrying),
        "a first send is not retrying"
    );
    let pending = tab("pending").await;
    let [item] = pending.as_slice() else { panic!("the claimed row lists under Pending, got {pending:?}") };
    assert_eq!((item.state.as_str(), item.attempts), ("pending", 1));
    assert_eq!(item.actions, ["cancel"]);
    assert!(tab("retrying").await.is_empty(), "a first send is not under Retrying");
    assert!(tab("attention").await.is_empty(), "a first send needs no attention");

    let failure = Disposition::Retry {
        last_error: "mail provider unavailable: transient error (421)".to_string(),
        backoff_secs: 60.0,
    };
    let mut tx = bounded_tx(&pool, std::time::Duration::from_secs(5)).await.unwrap();
    assert_eq!(Store.finish_tx(&mut tx, &claimed, &failure, "fake").await.unwrap(), 1);
    tx.commit().await.unwrap();

    let failed = svc.outbox_stats().await.unwrap();
    assert_eq!(
        (failed.pending, failed.retrying),
        (before.pending - 1, before.retrying + 1),
        "a failed attempt is retrying"
    );
    assert!(tab("pending").await.is_empty(), "a failed row leaves Pending");
    for state in ["retrying", "attention"] {
        let items = tab(state).await;
        let [item] = items.as_slice() else { panic!("the failed row lists once under {state}, got {items:?}") };
        assert_eq!(item.state, "retrying", "under {state}");
        assert_eq!(item.actions, ["retry-now", "park", "cancel"]);
        assert_eq!(item.last_error.as_deref(), Some("mail provider unavailable: transient error (421)"));
    }
}
