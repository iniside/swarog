//! The control-plane provider over a live Postgres: the Groups tab's reader and body, its
//! count, the purge attention item, the group header and the manifest.

use std::sync::Arc;

use controlplane::{
    order, Align, CellKind, DataTable, EntityRef, Link, PanelBody, PanelQuery, Point, Primitive, Provider, Role,
};
use groupsapi::{JOIN_INVITE, JOIN_OPEN, ROLE_ADMIN, ROLE_MEMBER, STATE_INVITED, STATE_MEMBER, STATE_REQUESTED};
use opsapi::Status;
use sqlx::PgPool;

use crate::admin;
use crate::store::{AdminMembershipRow, Store, ADMIN_PAGE_SIZE};
use crate::tests::{test_pool, unique_uuid, wired, with_cleanup, FakeDirectory, DB_LOCK};

async fn insert_group(pool: &PgPool, name: &str, policy: &str, created: &str) -> String {
    let id = unique_uuid(pool).await;
    let creator = unique_uuid(pool).await;
    sqlx::query(
        "INSERT INTO groups.groups (id, name, join_policy, creator_id, created_at) \
         VALUES ($1::uuid, $2, $3, $4::uuid, $5::timestamptz)",
    )
    .bind(&id)
    .bind(name)
    .bind(policy)
    .bind(&creator)
    .bind(created)
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn insert_row(pool: &PgPool, group_id: &str, player_id: &str, state: &str, role: &str, created: &str) {
    sqlx::query(
        "INSERT INTO groups.memberships (group_id, player_id, state, role, created_at) \
         VALUES ($1::uuid, $2::uuid, $3, $4, $5::timestamptz)",
    )
    .bind(group_id)
    .bind(player_id)
    .bind(state)
    .bind(role)
    .bind(created)
    .execute(pool)
    .await
    .unwrap();
}

fn query(contribution: &str, kind: &str, id: &str, cursor: Option<&str>) -> PanelQuery {
    PanelQuery {
        module: "groups".into(),
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
        other => panic!("the Groups tab is a DataTable, got {other:?}"),
    }
}

fn item_ids(t: &DataTable) -> Vec<String> {
    t.rows.iter().map(|r| r.item.clone().expect("a row's item is its group")).collect()
}

fn membership(group_id: &str, name: &str, state: &str, role: &str, members: i64) -> AdminMembershipRow {
    AdminMembershipRow {
        group_id: group_id.to_string(),
        name: name.to_string(),
        state: state.to_string(),
        role: role.to_string(),
        members,
        joined: "2026-03-04".to_string(),
        sort_at: "2026-03-04T10:00:00.000000Z".to_string(),
    }
}

#[test]
fn manifest_declares_the_group_kind_its_header_and_the_shared_orders() {
    let m = admin::manifest();
    assert!(!m.writes);
    assert!(m.actions.is_empty());
    assert_eq!(m.entity_kinds.len(), 1);
    assert_eq!(m.entity_kinds[0].kind, admin::GROUP_KIND);
    let find = |id: &str| m.contributions.iter().find(|c| c.id == id).expect(id);
    let tab = find(admin::GROUPS);
    assert_eq!(tab.order, order::player_tab::GROUPS);
    assert!(matches!(&tab.point, Point::EntityTab { kind, tab, tab_label, .. }
        if kind == "player" && tab == "groups" && tab_label == "Groups"));
    assert!(matches!(&find(admin::TAB_COUNT).point, Point::EntityTabCount { kind, tab }
        if kind == "player" && tab == "groups"));
    let purges = find(admin::ATTENTION_PURGES);
    assert_eq!((purges.order, &purges.point), (order::dashboard_attention::GROUPS_PURGES, &Point::DashboardAttention));
    assert_eq!(find(admin::GROUP_HEADER).point, Point::EntityHeader(admin::GROUP_KIND.to_string()));
    assert!(m.contributions.iter().all(|c| c.min_role == Role::Viewer));
}

#[test]
fn groups_body_maps_columns_cells_link_labels_and_meta() {
    let rows = [
        membership("g1", "Alpha", STATE_MEMBER, ROLE_ADMIN, 3),
        membership("g2", "Beta", STATE_MEMBER, ROLE_MEMBER, 1),
        membership("g3", "Gamma", STATE_INVITED, "", 2),
        membership("g4", "Delta", STATE_REQUESTED, "", 5),
    ];
    let body = admin::groups_body(&rows, 4, None);
    assert_eq!(body.meta.as_deref(), Some("4"));
    let t = table(&body);
    let cols: Vec<_> = t.columns.iter().map(|c| (c.key.as_str(), c.label.as_str(), c.kind, c.align)).collect();
    assert_eq!(cols, vec![
        ("group", "Group", CellKind::Text, Align::Left),
        ("role", "Role", CellKind::Label, Align::Left),
        ("members", "Members", CellKind::Number, Align::Right),
        ("joined", "Joined", CellKind::Text, Align::Left),
    ]);
    // One page: no total and no cursor, so the portal draws no foot.
    assert_eq!((t.page.total, t.page.next_cursor.as_deref()), (None, None));
    let first = &t.rows[0];
    assert!(first.entity.is_none(), "rows are not clickable");
    assert_eq!(first.item.as_deref(), Some("g1"));
    assert_eq!((first.cells[0].kind, first.cells[0].value.as_str()), (CellKind::Text, "Alpha"));
    assert_eq!(
        first.cells[0].link,
        Some(Link::Entity(EntityRef { kind: "group".into(), id: "g1".into() })),
    );
    let words: Vec<_> = t.rows.iter().map(|r| (r.cells[1].kind, r.cells[1].value.as_str())).collect();
    assert_eq!(words, vec![
        (CellKind::Label, "admin"),
        (CellKind::Label, "member"),
        (CellKind::Label, "invited"),
        (CellKind::Label, "requested"),
    ]);
    let members: Vec<_> = t.rows.iter().map(|r| r.cells[2].value.as_str()).collect();
    assert_eq!(members, vec!["3", "1", "2", "5"]);
    assert_eq!(first.cells[3].value, "2026-03-04");

    let next = admin::groups_body(&rows, 30, Some("c".into()));
    assert_eq!(table(&next).page.next_cursor.as_deref(), Some("c"));
    assert_eq!(table(&next).page.total, None);
}

#[test]
fn purges_body_is_empty_at_zero_and_one_item_otherwise() {
    let empty = admin::purges_body(0, "");
    assert!(matches!(&empty.primitive, Primitive::AttentionItems(items) if items.is_empty()));
    let some = admin::purges_body(2, "2026-01-02T03:04:05Z");
    match &some.primitive {
        Primitive::AttentionItems(items) => {
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].title, "Groups purges pending");
            assert!(items[0].text.contains("2 players"), "{}", items[0].text);
            assert!(items[0].text.contains("2026-01-02"), "{}", items[0].text);
        }
        other => panic!("an attention list, got {other:?}"),
    }
}

