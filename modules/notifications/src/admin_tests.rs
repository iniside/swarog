//! The control-plane provider: the Inbox tab's meta, entries and paging, the tab count, the
//! purge attention item and the manifest; and, over a live Postgres, the tab's pages and
//! counts through `Provider::panel`.

use controlplane::{
    order, ActionPoint, AttentionItem, CellKind, EntityRef, FieldKind, InvokeCall, InvokeOutcome, Link, PanelBody,
    PanelQuery, Point, Primitive, Provider, Role, Severity, Then, Timeline, Tone,
};
use notificationsapi::{Notification, MAX_BODY_BYTES, MAX_TITLE_BYTES};
use opsapi::Status;
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::admin::{self, NotificationsAdmin, PAGE_SIZE};
use crate::projection::{
    KIND_ACCOUNT_PROMOTED, KIND_FRIEND_ACCEPTED, KIND_FRIEND_REQUESTED, KIND_WALLET_CREDIT, PURGE_SWEEP_SUB,
};
use crate::service::{decode_cursor, encode_cursor, is_operator_dedup_key, Service, PLAYER_DELETED};
use crate::tests::{cleanup, operator_key, unique_player, wired, DB_LOCK};

fn message(i: usize, kind: &str, read: bool) -> Notification {
    Notification {
        id: format!("00000000-0000-4000-8000-{i:012}"),
        kind: kind.to_string(),
        title: format!("Title {i}"),
        body: format!("Body {i}"),
        created_at: format!("2026-11-22T12:{:02}:00.000000Z", 59 - i % 60),
        read_at: if read { "2026-11-22T13:00:00.000000Z".to_string() } else { String::new() },
    }
}

fn timeline(body: &PanelBody) -> &Timeline {
    match &body.primitive {
        Primitive::Timeline(t) => t,
        other => panic!("the Inbox tab is a Timeline, got {other:?}"),
    }
}

fn query(contribution: &str, id: &str, before: Option<&str>) -> PanelQuery {
    PanelQuery {
        module: "notifications".into(),
        contribution: contribution.into(),
        entity: Some(EntityRef { kind: "player".into(), id: id.into() }),
        search: None,
        cursor: None,
        before: before.map(String::from),
        queue_state: None,
        range: None,
        params: Default::default(),
        refs: Vec::new(),
    }
}

#[test]
fn meta_counts_every_message_and_the_unread_ones() {
    assert_eq!(admin::meta_text(4, 1), "4 · 1 unread");
    assert_eq!(admin::meta_text(0, 0), "0 · 0 unread");
}

#[test]
fn an_entry_is_the_bold_title_the_body_and_the_kind_with_its_read_state() {
    let unread = admin::entry(&message(1, KIND_WALLET_CREDIT, false));
    assert_eq!(unread.at, "2026-11-22T12:58:00.000000Z");
    assert_eq!(unread.title.as_deref(), Some("Title 1"));
    assert_eq!(unread.summary, "Body 1");
    assert_eq!(unread.detail.as_deref(), Some("wallet.credit · unread"));
    assert_eq!(unread.tone, Some(Tone::Accent));
    assert_eq!(unread.icon, "gift");
    assert!(unread.actor.is_none() && unread.target.is_none());
    assert!(unread.verb.is_empty() && unread.badges.is_empty());

    let read = admin::entry(&message(2, KIND_FRIEND_ACCEPTED, true));
    assert_eq!(read.detail.as_deref(), Some("friend.accepted · read"));
    assert_eq!(read.tone, None);
    assert_eq!(read.icon, "heart");
}

#[test]
fn each_kind_has_its_icon_and_an_unknown_kind_is_the_bell() {
    let cases = [
        (KIND_WALLET_CREDIT, "gift"),
        (KIND_FRIEND_REQUESTED, "heart"),
        (KIND_FRIEND_ACCEPTED, "heart"),
        (admin::KIND_OPERATOR_MAIL, "mail"),
        (KIND_ACCOUNT_PROMOTED, "bell"),
        ("some.future.kind", "bell"),
    ];
    for (kind, icon) in cases {
        assert_eq!(admin::kind_icon(kind), icon, "{kind}");
    }
}

#[test]
fn a_full_page_without_a_surplus_row_has_no_older_cursor() {
    let rows: Vec<Notification> = (0..PAGE_SIZE).map(|i| message(i, KIND_WALLET_CREDIT, true)).collect();
    let body = admin::inbox_body(&rows, 25, 0);
    let t = timeline(&body);
    assert_eq!(t.entries.len(), 25);
    assert_eq!(t.cursor_before, None);
    assert!(t.days && !t.date_only && !t.live);
    assert_eq!(body.meta.as_deref(), Some("25 · 0 unread"));
}

