//! The scheduler provider: the manifest, the Schedules and Recent fires bodies and the Fire
//! now guards; and, over a live Postgres, the next-fire and last-fire text the SQL computes
//! (overdue, upcoming, never fired, a legacy out-of-range interval), the 24-hour fire window,
//! and Fire now's fire, repeat, held lock, missing row and disabled refusal through
//! `Provider::invoke`.

use controlplane::{
    order, ActionPoint, CellKind, DataTable, Layout, PanelBody, PanelQuery, Point, Primitive, Provider, Role, Then,
    Timeline, Tone,
};
use opsapi::Status;
use sqlx::{Connection, PgConnection, PgPool};

use super::*;
use crate::store::{FireRow, ScheduleRow};
use crate::tests::{bus_with_fake, cleanup, test_pool, TICK_TEST_LOCK};
use crate::{lock_key, INTERVAL_SECONDS_CEILING};

const DEAD_DSN: &str = "postgres://gamebackend:gamebackend@127.0.0.1:1/gamebackend?sslmode=disable";

/// A provider on a pool that cannot connect: only an answer given before any I/O comes back
/// as anything but `Internal`.
fn offline(enabled: bool) -> SchedulerAdmin {
    SchedulerAdmin::new(PgPool::connect_lazy(DEAD_DSN).unwrap(), Arc::new(Bus::new()), enabled)
}

fn table(body: &PanelBody) -> &DataTable {
    match &body.primitive {
        Primitive::DataTable(t) => t,
        other => panic!("a DataTable, got {other:?}"),
    }
}