/// Four rows in four states and two group sizes: every state listed with its role or
/// state word, each group's own member count (pending rows excluded), the joined day in
/// UTC, and the tab count equal to the rows. The UTC day is read again on a connection
/// whose session zone is UTC+14, where the 23:30Z row is already the next day: sqlx pins
/// `TimeZone=UTC` at connect, so the pool's own sessions cannot tell the two apart.
#[tokio::test]
async fn groups_tab_lists_every_state_with_member_counts_and_joined_day() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool, Arc::new(FakeDirectory::new())).await;
    let me = unique_uuid(&pool).await;
    let big = insert_group(&pool, "Big", JOIN_OPEN, "2026-01-01T00:00:00Z").await;
    let small = insert_group(&pool, "Small", JOIN_OPEN, "2026-01-01T00:00:00Z").await;
    let invited = insert_group(&pool, "Inv", JOIN_INVITE, "2026-01-01T00:00:00Z").await;
    let requested = insert_group(&pool, "Req", JOIN_OPEN, "2026-01-01T00:00:00Z").await;
    let ids = vec![big.clone(), small.clone(), invited.clone(), requested.clone()];
    let p = pool.clone();
    with_cleanup(&pool, ids, async move {
        // Late evening UTC: the day is the UTC day.
        insert_row(&p, &big, &me, STATE_MEMBER, ROLE_ADMIN, "2026-03-04T23:30:00Z").await;
        for _ in 0..2 {
            let other = unique_uuid(&p).await;
            insert_row(&p, &big, &other, STATE_MEMBER, ROLE_MEMBER, "2026-01-02T00:00:00Z").await;
        }
        let pending = unique_uuid(&p).await;
        insert_row(&p, &big, &pending, STATE_REQUESTED, "", "2026-01-02T00:00:00Z").await;
        insert_row(&p, &small, &me, STATE_MEMBER, ROLE_MEMBER, "2026-03-03T00:00:00Z").await;
        insert_row(&p, &invited, &me, STATE_INVITED, "", "2026-03-02T00:00:00Z").await;
        let owner = unique_uuid(&p).await;
        insert_row(&p, &invited, &owner, STATE_MEMBER, ROLE_ADMIN, "2026-01-02T00:00:00Z").await;
        insert_row(&p, &requested, &me, STATE_REQUESTED, "", "2026-03-01T00:00:00Z").await;

        let admin = admin::GroupsAdmin::new(svc.clone());
        let body = admin.panel(query(admin::GROUPS, "player", &me, None)).await.unwrap();
        assert_eq!(body.meta.as_deref(), Some("4"));
        let t = table(&body);
        assert_eq!(item_ids(t), vec![big.clone(), small.clone(), invited.clone(), requested.clone()]);
        let cells: Vec<Vec<&str>> =
            t.rows.iter().map(|r| r.cells.iter().map(|c| c.value.as_str()).collect()).collect();
        assert_eq!(cells, vec![
            vec!["Big", "admin", "3", "2026-03-04"],
            vec!["Small", "member", "1", "2026-03-03"],
            vec!["Inv", "invited", "1", "2026-03-02"],
            vec!["Req", "requested", "0", "2026-03-01"],
        ]);
        assert_eq!((t.page.total, t.page.next_cursor.as_deref()), (None, None));

        let count = admin.panel(query(admin::TAB_COUNT, "player", &me, None)).await.unwrap();
        assert!(matches!(&count.primitive, Primitive::Cell(c) if c.value == "4"));

        let opts = (*p.connect_options()).clone();
        let kiritimati = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .after_connect(|conn, _| {
                Box::pin(async move {
                    sqlx::query("SET TIME ZONE 'Pacific/Kiritimati'").execute(conn).await?;
                    Ok(())
                })
            })
            .connect_with(opts)
            .await
            .unwrap();
        let (local,): (String,) =
            sqlx::query_as("SELECT to_char('2026-03-04T23:30:00Z'::timestamptz, 'YYYY-MM-DD')")
                .fetch_one(&kiritimati)
                .await
                .unwrap();
        assert_eq!(local, "2026-03-05", "the session zone is UTC+14");
        let store = Store { pool: kiritimati.clone() };
        let rows = store.admin_page(STATE_MEMBER, &me, None, 10).await.unwrap();
        let days: Vec<&str> = rows.iter().map(|r| r.joined.as_str()).collect();
        assert_eq!(days, vec!["2026-03-04", "2026-03-03", "2026-03-02", "2026-03-01"]);
        kiritimati.close().await;
    })
    .await;
}

