//! The accounts control-plane provider against the live database.

use std::collections::BTreeMap;
use std::sync::Arc;

use accountsapi::Auth as _;
use controlplane::{
    CellKind, DataTable, EntityRef, Link, PanelBody, PanelQuery, Primitive, Provider as _, SearchQuery,
};
use opsapi::Status;
use sqlx::PgPool;
use testdb::test_pool;

use super::{fake_client, fake_country, fake_ip, mask, thousands, AccountsAdmin, MAX_NEEDLE_BYTES, PAGE_SIZE};
use crate::tests::{cleanup_player, suffix, wired};

/// A canonical uuid no registration mints: registrations are v4, this is v0.
const NO_PLAYER: &str = "00000000-0000-0000-0000-000000000000";

fn query(contribution: &str) -> PanelQuery {
    PanelQuery {
        module: "accounts".to_string(),
        contribution: contribution.to_string(),
        entity: None,
        search: None,
        cursor: None,
        before: None,
        queue_state: None,
        range: None,
        params: BTreeMap::new(),
        refs: Vec::new(),
    }
}

fn label_query(refs: Vec<String>) -> PanelQuery {
    PanelQuery { refs, ..query("label") }
}

fn player_query(contribution: &str, id: &str) -> PanelQuery {
    PanelQuery {
        entity: Some(EntityRef { kind: "player".to_string(), id: id.to_string() }),
        ..query(contribution)
    }
}

fn players_query(search: Option<&str>, provider: Option<&str>, cursor: Option<String>) -> PanelQuery {
    let mut params = BTreeMap::new();
    if let Some(p) = provider {
        params.insert("provider".to_string(), p.to_string());
    }
    PanelQuery { search: search.map(str::to_string), cursor, params, ..query("players") }
}

fn table(body: PanelBody) -> DataTable {
    let Primitive::DataTable(table) = body.primitive else {
        panic!("a players or detail panel answers a DataTable");
    };
    table
}

fn row_ids(table: &DataTable) -> Vec<String> {
    let id = |r: &controlplane::Row| r.entity.as_ref().expect("a players row names its player").id.clone();
    table.rows.iter().map(id).collect()
}

/// The refresh family a refresh token belongs to.
async fn family_of(pool: &PgPool, token: &str) -> String {
    sqlx::query_scalar("SELECT family_id::text FROM accounts.refresh_tokens WHERE token = $1")
        .bind(token)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn player_label_answers_each_handle_and_leaves_out_a_missing_player() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let mut ids = Vec::new();
    for name in ["LabelA", "LabelB"] {
        let email = format!("{name}-{}@test.local", suffix());
        ids.push(svc.register(email, "pw".into(), name.into()).await.unwrap().player_id);
    }
    let refs = vec![ids[0].clone(), NO_PLAYER.to_string(), "not-a-uuid".to_string(), ids[1].clone()];
    let answer = AccountsAdmin::new(Arc::clone(&svc)).panel(label_query(refs)).await;
    for id in &ids {
        cleanup_player(&pool, id).await;
    }

    let Primitive::ColumnCells(cells) = answer.unwrap().primitive else {
        panic!("a player label answers ColumnCells");
    };
    let drawn: BTreeMap<&str, (&str, CellKind)> =
        cells.iter().map(|(id, c)| (id.as_str(), (c.value.as_str(), c.kind))).collect();
    let keys: Vec<&str> = drawn.keys().copied().collect();
    let mut known: Vec<&str> = ids.iter().map(String::as_str).collect();
    known.sort();
    assert_eq!(keys, known, "one label per known player; a missing or malformed id draws nothing");
    for (id, name) in ids.iter().zip(["LabelA", "LabelB"]) {
        let (value, kind) = drawn[id.as_str()];
        assert_eq!(kind, CellKind::Text, "a label is a text cell");
        let digits = value
            .strip_prefix(name)
            .and_then(|rest| rest.strip_prefix('#'))
            .unwrap_or_else(|| {
                panic!("label {value:?} for {id} is not {name}#...: keyed to the wrong player")
            });
        assert!(
            digits.len() == 4 && digits.bytes().all(|b| b.is_ascii_digit()),
            "label {value:?} is the registered name plus '#' and exactly 4 digits",
        );
    }
}