fn timeline(body: &PanelBody) -> &Timeline {
    match &body.primitive {
        Primitive::Timeline(t) => t,
        other => panic!("a Timeline, got {other:?}"),
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

fn call(action: &str) -> InvokeCall {
    InvokeCall { action: action.into(), ..fire_call("audit-prune") }
}

fn fire_call(name: &str) -> InvokeCall {
    InvokeCall {
        module: MODULE.into(),
        action: SCHEDULE_FIRE.into(),
        entity: Some(EntityRef { kind: SCHEDULE_KIND.into(), id: name.into() }),
        input: Default::default(),
        expected_revision: None,
        reason: None,
        operator: "tester".into(),
        item: None,
        request_id: None,
    }
}

fn schedule(name: &str, next_fire: Option<&str>, last_at: Option<&str>) -> ScheduleRow {
    ScheduleRow {
        name: name.to_string(),
        interval_seconds: 86_400,
        next_fire: next_fire.map(String::from),
        last_at: last_at.map(String::from),
    }
}

#[test]
fn manifest_declares_the_page_and_its_one_writer() {
    let m = manifest();
    assert_eq!(m.module, "scheduler");
    assert!(m.writes, "Fire now writes");
    assert_eq!(m.entries.len(), 1);
    let nav = &m.entries[0];
    assert_eq!((nav.section.as_str(), nav.id.as_str(), nav.label.as_str()), ("platform", "scheduler", "Scheduler"));
    assert_eq!((nav.icon.as_str(), nav.page.as_str(), nav.counter.as_deref()), ("clock", PAGE, None));
    assert_eq!(nav.order, order::platform_nav::SCHEDULER);
    assert_eq!(PAGE, "schedules");
    let page = &m.pages[0];
    assert_eq!((page.title.as_str(), page.subtitle.as_str()), ("Scheduler", SUBTITLE));
    assert!(page.head_actions.is_empty() && page.tiles.is_empty() && page.params.is_empty());
    assert_eq!(page.layout, Layout::Stack(vec![SCHEDULES.to_string(), FIRES.to_string()]));
    let ids: Vec<(&str, i32, &str)> =
        m.contributions.iter().map(|c| (c.id.as_str(), c.order, c.title.as_str())).collect();
    assert_eq!(
        ids,
        [
            (SCHEDULES, 10, "Schedules"),
            (FIRES, 20, "Recent fires"),
        ]
    );
    assert_eq!(m.contributions.len(), 2);
    for c in &m.contributions {
        assert_eq!(c.point, Point::Page(PAGE.to_string()));
        assert_eq!((c.min_role, c.readonly.as_deref(), c.cache_ttl_s), (Role::Viewer, None, None));
    }
    assert_eq!(m.entity_kinds.len(), 1);
    assert_eq!((m.entity_kinds[0].kind.as_str(), m.entity_kinds[0].label.as_str()), (SCHEDULE_KIND, "Schedule"));
    assert!(m.search.is_empty());
}

#[test]
fn fire_now_is_the_one_implemented_invoke() {
    let m = manifest();
    let row = ActionPoint::RowAction(SCHEDULE_KIND.to_string());
    let got: Vec<(&str, &ActionPoint, &str, &str, i32, bool, bool)> = m
        .actions
        .iter()
        .map(|a| (a.id.as_str(), &a.point, a.label.as_str(), a.icon.as_str(), a.order, a.danger, a.when_empty))
        .collect();
    assert_eq!(got, [(SCHEDULE_FIRE, &row, "Fire now", "zap", 10, false, false)]);
    for a in &m.actions {
        assert!(a.implemented, "{} is implemented", a.id);
        assert_eq!(a.then, Then::Invoke, "{} opens nothing", a.id);
        assert_eq!(a.min_role, Role::Operator, "{}", a.id);
        assert!(!a.prepare, "{}", a.id);
    }
}

/// Every guard answers before any I/O: the provider's pool cannot connect, so a guard that
/// let the call through would answer `Internal`.
#[tokio::test]
async fn invoke_guards_answer_before_any_io() {
    let p = offline(true);
    for action in ["schedule.create", "schedule.edit", "schedule.delete", "schedule.pause"] {
        assert_eq!(p.invoke(call(action)).await.unwrap_err().status, Status::NotFound, "{action}");
    }
    assert_eq!(p.panel(query("nope")).await.unwrap_err().status, Status::NotFound);

    let mut over = fire_call("audit-prune");
    over.input.insert("value".into(), "a".repeat(controlplane::MAX_INPUT_VALUE_BYTES + 1));
    assert_eq!(p.invoke(over).await.unwrap_err().status, Status::Invalid, "an over-cap input value");
    for entity in [
        None,
        Some(EntityRef { kind: "player".into(), id: "audit-prune".into() }),
        Some(EntityRef { kind: SCHEDULE_KIND.into(), id: String::new() }),
    ] {
        let req = InvokeCall { entity: entity.clone(), ..fire_call("audit-prune") };
        assert_eq!(p.invoke(req).await.unwrap_err().status, Status::Invalid, "{entity:?}");
    }

    let refused = offline(false).invoke(fire_call("audit-prune")).await.unwrap_err();
    assert_eq!((refused.status, refused.msg.as_str()), (Status::Forbidden, DISABLED_REFUSAL));

    let _held = p.firing.lock().await;
    let busy = p.invoke(fire_call("audit-prune")).await.unwrap_err();
    assert_eq!((busy.status, busy.msg.as_str()), (Status::Conflict, FIRE_BUSY));
}

#[test]
fn every_names_the_largest_whole_unit() {
    assert_eq!(every(86_400), "every 24 h");
    assert_eq!(every(3_600), "every 1 h");
    assert_eq!(every(300), "every 5 min");
    assert_eq!(every(90), "every 90 s");
    assert_eq!(every(2), "every 2 s");
}

#[test]
fn schedules_body_draws_each_column() {
    let rows = [
        schedule("ok", Some("due"), Some("03:30")),
        schedule("later", Some("2026-11-24 03:30"), Some("03:30")),
        schedule("never", Some("due"), None),
        schedule("legacy", None, Some("12:00")),
    ];
    let body = schedules_body(&rows, 4, true);
    assert_eq!(body.meta.as_deref(), Some("4"));
    assert_eq!((body.foot.as_deref(), body.banner.as_ref()), (None, None));
    let t = table(&body);
    let labels: Vec<&str> = t.columns.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, ["Id", "When", "Next fire", "Last"]);
    assert_eq!((t.page.next_cursor.as_deref(), t.page.total), (None, None));
    assert_eq!(t.item_noun.as_ref().map(|n| (n.of(1), n.of(2))), Some(("schedule", "schedules")));

    let r = &t.rows[0];
    let entity = r.entity.as_ref().unwrap();
    assert_eq!((entity.kind.as_str(), entity.id.as_str()), (SCHEDULE_KIND, "ok"));
    let kinds: Vec<CellKind> = r.cells.iter().map(|c| c.kind).collect();
    assert_eq!(kinds, [CellKind::Mono, CellKind::Mono, CellKind::Dim, CellKind::Dim]);
    let values: Vec<&str> = r.cells.iter().map(|c| c.value.as_str()).collect();
    assert_eq!(values, ["ok", "every 24 h", "due", "03:30"]);

    assert_eq!(t.rows[1].cells[2].value, "2026-11-24 03:30");
    assert_eq!(t.rows[2].cells[3].value, NEVER);
    assert_eq!(t.rows[3].cells[2].value, NEVER);
}

#[test]
fn schedules_body_caps_the_rows_and_warns_when_disabled() {
    let rows: Vec<ScheduleRow> = (0..=MAX_ROWS).map(|i| schedule(&format!("s{i:03}"), Some("due"), None)).collect();
    let body = schedules_body(&rows, 250, false);
    assert_eq!(table(&body).rows.len(), MAX_ROWS);
    assert_eq!(body.meta.as_deref(), Some("250"));
    assert_eq!(body.foot.as_deref(), Some("first 100 schedules"));
    let banner = body.banner.expect("a disabled loop warns");
    assert_eq!((banner.tone, banner.text.as_str()), (Tone::Warn, DISABLED_BANNER));

    let body = schedules_body(&rows[..MAX_ROWS], MAX_ROWS as i64, true);
    assert_eq!(table(&body).rows.len(), MAX_ROWS);
    assert_eq!(body.foot, None);

    let body = schedules_body(&[], 0, true);
    assert!(table(&body).rows.is_empty());
    assert_eq!(body.meta.as_deref(), Some("0"));
}

#[test]
fn fires_body_draws_one_plain_entry_per_fire() {
    let rows = [
        FireRow { name: "audit-prune".into(), at: "2026-11-23T03:30:00Z".into() },
        FireRow { name: "mail-sweep".into(), at: "2026-11-23T03:00:00Z".into() },
    ];
    let body = fires_body(&rows);
    assert_eq!(body.tag.as_deref(), Some(FIRES_WINDOW));
    let t = timeline(&body);
    assert!(!t.live && !t.days && !t.date_only && t.cursor_before.is_none());
    assert_eq!(t.entries.len(), 2);
    for (e, (name, at)) in t.entries.iter().zip([("audit-prune", "2026-11-23T03:30:00Z"), ("mail-sweep", "2026-11-23T03:00:00Z")]) {
        assert_eq!((e.at.as_str(), e.verb.as_str(), e.icon.as_str()), (at, name, "zap"));
        assert_eq!((e.tone, e.detail.as_deref(), e.summary.as_str()), (None, None, ""));
        assert!(e.badges.is_empty());
    }

    assert!(timeline(&fires_body(&[])).entries.is_empty());
}

/// A run-unique name sorting before every seeded schedule, so the capped read lists it.
async fn test_name(pool: &PgPool) -> String {
    let (s,): (String,) = sqlx::query_as("SELECT '0-admin-test-' || gen_random_uuid()::text")
        .fetch_one(pool)
        .await
        .unwrap();
    s
}

async fn insert(pool: &PgPool, name: &str, interval_seconds: i64, last_fired: &str) {
    sqlx::query(&format!(
        "INSERT INTO scheduler.schedules (name, interval_seconds, last_fired) VALUES ($1, $2, {last_fired})"
    ))
    .bind(name)
    .bind(interval_seconds)
    .execute(pool)
    .await
    .expect("insert schedule");
}

fn find<'a>(t: &'a DataTable, name: &str) -> &'a controlplane::Row {
    t.rows.iter().find(|r| r.cells[0].value == name).unwrap_or_else(|| panic!("{name} is listed"))
}