/// 28 rows, four to a timestamp, read as two pages: 25 then 3. The first page holds the
/// six newest minutes (24 rows) and ONE of the oldest minute's four, so the cut falls
/// inside a timestamp and only the cursor's group_id half reaches the other three.
/// Together the pages hold every row once in (created_at, group_id) DESC order, the
/// cursor only past the first page.
#[tokio::test]
async fn groups_tab_keyset_pages_every_row_once_in_order() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool, Arc::new(FakeDirectory::new())).await;
    let me = unique_uuid(&pool).await;
    let mut groups = Vec::new();
    for i in 0..28 {
        let created = format!("2026-02-01T00:{:02}:00Z", i / 4);
        let g = insert_group(&pool, &format!("G{i}"), JOIN_OPEN, "2026-01-01T00:00:00Z").await;
        groups.push((created, g));
    }
    let ids: Vec<String> = groups.iter().map(|(_, g)| g.clone()).collect();
    let p = pool.clone();
    with_cleanup(&pool, ids, async move {
        for (created, g) in &groups {
            insert_row(&p, g, &me, STATE_MEMBER, ROLE_MEMBER, created).await;
        }
        let mut want = groups.clone();
        want.sort_by(|a, b| b.cmp(a));
        let want: Vec<String> = want.into_iter().map(|(_, g)| g).collect();

        let admin = admin::GroupsAdmin::new(svc.clone());
        let first = admin.panel(query(admin::GROUPS, "player", &me, None)).await.unwrap();
        let t = table(&first);
        let page1 = item_ids(t);
        assert_eq!(page1.len(), ADMIN_PAGE_SIZE);
        assert_eq!(first.meta.as_deref(), Some("28"));
        let cursor = t.page.next_cursor.clone().expect("a full page has a cursor");

        let second = admin.panel(query(admin::GROUPS, "player", &me, Some(&cursor))).await.unwrap();
        let t = table(&second);
        let page2 = item_ids(t);
        assert_eq!(page2.len(), 3);
        assert!(t.page.next_cursor.is_none(), "the last page has no cursor");
        assert_eq!([page1, page2].concat(), want, "the pages concatenate to every row once, in order");

        let count = svc.store.admin_count(&me).await.unwrap();
        assert_eq!(count, 28);
    })
    .await;
}