#[test]
fn a_surplus_row_is_not_drawn_and_sets_the_cursor_to_the_last_drawn_row() {
    let rows: Vec<Notification> = (0..=PAGE_SIZE).map(|i| message(i, KIND_WALLET_CREDIT, false)).collect();
    let body = admin::inbox_body(&rows, 40, 40);
    let t = timeline(&body);
    assert_eq!(t.entries.len(), 25);
    let last = &rows[PAGE_SIZE - 1];
    assert_eq!(t.cursor_before, Some(encode_cursor(&last.created_at, &last.id)));
    assert_eq!(t.entries.last().map(|e| e.title.as_deref()), Some(Some("Title 24")));
}

#[test]
fn an_empty_inbox_is_an_empty_timeline_with_a_zero_meta() {
    let body = admin::inbox_body(&[], 0, 0);
    let t = timeline(&body);
    assert!(t.entries.is_empty());
    assert_eq!(t.cursor_before, None);
    assert_eq!(body.meta.as_deref(), Some("0 · 0 unread"));
}

#[test]
fn purges_body_is_empty_at_zero_and_one_warn_item_linking_the_sweep_otherwise() {
    match admin::purges_body(0, "").primitive {
        Primitive::AttentionItems(items) => assert!(items.is_empty()),
        other => panic!("an attention item list, got {other:?}"),
    }
    let items = match admin::purges_body(2, "2026-11-20").primitive {
        Primitive::AttentionItems(items) => items,
        other => panic!("an attention item list, got {other:?}"),
    };
    let [AttentionItem { severity, title, text, link, .. }] = items.as_slice() else {
        panic!("one item, got {items:?}")
    };
    assert_eq!(*severity, Severity::Warn);
    assert_eq!(title, "Notifications purges pending");
    assert_eq!(text, "2 players awaiting sweep · oldest 2026-11-20");
    match link {
        Link::Page { params, .. } => assert_eq!(params.values().next().map(String::as_str), Some(PURGE_SWEEP_SUB.id)),
        other => panic!("a page link, got {other:?}"),
    }
}

#[test]
fn manifest_declares_the_tab_its_count_and_the_purges_item_at_the_shared_orders() {
    let m = admin::manifest();
    assert_eq!(m.module, "notifications");
    assert!(m.writes);
    let decls: Vec<(&str, &Point, i32)> = m.contributions.iter().map(|c| (c.id.as_str(), &c.point, c.order)).collect();
    assert_eq!(decls.len(), 3);
    match decls[0] {
        (admin::INBOX, Point::EntityTab { kind, tab, tab_label, columns, params }, o) => {
            assert_eq!((kind.as_str(), tab.as_str(), tab_label.as_str(), *columns), ("player", "inbox", "Inbox", 1));
            assert!(params.is_empty());
            assert_eq!(o, order::player_tab::NOTIFICATIONS);
        }
        other => panic!("the Inbox tab first, got {other:?}"),
    }
    match decls[1] {
        (admin::TAB_COUNT, Point::EntityTabCount { kind, tab }, 0) => {
            assert_eq!((kind.as_str(), tab.as_str()), ("player", "inbox"));
        }
        other => panic!("the tab count second, got {other:?}"),
    }
    assert_eq!(
        (decls[2].0, decls[2].1, decls[2].2),
        (admin::ATTENTION_PURGES, &Point::DashboardAttention, order::dashboard_attention::NOTIFICATIONS_PURGES)
    );
    for c in &m.contributions {
        assert_eq!(c.min_role, Role::Viewer, "{}", c.id);
        assert!(c.readonly.is_none() && c.cache_ttl_s.is_none(), "{}", c.id);
    }
}

