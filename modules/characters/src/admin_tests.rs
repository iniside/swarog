//! The characters control-plane provider: the Characters tab's data mapping, the summary fact,
//! the tab count, the list column, the character label and the purges item (pure), the refs cap,
//! and the panels against the live database.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use charactersapi::Character;
use controlplane::{
    Align, CellKind, EntityRef, Link, PanelBody, PanelQuery, Point, Primitive, Provider as _, Severity,
    MAX_PANEL_REFS, PAGE_PARAM_SEARCH,
};
use opsapi::Status;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use testdb::test_pool;

use super::{
    characters_body, column_cells, count_cell, fact_text, fake_level, fake_region, label_cells, manifest,
    purges_body, CharactersAdmin,
};
use crate::store::Store;
use crate::tests::{cleanup, unique_player, wired};
use crate::Service;

const FIRST: &str = "00000000-0000-4000-8000-000000000001";
const SECOND: &str = "00000000-0000-4000-8000-000000000002";

fn character(id: &str, name: &str, class: &str) -> Character {
    Character {
        id: id.to_string(),
        player_id: "00000000-0000-4000-8000-0000000000ff".to_string(),
        name: name.to_string(),
        class: class.to_string(),
        created_at: "2026-11-21 10:00:00+00".to_string(),
    }
}

fn table(body: &PanelBody) -> &controlplane::DataTable {
    match &body.primitive {
        Primitive::DataTable(t) => t,
        _ => panic!("the Characters tab answers a DataTable"),
    }
}

fn query(contribution: &str, player: &str) -> PanelQuery {
    PanelQuery {
        module: "characters".to_string(),
        contribution: contribution.to_string(),
        entity: Some(EntityRef {
            kind: "player".to_string(),
            id: player.to_string(),
        }),
        search: None,
        cursor: None,
        before: None,
        queue_state: None,
        range: None,
        params: BTreeMap::new(),
        refs: Vec::new(),
    }
}

/// The value and kind of a single-cell answer (the fact, the tab count).
fn cell(body: PanelBody) -> (String, CellKind) {
    match body.primitive {
        Primitive::Cell(c) => (c.value, c.kind),
        _ => panic!("a fact or a tab count answers a Cell"),
    }
}

#[test]
fn tab_panel_maps_each_character_to_the_six_cells_in_the_given_order() {
    let body = characters_body(&[character(SECOND, "Bram", "rogue"), character(FIRST, "Aiko", "mage")]);
    assert_eq!(body.meta.as_deref(), Some("2"), "meta is the row count");
    let t = table(&body);
    let heads: Vec<(&str, CellKind)> = t.columns.iter().map(|c| (c.label.as_str(), c.kind)).collect();
    assert_eq!(
        heads,
        [
            ("Name", CellKind::Text),
            ("Class", CellKind::Text),
            ("Level", CellKind::Number),
            ("Region", CellKind::Ref),
            ("Last played", CellKind::Text),
            ("Id", CellKind::MonoMuted),
        ]
    );
    let aligns: Vec<bool> = t.columns.iter().map(|c| matches!(c.align, Align::Right)).collect();
    assert_eq!(aligns, [false, false, true, false, false, false], "only Level is right-aligned");
    assert!(t.page.next_cursor.is_none() && t.page.total.is_none(), "no pager");
    assert!(t.search_placeholder.is_none() && t.filters.is_empty() && !t.selectable, "no toolbar");
    let rows: Vec<Vec<(&str, CellKind)>> = t
        .rows
        .iter()
        .map(|r| r.cells.iter().map(|c| (c.value.as_str(), c.kind)).collect())
        .collect();
    assert_eq!(
        rows,
        [
            vec![
                ("Bram", CellKind::Text),
                ("rogue", CellKind::Text),
                ("30", CellKind::Number),
                ("eu-central", CellKind::Ref),
                ("—", CellKind::Text),
                (SECOND, CellKind::MonoMuted),
            ],
            vec![
                ("Aiko", CellKind::Text),
                ("mage", CellKind::Text),
                ("57", CellKind::Number),
                ("ap-ne", CellKind::Ref),
                ("—", CellKind::Text),
                (FIRST, CellKind::MonoMuted),
            ],
        ],
        "rows keep the reader's order; the Id is the full uuid"
    );
    for row in &t.rows {
        assert!(row.entity.is_none() && row.actions.is_empty(), "plain rows: no link, no row action");
    }
}