#[tokio::test]
async fn player_search_hit_links_to_the_player_entity() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("Seeker-{}@test.local", suffix());
    let id = svc.register(email, "pw".into(), "Seeker".into()).await.unwrap().player_id;
    let query = SearchQuery { module: "accounts".to_string(), kind: "player".to_string(), q: id.clone(), limit: 10 };
    let hits = AccountsAdmin::new(Arc::clone(&svc)).search(query).await;
    cleanup_player(&pool, &id).await;

    let hits = hits.unwrap();
    let hit = hits
        .iter()
        .find(|h| h.subtitle.ends_with(&id))
        .unwrap_or_else(|| panic!("searching by id finds player {id}: {hits:?}"));
    assert_eq!(
        hit.link,
        Link::Entity(EntityRef { kind: "player".to_string(), id: id.clone() }),
        "a player hit opens that player's entity page",
    );
}

#[tokio::test]
async fn players_table_pages_by_created_then_id_and_resumes_after_the_cursor() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    // One shared created_at, so only the id orders the rows and the cursor's id half decides
    // where page two starts. Each row commits with its handle claim, as migrate requires.
    let name = format!("Pg{}", suffix());
    let mut ids: Vec<String> = sqlx::query_scalar(
        "WITH p AS (INSERT INTO accounts.players (display_name, discriminator, created_at) \
                    SELECT $1, lpad(g::text, 4, '0'), '2001-01-01T00:00:00Z' \
                      FROM generate_series(1, $2) g \
                    RETURNING id, display_name, discriminator), \
              h AS (INSERT INTO accounts.handles (display_name_lower, discriminator, player_id) \
                    SELECT lower(display_name), discriminator, id FROM p) \
         SELECT id::text FROM p",
    )
    .bind(&name)
    .bind(PAGE_SIZE as i32 + 2)
    .fetch_all(&pool)
    .await
    .unwrap();
    let admin = AccountsAdmin::new(Arc::clone(&svc));
    let first = admin.panel(players_query(Some(&name), None, None)).await;
    let cursor = first.as_ref().ok().and_then(|b| table(b.clone()).page.next_cursor);
    let second = admin.panel(players_query(Some(&name), None, cursor.clone())).await;
    for id in &ids {
        cleanup_player(&pool, id).await;
    }

    let (first, second) = (table(first.unwrap()), table(second.unwrap()));
    assert_eq!(first.rows.len(), PAGE_SIZE, "a full first page");
    assert!(cursor.is_some(), "a page with more rows behind it hands out a cursor");
    assert_eq!(second.page.next_cursor, None, "the last page hands out no cursor");
    assert_eq!(first.page.total, None, "a searched table has no total");
    let mut shown = row_ids(&first);
    shown.extend(row_ids(&second));
    ids.sort();
    ids.reverse();
    assert_eq!(shown, ids, "two pages show every player once, id descending within one created_at");
}

#[tokio::test]
async fn players_table_finds_by_id_handle_email_and_substring_and_filters_by_provider() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let name = format!("Find{}", suffix());
    let email = format!("{}@test.local", name.to_lowercase());
    let id = svc.register(email.clone(), "pw".into(), name.clone()).await.unwrap().player_id;
    let admin = AccountsAdmin::new(Arc::clone(&svc));
    let row = admin.svc.store.player_row(&id).await.unwrap().unwrap();
    let handle = format!("{}#{}", name.to_lowercase(), row.discriminator);
    let mut found = Vec::new();
    for needle in [id.as_str(), handle.as_str(), email.as_str(), &name[2..]] {
        found.push((needle.to_string(), admin.panel(players_query(Some(needle), None, None)).await));
    }
    let dev = admin.panel(players_query(Some(&id), Some("dev"), None)).await;
    let guest = admin.panel(players_query(Some(&id), Some("guest"), None)).await;
    let unfiltered = admin.panel(players_query(None, None, None)).await;
    cleanup_player(&pool, &id).await;

    for (needle, body) in found {
        let t = table(body.unwrap());
        assert_eq!(row_ids(&t), vec![id.clone()], "needle {needle:?} finds exactly the player");
        assert_eq!(t.page.total, None, "a searched table has no total");
    }
    let dev = table(dev.unwrap());
    assert_eq!(row_ids(&dev), vec![id.clone()], "the dev filter keeps a player with a dev identity");
    assert_eq!(dev.page.total, None, "a filtered table has no total");
    let cells = &dev.rows[0].cells;
    assert_eq!(cells[0].value, format!("{name}#{}", row.discriminator), "the User cell is the handle");
    let entity = EntityRef { kind: "player".to_string(), id: id.clone() };
    assert_eq!(cells[0].link, Some(Link::Entity(entity)), "the User cell opens the player");
    assert_eq!((cells[3].kind, cells[3].value.as_str()), (CellKind::Ref, "dev"), "Identities tags dev");
    assert!(row_ids(&table(guest.unwrap())).is_empty(), "the guest filter drops a dev-only player");
    assert!(
        table(unfiltered.unwrap()).page.total.is_some_and(|n| n >= 1),
        "an unsearched, unfiltered table carries the exact count",
    );
}