/// H5, H10 and S2 over the real SQL: the overdue, upcoming, never-fired and legacy rows list
/// with their next and last fire, and the fire window holds only the last 24 hours, newest
/// first. Holds the tick lock: the never-fired row is due, and a tick test must not fire it.
#[tokio::test(flavor = "multi_thread")]
async fn panels_compute_next_and_last_fire_in_sql() {
    let _tick_guard = TICK_TEST_LOCK.lock().await;
    let Some(pool) = test_pool().await else {
        return;
    };
    let p = SchedulerAdmin::new(pool.clone(), Arc::new(Bus::new()), true);
    let overdue = test_name(&pool).await;
    let upcoming = test_name(&pool).await;
    let never = test_name(&pool).await;
    let recent = test_name(&pool).await;
    let older = test_name(&pool).await;
    let legacy = test_name(&pool).await;
    let far = test_name(&pool).await;

    // Panic residual as in `due_scan_survives_legacy_huge_interval_row`: a failed assert
    // before the restore below leaves the CHECK NOT VALID and the rows behind.
    sqlx::query("ALTER TABLE scheduler.schedules DROP CONSTRAINT schedules_interval_bounds")
        .execute(&pool)
        .await
        .expect("drop CHECK to simulate a pre-CHECK legacy row");
    insert(&pool, &overdue, 60, "'2000-01-01T00:00:00Z'").await;
    insert(&pool, &upcoming, 3_600, "'2999-01-01T00:00:00Z'").await;
    insert(&pool, &never, 60, "to_timestamp(0)").await;
    insert(&pool, &recent, 86_400, "now() - interval '5 minutes'").await;
    insert(&pool, &older, 86_400, "now() - interval '10 minutes'").await;
    insert(&pool, &legacy, INTERVAL_SECONDS_CEILING + 1, "'2000-01-01T00:00:00Z'").await;
    sqlx::query(&format!(
        "ALTER TABLE scheduler.schedules ADD CONSTRAINT schedules_interval_bounds \
         CHECK (interval_seconds > 0 AND interval_seconds <= {INTERVAL_SECONDS_CEILING}) NOT VALID",
    ))
    .execute(&pool)
    .await
    .expect("re-add CHECK as NOT VALID (does not re-validate the legacy row)");
    // Valid under the CHECK, yet `last_fired + interval` lies past the largest timestamp.
    insert(&pool, &far, INTERVAL_SECONDS_CEILING, "now()").await;

    let body = p.panel(query(SCHEDULES)).await.expect("a legacy or far row must not error the panel");
    let count: u64 = body.meta.as_deref().unwrap().parse().unwrap();
    assert!(count >= 7, "meta counts every schedule: {count}");
    assert!(body.banner.is_none());
    let t = table(&body);
    let row = find(t, &overdue);
    assert_eq!((row.cells[1].value.as_str(), row.cells[2].value.as_str()), ("every 1 min", "due"));
    assert_eq!(row.cells[3].value, "00:00");
    let row = find(t, &upcoming);
    assert_eq!((row.cells[2].value.as_str(), row.cells[3].value.as_str()), ("2999-01-01 01:00", "00:00"));
    let row = find(t, &never);
    assert_eq!((row.cells[2].value.as_str(), row.cells[3].value.as_str()), ("due", NEVER));
    assert_eq!(find(t, &legacy).cells[2].value, NEVER);
    assert_eq!(find(t, &far).cells[2].value, NEVER, "a next fire past the largest timestamp");

    let fires = p.panel(query(FIRES)).await.unwrap();
    let names: Vec<&str> = timeline(&fires).entries.iter().map(|e| e.verb.as_str()).collect();
    let at = |n: &str| names.iter().position(|v| *v == n);
    assert!(at(&recent).unwrap() < at(&older).unwrap(), "newest first: {names:?}");
    for outside in [&overdue, &upcoming, &never, &legacy] {
        assert_eq!(at(outside), None, "{outside} did not fire in the last 24 h");
    }
    let entry = &timeline(&fires).entries[at(&recent).unwrap()];
    assert!(entry.at.ends_with('Z') && entry.at.len() == 20, "RFC 3339 UTC: {}", entry.at);

    for n in [&overdue, &upcoming, &never, &recent, &older, &legacy, &far] {
        cleanup(&pool, n).await;
    }
    sqlx::query("ALTER TABLE scheduler.schedules VALIDATE CONSTRAINT schedules_interval_bounds")
        .execute(&pool)
        .await
        .expect("re-validate the CHECK after removing the legacy row");
}