#[test]
fn tab_panel_with_no_characters_is_an_empty_table() {
    let body = characters_body(&[]);
    assert_eq!(body.meta.as_deref(), Some("0"));
    assert!(table(&body).rows.is_empty(), "an empty DataTable draws the portal's empty state");
}

#[test]
fn fake_level_and_region_are_pinned_pure_functions_of_the_id() {
    // Pinned: a change of salt, range or hash moves these values.
    assert_eq!((fake_level(FIRST), fake_region(FIRST)), (57, "ap-ne"));
    assert_eq!((fake_level(SECOND), fake_region(SECOND)), (30, "eu-central"));
    for n in 0..500 {
        let id = format!("00000000-0000-4000-8000-{n:012}");
        assert!((1..=60).contains(&fake_level(&id)), "level of {id} is in 1..=60");
        assert_eq!(fake_level(&id), fake_level(&id));
    }
}

#[test]
fn fact_wording_is_singular_only_at_one() {
    assert_eq!(fact_text(0), "0 characters");
    assert_eq!(fact_text(1), "1 character");
    assert_eq!(fact_text(3), "3 characters");
}

#[test]
fn tab_count_is_a_number_cell_at_zero() {
    let c = count_cell(0);
    assert_eq!((c.value.as_str(), c.kind), ("0", CellKind::Number), "0 still draws a count");
}

#[test]
fn list_column_answers_every_input_ref_with_0_for_a_ref_the_counts_lack() {
    let refs = [FIRST.to_string(), SECOND.to_string()];
    let counts = BTreeMap::from([(FIRST.to_string(), 2)]);
    let cells = match column_cells(&refs, &counts).primitive {
        Primitive::ColumnCells(cells) => cells,
        _ => panic!("a list column answers ColumnCells"),
    };
    let cells: Vec<(&str, &str, CellKind)> =
        cells.iter().map(|(r, c)| (r.as_str(), c.value.as_str(), c.kind)).collect();
    assert_eq!(cells, [(FIRST, "2", CellKind::Number), (SECOND, "0", CellKind::Number)]);
}

#[test]
fn label_answers_only_the_input_refs_the_names_know_keyed_as_given() {
    let upper = FIRST.to_uppercase();
    let refs = [upper.clone(), SECOND.to_string()];
    let names = BTreeMap::from([(upper.clone(), "Alpha".to_string())]);
    let cells = match label_cells(&refs, &names).primitive {
        Primitive::ColumnCells(cells) => cells,
        _ => panic!("a label answers ColumnCells"),
    };
    let cells: Vec<(&str, &str, CellKind)> =
        cells.iter().map(|(r, c)| (r.as_str(), c.value.as_str(), c.kind)).collect();
    assert_eq!(cells, [(upper.as_str(), "Alpha", CellKind::Text)], "an unknown ref draws no cell");
}

#[test]
fn purges_draw_nothing_at_0_pending() {
    let items = match purges_body(0, "").primitive {
        Primitive::AttentionItems(items) => items,
        _ => panic!("the purges item answers AttentionItems"),
    };
    assert!(items.is_empty(), "no backlog draws no attention item");
}

#[test]
fn purges_draw_one_warn_item_with_the_backlog_and_the_oldest_day() {
    let body = |n| match purges_body(n, "2026-11-20 08:15:42.123+00").primitive {
        Primitive::AttentionItems(items) => items,
        _ => panic!("the purges item answers AttentionItems"),
    };
    let items = body(3);
    assert_eq!(items.len(), 1, "a backlog is one item");
    let item = &items[0];
    assert_eq!(item.severity, Severity::Warn);
    assert_eq!(item.title, "Character purges pending");
    assert_eq!(item.text, "3 players awaiting sweep · oldest 2026-11-20");
    assert!(matches!(
        &item.link,
        Link::Page { module, page, params }
            if module == "core.events" && page == "events"
                && params.get(PAGE_PARAM_SEARCH).map(String::as_str) == Some("characters.purge-on-scheduler.v1")
    ));
    assert_eq!(body(1)[0].text, "1 player awaiting sweep · oldest 2026-11-20");
}