/// 26 messages: the first page draws 25 with an older cursor, the older page the one left
/// and no cursor; the meta and the tab count read every message and the unread ones.
#[tokio::test]
async fn inbox_tab_pages_one_player_by_25_with_the_player_counts() {
    let Some(pool) = testdb::test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (_ctx, svc) = wired(&pool).await;
    let pid = unique_player(&pool).await;
    let other = unique_player(&pool).await;
    let key = operator_key(&pool).await;
    sqlx::query(
        "INSERT INTO notifications.messages \
             (id, player_id, kind, title, body, created_at, read_at, source_event_id) \
         SELECT gen_random_uuid(), $1::uuid, 'operator.mail', 'm' || n, 'b', \
                timestamptz '2026-01-02 03:04:05+00' + n * interval '1 minute', \
                CASE WHEN n % 2 = 0 THEN now() END, $2 || '-' || n \
           FROM generate_series(1, 26) AS n \
         UNION ALL \
         SELECT gen_random_uuid(), $3::uuid, 'operator.mail', 'x', 'b', now(), NULL, $2 || '-x'",
    )
    .bind(&pid)
    .bind(&key)
    .bind(&other)
    .execute(&pool)
    .await
    .unwrap();
    let provider = NotificationsAdmin::new(svc);

    let first = provider.panel(query(admin::INBOX, &pid, None)).await.unwrap();
    assert_eq!(first.meta.as_deref(), Some("26 · 13 unread"));
    let t = timeline(&first);
    assert_eq!(t.entries.len(), 25);
    assert_eq!(t.entries[0].title.as_deref(), Some("m26"), "newest first");
    assert_eq!(t.entries[0].tone, None, "m26 is read");
    assert_eq!(t.entries[1].tone, Some(Tone::Accent), "m25 is unread");
    let before = t.cursor_before.clone().expect("a 26th row means an older page");
    assert!(decode_cursor(&before).unwrap().is_some());

    let older = provider.panel(query(admin::INBOX, &pid, Some(&before))).await.unwrap();
    let t = timeline(&older);
    assert_eq!(t.entries.iter().map(|e| e.title.clone().unwrap()).collect::<Vec<_>>(), vec!["m1".to_string()]);
    assert_eq!(t.cursor_before, None);

    let count = provider.panel(query(admin::TAB_COUNT, &pid, None)).await.unwrap();
    match count.primitive {
        Primitive::Cell(c) => assert_eq!((c.kind, c.value.as_str()), (CellKind::Number, "26")),
        other => panic!("the tab count is a cell, got {other:?}"),
    }
    let fresh = unique_player(&pool).await;
    match provider.panel(query(admin::TAB_COUNT, &fresh, None)).await.unwrap().primitive {
        Primitive::Cell(c) => assert_eq!(c.value, "0", "a count is a cell at 0 too"),
        other => panic!("the tab count is a cell, got {other:?}"),
    }

    let bad = provider.panel(query(admin::INBOX, "not-a-uuid", None)).await.unwrap_err();
    assert_eq!(bad.status, Status::Invalid);
    let bad = provider.panel(query(admin::INBOX, &pid, Some("%%%"))).await.unwrap_err();
    assert_eq!(bad.status, Status::Invalid);

    cleanup(&pool, &[&pid, &other]).await;
}

/// A provider over a pool that cannot connect: every case below is decided before the store.
fn dead_provider() -> NotificationsAdmin {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_millis(250))
        .connect_lazy("postgres://gamebackend:gamebackend@127.0.0.1:1/gamebackend?sslmode=disable")
        .unwrap();
    NotificationsAdmin::new(Arc::new(Service::new(pool, Arc::new(push::Push::new()))))
}

fn send_call(action: &str, entity: Option<EntityRef>, title: &str, body: &str, request_id: Option<&str>) -> InvokeCall {
    InvokeCall {
        module: "notifications".into(),
        action: action.into(),
        entity,
        input: BTreeMap::from([
            (admin::FIELD_TITLE.to_string(), title.to_string()),
            (admin::FIELD_BODY.to_string(), body.to_string()),
        ]),
        expected_revision: None,
        reason: None,
        operator: "tester".into(),
        item: None,
        request_id: request_id.map(String::from),
    }
}

fn player(id: &str) -> Option<EntityRef> {
    Some(EntityRef { kind: "player".into(), id: id.into() })
}

#[test]
fn send_form_is_title_message_and_a_read_only_request_id() {
    let form = admin::send_form();
    assert_eq!(form.title.as_deref(), Some("Send notification"));
    assert_eq!(form.submit_label, "Send");
    let fields: Vec<(&str, &str, &FieldKind, bool, bool)> =
        form.fields.iter().map(|f| (f.key.as_str(), f.label.as_str(), &f.kind, f.required, f.mono)).collect();
    assert_eq!(
        fields,
        vec![
            ("title", "Title", &FieldKind::String, true, false),
            ("body", "Message", &FieldKind::Text, true, false),
            ("request_id", "Request id", &FieldKind::RequestId, false, true),
        ]
    );
    assert!(form.fields.iter().all(|f| f.default.is_none()));
    assert!(controlplane::form_spec_errors(&form).is_empty());
}