#[tokio::test]
async fn disabled_scheduler_warns_on_the_live_panel() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let body = SchedulerAdmin::new(pool, Arc::new(Bus::new()), false).panel(query(SCHEDULES)).await.unwrap();
    assert_eq!(body.banner.map(|b| b.text), Some(DISABLED_BANNER.to_string()));
}

/// D8 over the real fire path: Fire now fires a schedule that is not due and stamps its
/// `last_fired`, a repeat fires again, a held advisory lock is a Conflict and a deleted row
/// NotFound, both with nothing emitted, and a disabled process refuses with nothing emitted
/// or stamped.
#[tokio::test(flavor = "multi_thread")]
async fn fire_now_fires_on_demand_and_answers_each_outcome() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let name = test_name(&pool).await;
    insert(&pool, &name, 3_600, "now() - interval '1 minute'").await;
    let (bus, ft) = bus_with_fake();
    let p = SchedulerAdmin::new(pool.clone(), bus.clone(), true);
    let age = || async {
        sqlx::query_scalar::<_, f64>(
            "SELECT extract(epoch FROM now() - last_fired)::float8 FROM scheduler.schedules WHERE name = $1",
        )
        .bind(&name)
        .fetch_one(&pool)
        .await
        .unwrap()
    };

    let refused = SchedulerAdmin::new(pool.clone(), bus.clone(), false).invoke(fire_call(&name)).await.unwrap_err();
    assert_eq!(refused.status, Status::Forbidden);
    assert_eq!(ft.count(&name), 0, "a disabled process fires nothing");
    assert!(age().await >= 59.0, "a disabled process stamps nothing");

    for n in 1..=2 {
        match p.invoke(fire_call(&name)).await.unwrap() {
            InvokeOutcome::Ok { message, .. } => assert_eq!(message, format!("Fired {name}")),
            other => panic!("fire {n}: {other:?}"),
        }
        assert_eq!(ft.count(&name), n, "fire {n} emits once");
        assert!(age().await < 30.0, "fire {n} stamped last_fired");
    }

    let mut holder = PgConnection::connect_with(&pool.connect_options()).await.unwrap();
    let held: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(lock_key(&name))
        .fetch_one(&mut holder)
        .await
        .unwrap();
    assert!(held);
    assert_eq!(p.invoke(fire_call(&name)).await.unwrap_err().status, Status::Conflict);
    assert_eq!(ft.count(&name), 2, "a held lock fires nothing");
    holder.close().await.unwrap();

    cleanup(&pool, &name).await;
    assert_eq!(p.invoke(fire_call(&name)).await.unwrap_err().status, Status::NotFound);
    assert_eq!(ft.count(&name), 2, "a deleted schedule fires nothing");
}