#[tokio::test]
async fn players_table_refuses_an_unknown_provider_an_overlong_search_and_a_bad_cursor() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let admin = AccountsAdmin::new(svc);
    let long = "x".repeat(MAX_NEEDLE_BYTES + 1);
    let past_exact = format!("{}.{NO_PLAYER}", (1_i64 << 53) + 1);
    let cases = [
        ("unknown provider", players_query(None, Some("steam"), None)),
        ("overlong search", players_query(Some(&long), None, None)),
        ("cursor without a dot", players_query(None, None, Some("123".to_string()))),
        ("cursor with a bad id", players_query(None, None, Some("123.nope".to_string()))),
        ("cursor with signed micros", players_query(None, None, Some(format!("-1.{NO_PLAYER}")))),
        ("cursor past exact float8", players_query(None, None, Some(past_exact))),
    ];
    for (what, q) in cases {
        let e = admin.panel(q).await.unwrap_err();
        assert_eq!(e.status, Status::Invalid, "{what} is Invalid, never a database error");
    }
}

#[tokio::test]
async fn nav_counter_draws_the_player_count_with_separators() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("count-{}@test.local", suffix());
    let id = svc.register(email, "pw".into(), "Counted".into()).await.unwrap().player_id;
    let body = AccountsAdmin::new(Arc::clone(&svc)).panel(query("nav.players")).await;
    cleanup_player(&pool, &id).await;

    let Primitive::Cell(cell) = body.unwrap().primitive else {
        panic!("with a player registered the nav counter is a cell");
    };
    assert_eq!(cell.kind, CellKind::Badge);
    let n: i64 = cell.value.replace(',', "").parse().expect("the counter is a grouped number");
    assert!(n >= 1, "the counter counts the registered player");
    assert_eq!(cell.value, thousands(n), "the counter groups its digits");
}

#[tokio::test]
async fn sessions_panel_lists_live_families_newest_first_and_counts_them() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("sess-{}@test.local", suffix());
    let registered = svc.register(email.clone(), "pw".into(), "Sess".into()).await.unwrap();
    let id = registered.player_id.clone();
    // A rotated family stays one live family.
    let rotated = svc.refresh(registered.refresh_token.clone()).await.unwrap();
    let second = svc.login(email.clone(), "pw".into()).await.unwrap();
    let expired = svc.login(email, "pw".into()).await.unwrap();
    let (a, b, c) = (
        family_of(&pool, &rotated.refresh_token).await,
        family_of(&pool, &second.refresh_token).await,
        family_of(&pool, &expired.refresh_token).await,
    );
    sqlx::query(
        "UPDATE accounts.refresh_tokens SET expires_at = now() - interval '1 second' \
          WHERE family_id = $1::uuid",
    )
        .bind(&c)
        .execute(&pool)
        .await
        .unwrap();
    let body = AccountsAdmin::new(Arc::clone(&svc)).panel(player_query("sessions", &id)).await;
    cleanup_player(&pool, &id).await;

    let body = body.unwrap();
    assert_eq!(body.meta.as_deref(), Some("2"), "the meta counts live families, not tokens");
    let t = table(body);
    let items: Vec<&str> = t.rows.iter().map(|r| r.item.as_deref().unwrap()).collect();
    assert_eq!(items, vec![b.as_str(), a.as_str()], "live families newest first; the expired one is gone");
    for (row, family) in t.rows.iter().zip([&b, &a]) {
        assert_eq!(row.cells[0].value, fake_client(family), "Client hashes the family id");
        assert_eq!(row.cells[1].value, fake_ip(family), "IP hashes the family id");
        assert!(row.cells[3].value.starts_with("in "), "Expires is relative: {:?}", row.cells[3].value);
        assert_eq!(row.actions, vec!["revoke-session".to_string()]);
    }
}