#[test]
fn notify_send_and_the_inbox_tool_open_one_form_as_operator_writes() {
    let m = admin::manifest();
    let actions: Vec<(&str, &ActionPoint, &str, i32, bool)> =
        m.actions.iter().map(|a| (a.id.as_str(), &a.point, a.label.as_str(), a.order, a.when_empty)).collect();
    assert_eq!(
        actions,
        vec![
            (
                admin::NOTIFY,
                &ActionPoint::HeaderAction("player".into()),
                "Notify",
                order::player_header_action::NOTIFICATIONS_NOTIFY,
                false,
            ),
            (
                admin::SEND,
                &ActionPoint::RowAction("player".into()),
                "Send notification…",
                order::player_row_action::NOTIFICATIONS,
                false,
            ),
            (admin::INBOX_SEND, &ActionPoint::PanelTool(admin::INBOX.into()), "Send notification", 0, true),
        ]
    );
    for a in &m.actions {
        assert_eq!(a.then, Then::Form(admin::send_form()), "{}", a.id);
        assert!(a.implemented && !a.danger && !a.prepare && !a.multi, "{}", a.id);
        assert_eq!((a.min_role, a.icon.as_str()), (Role::Operator, "send"), "{}", a.id);
    }
}

#[test]
fn the_dedup_key_is_the_operator_prefix_and_the_request_ids_sha256_head() {
    // SHA-256("abc") = ba7816bf8f01cfea414140de5dae2223b00361a3...
    assert_eq!(admin::operator_dedup_key("abc"), "admin-send-mail-ba7816bf8f01cfea414140de5dae2223");
    let key = admin::operator_dedup_key("send:token-1");
    assert_eq!(key, admin::operator_dedup_key("send:token-1"), "one form instance, one key");
    assert_ne!(key, admin::operator_dedup_key("send:token-2"));
    assert!(is_operator_dedup_key(&key));
}

#[test]
fn title_and_message_are_required_and_capped_at_their_columns() {
    let input = |title: &str, body: &str| {
        BTreeMap::from([
            (admin::FIELD_TITLE.to_string(), title.to_string()),
            (admin::FIELD_BODY.to_string(), body.to_string()),
        ])
    };
    let at_caps = input(&"t".repeat(MAX_TITLE_BYTES), &"b".repeat(MAX_BODY_BYTES));
    assert!(admin::send_fields(&at_caps).is_ok());
    let over = input(&"t".repeat(MAX_TITLE_BYTES + 1), &"b".repeat(MAX_BODY_BYTES + 1));
    let over = admin::send_fields(&over).unwrap_err();
    assert_eq!(
        over,
        BTreeMap::from([
            ("body".to_string(), format!("Message is longer than {MAX_BODY_BYTES} bytes")),
            ("title".to_string(), format!("Title is longer than {MAX_TITLE_BYTES} bytes")),
        ])
    );
    let empty = admin::send_fields(&BTreeMap::from([(admin::FIELD_TITLE.to_string(), " ".to_string())])).unwrap_err();
    assert_eq!(
        empty,
        BTreeMap::from([
            ("body".to_string(), "Message is required".to_string()),
            ("title".to_string(), "Title is required".to_string()),
        ])
    );
}

/// Every refusal decided before the store: field errors, a missing request id, a non-player
/// or absent entity, an unknown action and an over-cap input value.
#[tokio::test]
async fn a_send_is_refused_before_the_store_when_its_input_cannot_be_written() {
    let provider = dead_provider();
    let id = "00000000-0000-4000-8000-000000000001";
    let long = "t".repeat(MAX_TITLE_BYTES + 1);
    match provider.invoke(send_call(admin::NOTIFY, player(id), &long, "b", Some("notify:1"))).await.unwrap() {
        InvokeOutcome::FieldErrors(errors) => assert_eq!(errors.keys().collect::<Vec<_>>(), vec!["title"]),
        other => panic!("an over-cap title is a field error, got {other:?}"),
    }
    let e = provider.invoke(send_call(admin::SEND, player(id), "t", "b", None)).await.unwrap_err();
    assert_eq!(e.status, Status::Invalid);
    let e = provider.invoke(send_call(admin::SEND, player(id), "t", "b", Some(""))).await.unwrap_err();
    assert_eq!(e.status, Status::Invalid);
    let group = Some(EntityRef { kind: "group".into(), id: id.into() });
    for entity in [group, None, player("not-a-uuid")] {
        let e = provider.invoke(send_call(admin::INBOX_SEND, entity, "t", "b", Some("x:1"))).await.unwrap_err();
        assert_eq!(e.status, Status::Invalid);
    }
    let e = provider.invoke(send_call("broadcast", player(id), "t", "b", Some("x:1"))).await.unwrap_err();
    assert_eq!(e.status, Status::NotFound);
    let mut call = send_call(admin::SEND, player(id), "t", "b", Some("x:1"));
    call.input.insert("value".into(), "a".repeat(controlplane::MAX_INPUT_VALUE_BYTES + 1));
    assert_eq!(provider.invoke(call).await.unwrap_err().status, Status::Invalid);
}

