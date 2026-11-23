//! The control-plane provider: the Inbox tab's meta, entries and paging, the tab count, the
//! purge attention item and the manifest; and, over a live Postgres, the tab's pages and
//! counts through `Provider::panel`.

use controlplane::{
    order, AttentionItem, CellKind, EntityRef, Link, PanelBody, PanelQuery, Point, Primitive, Provider, Role,
    Severity, Timeline, Tone,
};
use notificationsapi::Notification;
use opsapi::Status;

use crate::admin::{self, NotificationsAdmin, PAGE_SIZE};
use crate::projection::{
    KIND_ACCOUNT_PROMOTED, KIND_FRIEND_ACCEPTED, KIND_FRIEND_REQUESTED, KIND_WALLET_CREDIT, PURGE_SWEEP_SUB,
};
use crate::service::{decode_cursor, encode_cursor};
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
    assert!(m.actions.is_empty() && !m.writes);
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