#[tokio::test]
async fn identities_panel_masks_the_subject_and_counts_the_links() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("ident-{}@test.local", suffix().to_lowercase());
    let id = svc.register(email.clone(), "pw".into(), "Ident".into()).await.unwrap().player_id;
    let body = AccountsAdmin::new(Arc::clone(&svc)).panel(player_query("identities", &id)).await;
    cleanup_player(&pool, &id).await;

    let body = body.unwrap();
    assert_eq!(body.meta.as_deref(), Some("1"));
    let t = table(body);
    let row = &t.rows[0];
    assert_eq!(row.cells[0].value, "dev");
    assert_eq!(row.cells[1].value, mask(&email), "the email subject is masked");
    assert!(!row.cells[1].value.contains(&email[4..email.len() - 4]), "the masked middle never leaks");
    assert_eq!(row.item.as_deref(), Some("dev"), "unlink acts on the provider");
}

#[tokio::test]
async fn player_panels_draw_the_handle_and_refuse_a_missing_or_absent_player() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("Head-{}@test.local", suffix());
    let id = svc.register(email, "pw".into(), "Head".into()).await.unwrap().player_id;
    let admin = AccountsAdmin::new(Arc::clone(&svc));
    let header = admin.panel(player_query("header", &id)).await;
    let fact = admin.panel(player_query("fact.id", &id)).await;
    let missing = admin.panel(player_query("profile", NO_PLAYER)).await;
    let absent = admin.panel(query("profile")).await;
    cleanup_player(&pool, &id).await;

    let Primitive::EntityHeader(h) = header.unwrap().primitive else {
        panic!("the header panel is an EntityHeader");
    };
    assert_eq!(h.title, "Head");
    let handle = h.crumb.clone().expect("the crumb shows the handle");
    assert!(handle.starts_with("Head#"), "the crumb is the handle, not the uuid: {handle}");
    assert_eq!(h.subtitle, format!("@{handle}"));
    let Primitive::Cell(cell) = fact.unwrap().primitive else { panic!("fact.id is a cell") };
    assert_eq!((cell.kind, cell.value), (CellKind::Mono, id.clone()));
    assert_eq!(missing.unwrap_err().status, Status::NotFound, "a missing player is NotFound");
    assert_eq!(absent.unwrap_err().status, Status::Invalid, "a player panel needs an entity");
}

#[tokio::test]
async fn player_search_answers_nothing_for_another_kind_or_a_blank_query() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let name = format!("Srch{}", suffix().replace(['-', '_'], "x"));
    let email = format!("{name}@test.local");
    let id = svc.register(email, "pw".into(), name.clone()).await.unwrap().player_id;
    let admin = AccountsAdmin::new(Arc::clone(&svc));
    let search = |kind: &str, q: String| SearchQuery {
        module: "accounts".to_string(),
        kind: kind.to_string(),
        q,
        limit: 10,
    };
    // Both queries would list the player just registered (newest first, and an
    // empty needle matches every name) if search did not answer them early.
    let other_kind = admin.search(search("character", name.clone())).await;
    let blank = admin.search(search("player", "   ".to_string())).await;
    cleanup_player(&pool, &id).await;

    assert_eq!(other_kind.unwrap(), Vec::new(), "another kind finds nothing, not the named player");
    assert_eq!(blank.unwrap(), Vec::new(), "a blank query finds nothing, not every player");
}

#[test]
fn fakes_draw_pinned_values_from_the_shared_helper() {
    assert_eq!(fake_country("p-1"), "ES");
    assert_eq!(fake_ip("fam-1"), "198.51.100.64");
}

#[test]
fn thousands_groups_digits_and_mask_hides_a_short_subject() {
    assert_eq!(thousands(1_234_567), "1,234,567");
    assert_eq!(thousands(999), "999");
    assert_eq!(thousands(-3), "0");
    assert_eq!(mask("short"), "…", "a subject of eight characters or fewer shows nothing");
}