/// The Inbox panels are a player's: another entity kind, or none, is refused.
#[tokio::test]
async fn the_inbox_panels_refuse_a_non_player_entity_and_a_missing_one() {
    let provider = dead_provider();
    for contribution in [admin::INBOX, admin::TAB_COUNT] {
        let mut q = query(contribution, "00000000-0000-4000-8000-000000000001", None);
        q.entity = Some(EntityRef { kind: "group".into(), id: q.entity.unwrap().id });
        assert_eq!(provider.panel(q).await.unwrap_err().status, Status::Invalid, "{contribution}");
        let mut q = query(contribution, "00000000-0000-4000-8000-000000000001", None);
        q.entity = None;
        assert_eq!(provider.panel(q).await.unwrap_err().status, Status::Invalid, "{contribution}");
    }
}

async fn inbox_rows(pool: &sqlx::PgPool, pid: &str) -> Vec<(String, String, String)> {
    sqlx::query_as("SELECT kind, title, body FROM notifications.messages WHERE player_id = $1::uuid")
        .bind(pid)
        .fetch_all(pool)
        .await
        .unwrap()
}

/// A send appends one operator.mail row; a double submit of the same form instance is sent
/// and still one row; the same instance with edited text is refused; a deleted player is
/// refused and gets no row.
#[tokio::test]
async fn a_send_appends_once_per_form_instance_and_refuses_a_reused_key_and_a_deleted_player() {
    let Some(pool) = testdb::test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (_ctx, svc) = wired(&pool).await;
    let pid = unique_player(&pool).await;
    let gone = unique_player(&pool).await;
    let provider = NotificationsAdmin::new(svc);
    let request_id = format!("notify:{}", operator_key(&pool).await);

    for _ in 0..2 {
        let call = send_call(admin::NOTIFY, player(&pid), "Hello", "Welcome back", Some(&request_id));
        match provider.invoke(call).await.unwrap() {
            InvokeOutcome::Ok { message, .. } => assert_eq!(message, "Notification sent"),
            other => panic!("a send is Ok, got {other:?}"),
        }
    }
    let want = vec![("operator.mail".to_string(), "Hello".to_string(), "Welcome back".to_string())];
    assert_eq!(inbox_rows(&pool, &pid).await, want, "a double submit writes one row");

    let e = provider
        .invoke(send_call(admin::NOTIFY, player(&pid), "Hello", "Edited", Some(&request_id)))
        .await
        .unwrap_err();
    assert_eq!((e.status, e.msg.as_str()), (Status::Conflict, admin::KEY_REUSED));
    assert_eq!(inbox_rows(&pool, &pid).await, want, "an edited resubmit writes nothing");

    let other = format!("send:{}", operator_key(&pool).await);
    match provider.invoke(send_call(admin::SEND, player(&pid), "Second", "b", Some(&other))).await.unwrap() {
        InvokeOutcome::Ok { .. } => {}
        other => panic!("another form instance is another row, got {other:?}"),
    }
    assert_eq!(inbox_rows(&pool, &pid).await.len(), 2);

    sqlx::query("INSERT INTO notifications.purged_players (player_id) VALUES ($1::uuid)")
        .bind(&gone)
        .execute(&pool)
        .await
        .unwrap();
    let e = provider
        .invoke(send_call(admin::INBOX_SEND, player(&gone), "t", "b", Some("inbox.send:1")))
        .await
        .unwrap_err();
    assert_eq!((e.status, e.msg.as_str()), (Status::Invalid, PLAYER_DELETED));
    assert!(inbox_rows(&pool, &gone).await.is_empty());

    let _ = sqlx::query("DELETE FROM notifications.purged_players WHERE player_id = $1::uuid")
        .bind(&gone)
        .execute(&pool)
        .await;
    cleanup(&pool, &[&pid, &gone]).await;
}