/// Exactly a page of rows is one page with no cursor: the reader fetches one row past the
/// page to know whether another follows, so a full last page does not point at an empty one.
#[tokio::test]
async fn groups_tab_exactly_one_full_page_has_no_cursor() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool, Arc::new(FakeDirectory::new())).await;
    let me = unique_uuid(&pool).await;
    let mut groups = Vec::new();
    for i in 0..ADMIN_PAGE_SIZE {
        let created = format!("2026-02-01T00:{i:02}:00Z");
        let g = insert_group(&pool, &format!("G{i}"), JOIN_OPEN, "2026-01-01T00:00:00Z").await;
        groups.push((created, g));
    }
    let ids: Vec<String> = groups.iter().map(|(_, g)| g.clone()).collect();
    let p = pool.clone();
    with_cleanup(&pool, ids, async move {
        for (created, g) in &groups {
            insert_row(&p, g, &me, STATE_MEMBER, ROLE_MEMBER, created).await;
        }
        let admin = admin::GroupsAdmin::new(svc.clone());
        let body = admin.panel(query(admin::GROUPS, "player", &me, None)).await.unwrap();
        let t = table(&body);
        assert_eq!(item_ids(t).len(), ADMIN_PAGE_SIZE);
        assert_eq!(body.meta.as_deref(), Some("25"));
        assert!(t.page.next_cursor.is_none(), "a full only page has no cursor");
    })
    .await;
}

/// The header of a live group names it, its policy and day, and its live counts; a missing
/// group and an id that is not a uuid are not found.
#[tokio::test]
async fn group_header_draws_a_group_and_refuses_a_missing_or_malformed_id() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool, Arc::new(FakeDirectory::new())).await;
    let g = insert_group(&pool, "Wolves", JOIN_INVITE, "2026-05-06T07:08:09Z").await;
    let p = pool.clone();
    with_cleanup(&pool, vec![g.clone()], async move {
        let admin_id = unique_uuid(&p).await;
        insert_row(&p, &g, &admin_id, STATE_MEMBER, ROLE_ADMIN, "2026-05-06T07:08:09Z").await;
        let invitee = unique_uuid(&p).await;
        insert_row(&p, &g, &invitee, STATE_INVITED, "", "2026-05-07T00:00:00Z").await;

        let admin = admin::GroupsAdmin::new(svc.clone());
        let body = admin.panel(query(admin::GROUP_HEADER, "group", &g, None)).await.unwrap();
        let Primitive::EntityHeader(h) = &body.primitive else { panic!("a header, got {:?}", body.primitive) };
        assert_eq!(h.title, "Wolves");
        assert_eq!(h.subtitle, "invite · created 2026-05-06");
        let badges: Vec<_> = h.badges.iter().map(|c| c.value.as_str()).collect();
        assert_eq!(badges, vec!["1 member", "1 pending"]);
        assert_eq!(h.avatar_seed, g);

        let missing = unique_uuid(&p).await;
        let err = admin.panel(query(admin::GROUP_HEADER, "group", &missing, None)).await.unwrap_err();
        assert_eq!(err.status, Status::NotFound);
        let err = admin.panel(query(admin::GROUP_HEADER, "group", "not-a-uuid", None)).await.unwrap_err();
        assert_eq!(err.status, Status::NotFound);
    })
    .await;
}

/// The attention item over the live store: an unswept tombstone shows in it.
#[tokio::test]
async fn purges_item_shows_a_live_unswept_tombstone() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (_ctx, svc) = wired(&pool, Arc::new(FakeDirectory::new())).await;
    let pid = unique_uuid(&pool).await;
    sqlx::query("INSERT INTO groups.purged_players (player_id) VALUES ($1::uuid)")
        .bind(&pid)
        .execute(&pool)
        .await
        .unwrap();
    let admin = admin::GroupsAdmin::new(svc.clone());
    let body = admin.panel(query(admin::ATTENTION_PURGES, "player", &pid, None)).await;
    let _ = sqlx::query("DELETE FROM groups.purged_players WHERE player_id = $1::uuid")
        .bind(&pid)
        .execute(&pool)
        .await;
    let body = body.unwrap();
    assert!(matches!(&body.primitive, Primitive::AttentionItems(items) if items.len() == 1));
}