#[test]
fn manifest_declares_the_tab_fact_count_and_list_column_and_no_write() {
    let m = manifest();
    assert!(!m.writes && m.actions.is_empty(), "characters is not a writer");
    let decls: Vec<(&str, i32, &str)> =
        m.contributions.iter().map(|c| (c.id.as_str(), c.order, c.icon.as_str())).collect();
    assert_eq!(
        decls,
        [
            ("characters", 20, ""),
            ("fact.count", 30, "swords"),
            ("tab.count", 0, ""),
            ("column.count", 10, ""),
            ("label.character", 0, ""),
            ("attention.purges", 61, ""),
        ]
    );
    assert!(matches!(
        &m.contributions[0].point,
        Point::EntityTab { kind, tab, tab_label, columns: 1 }
            if kind == "player" && tab == "characters" && tab_label == "Characters"
    ));
    assert!(matches!(&m.contributions[1].point, Point::EntitySummary(kind) if kind == "player"));
    assert!(matches!(
        &m.contributions[2].point,
        Point::EntityTabCount { kind, tab } if kind == "player" && tab == "characters"
    ));
    assert!(matches!(
        &m.contributions[3].point,
        Point::EntityListColumn { kind, label, align: Align::Right }
            if kind == "player" && label == "Characters"
    ));
    assert!(matches!(&m.contributions[4].point, Point::EntityLabel { kind } if kind == "character"));
    assert!(matches!(&m.contributions[5].point, Point::DashboardAttention));
    assert_eq!(m.contributions[5].title, "Character purges pending");
    let kinds: Vec<(&str, &str, &str)> =
        m.entity_kinds.iter().map(|k| (k.kind.as_str(), k.label.as_str(), k.icon.as_str())).collect();
    assert_eq!(kinds, [("character", "Character", "swords")], "characters owns the kind it labels");
}

/// Refs over the cap are refused before any SQL: the pool points at a closed port, so a
/// read that reached it would fail as Internal, not Invalid.
#[tokio::test]
async fn counts_refuse_refs_over_the_cap_before_sql() {
    let svc = Service {
        store: Store {
            pool: PgPoolOptions::new()
                .acquire_timeout(Duration::from_millis(500))
                .connect_lazy("postgres://nobody:nothing@127.0.0.1:1/none")
                .unwrap(),
        },
        bus: Arc::new(bus::Bus::new()),
        config: OnceLock::new(),
    };
    let admin = CharactersAdmin::new(Arc::new(svc));
    let refs = |n: usize| -> Vec<String> { (0..n).map(|i| format!("00000000-0000-4000-8000-{i:012}")).collect() };
    let over = admin.counts(&refs(MAX_PANEL_REFS + 1)).await.unwrap_err();
    assert_eq!(over.status, Status::Invalid);
    let at = admin.counts(&refs(MAX_PANEL_REFS)).await.unwrap_err();
    assert_eq!(at.status, Status::Internal, "at the cap the read reaches the (closed) pool");
    let over = admin.labels(&refs(MAX_PANEL_REFS + 1)).await.unwrap_err();
    assert_eq!(over.status, Status::Invalid);
    let at = admin.labels(&refs(MAX_PANEL_REFS)).await.unwrap_err();
    assert_eq!(at.status, Status::Internal, "at the cap the label read reaches the (closed) pool");
}

async fn plant(pool: &PgPool, player: &str, name: &str, created_at: &str) -> String {
    let (id,): (String,) = sqlx::query_as(
        "INSERT INTO characters.characters (player_id, name, class, created_at) \
         VALUES ($1::uuid, $2, 'mage', $3::timestamptz) RETURNING id::text",
    )
    .bind(player)
    .bind(name)
    .bind(created_at)
    .fetch_one(pool)
    .await
    .unwrap();
    id
}

#[tokio::test]
async fn panels_answer_a_players_characters_fact_and_count() {
    let Some(pool) = test_pool().await else { return };
    let (ctx, svc) = wired(&pool).await;
    let (three, one, none) = (unique_player(&pool).await, unique_player(&pool).await, unique_player(&pool).await);
    // Planted out of name order: the tab follows created_at, not the name.
    let late = plant(&pool, &three, "Alpha", "2026-11-21 12:00:00+00").await;
    let early = plant(&pool, &three, "Charlie", "2026-11-21 10:00:00+00").await;
    let middle = plant(&pool, &three, "Bravo", "2026-11-21 11:00:00+00").await;
    plant(&pool, &one, "Solo", "2026-11-21 10:00:00+00").await;

    let admin = CharactersAdmin::new(Arc::clone(&svc));
    let tab = admin.panel(query("characters", &three)).await;
    let empty = admin.panel(query("characters", &none)).await;
    let facts = [
        admin.panel(query("fact.count", &three)).await,
        admin.panel(query("fact.count", &one)).await,
        admin.panel(query("fact.count", &none)).await,
    ];
    let counts = [
        admin.panel(query("tab.count", &three)).await,
        admin.panel(query("tab.count", &none)).await,
    ];
    let by_ref = admin.counts(&[none.clone(), three.clone(), none.clone()]).await;
    let column = admin
        .panel(PanelQuery {
            entity: None,
            refs: vec![three.clone(), none.clone(), one.clone()],
            ..query("column.count", &three)
        })
        .await;
    let malformed = admin.panel(query("fact.count", "not-a-uuid")).await;
    // An unknown and a malformed ref are left out; an uppercase spelling answers as given.
    let unknown = "00000000-0000-4000-8000-0000000000ee".to_string();
    let late_upper = late.to_uppercase();
    let label = admin
        .panel(PanelQuery {
            entity: None,
            refs: vec![early.clone(), unknown, "not-a-uuid".to_string(), late_upper.clone()],
            ..query("label.character", &three)
        })
        .await;
    let providers = ctx.contributions(controlplane::PROVIDER_SLOT);
    cleanup(&pool, &[&three, &one]).await;

    let tab = tab.unwrap();
    assert_eq!(tab.meta.as_deref(), Some("3"));
    let ids: Vec<&str> = table(&tab).rows.iter().map(|r| r.cells[5].value.as_str()).collect();
    assert_eq!(ids, [early.as_str(), middle.as_str(), late.as_str()], "rows in created_at order");
    let empty = empty.unwrap();
    assert_eq!(empty.meta.as_deref(), Some("0"));
    assert!(table(&empty).rows.is_empty());

    let facts: Vec<(String, CellKind)> = facts.into_iter().map(|f| cell(f.unwrap())).collect();
    assert_eq!(
        facts,
        [
            ("3 characters".to_string(), CellKind::Text),
            ("1 character".to_string(), CellKind::Text),
            ("0 characters".to_string(), CellKind::Text),
        ]
    );
    let counts: Vec<(String, CellKind)> = counts.into_iter().map(|c| cell(c.unwrap())).collect();
    assert_eq!(counts, [("3".to_string(), CellKind::Number), ("0".to_string(), CellKind::Number)]);
    assert_eq!(
        by_ref.unwrap(),
        BTreeMap::from([(none.clone(), 0), (three.clone(), 3)]),
        "every ref answered by its own spelling, 0 for none"
    );
    let column = match column.unwrap().primitive {
        Primitive::ColumnCells(cells) => cells,
        _ => panic!("the list column answers ColumnCells"),
    };
    let column: BTreeMap<String, (String, CellKind)> =
        column.into_iter().map(|(r, c)| (r, (c.value, c.kind))).collect();
    assert_eq!(
        column,
        BTreeMap::from([
            (three.clone(), ("3".to_string(), CellKind::Number)),
            (none.clone(), ("0".to_string(), CellKind::Number)),
            (one.clone(), ("1".to_string(), CellKind::Number)),
        ]),
        "the column answers every ref by its own spelling, 0 for none"
    );
    assert_eq!(malformed.unwrap_err().status, Status::Invalid, "a malformed player id is the caller's error");
    let label = match label.unwrap().primitive {
        Primitive::ColumnCells(cells) => cells,
        _ => panic!("the label answers ColumnCells"),
    };
    let label: BTreeMap<String, (String, CellKind)> =
        label.into_iter().map(|(r, c)| (r, (c.value, c.kind))).collect();
    assert_eq!(
        label,
        BTreeMap::from([
            (early.clone(), ("Charlie".to_string(), CellKind::Text)),
            (late_upper, ("Alpha".to_string(), CellKind::Text)),
        ]),
        "names keyed by the input ref; an unknown or malformed ref draws no cell"
    );

    let modules: Vec<String> = providers.iter().map(|p| p.manifest().module).collect();
    assert_eq!(modules, ["characters"], "init contributes the provider to PROVIDER_SLOT");
}
