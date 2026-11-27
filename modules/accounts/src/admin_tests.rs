//! The accounts control-plane provider against the live database.

use std::collections::BTreeMap;
use std::sync::Arc;

use accountsapi::Auth as _;
use controlplane::{
    CellKind, DataTable, EntityRef, InvokeCall, InvokeOutcome, Link, PanelBody, PanelQuery,
    PrepareCall, Primitive, Provider as _, SearchQuery, Then,
};
use opsapi::Status;
use sqlx::PgPool;
use testdb::test_pool;

use super::{
    fake_ip, mask, session_rows, thousands, AccountsAdmin, CREATE_ACCOUNT,
    CREATE_CLIENT, DELETE_CLIENT, FIELD_AUDIENCE, FIELD_CONFIDENTIAL, FIELD_DEVICE_CODE_TTL, FIELD_DISPLAY_NAME,
    FIELD_EMAIL, FIELD_GRANTS, FIELD_REDIRECT_URIS, FIELD_ROOT_DOMAINS, MAX_NEEDLE_BYTES,
    PAGE_SIZE, RESET_PASSWORD, REVOKE_ALL, REVOKE_SESSION, SESSIONS_SHOWN, SET_DEVICE_CODE_TTL,
};
use crate::store::FamilyRow;
use crate::web::session::BrowserSessionRow;
use crate::tests::{
    cleanup_player, delete_client, insert_client, suffix, test_client, wired, TEST_CLIENT_ID,
};

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
        ids.push(svc.register(email, "pw".into(), name.into(), test_client()).await.unwrap().player_id);
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
    let id = svc.register(email, "pw".into(), "Seeker".into(), test_client()).await.unwrap().player_id;
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
    let id = svc.register(email.clone(), "pw".into(), name.clone(), test_client()).await.unwrap().player_id;
    let admin = AccountsAdmin::new(Arc::clone(&svc));
    let row = admin.svc.store.player_row(&id).await.unwrap().unwrap();
    let handle = format!("{}#{}", name.to_lowercase(), row.discriminator);
    let mut found = Vec::new();
    let upper_email = email.to_ascii_uppercase();
    for needle in [id.as_str(), handle.as_str(), email.as_str(), upper_email.as_str(), &name[2..]] {
        found.push((needle.to_string(), admin.panel(players_query(Some(needle), None, None)).await));
    }
    let password = admin.panel(players_query(Some(&id), Some("password"), None)).await;
    let guest = admin.panel(players_query(Some(&id), Some("guest"), None)).await;
    let unfiltered = admin.panel(players_query(None, None, None)).await;
    cleanup_player(&pool, &id).await;

    for (needle, body) in found {
        let t = table(body.unwrap());
        assert_eq!(row_ids(&t), vec![id.clone()], "needle {needle:?} finds exactly the player");
        assert_eq!(t.page.total, None, "a searched table has no total");
    }
    let password = table(password.unwrap());
    assert_eq!(row_ids(&password), vec![id.clone()], "the password filter keeps a password player");
    assert_eq!(password.page.total, None, "a filtered table has no total");
    let cells = &password.rows[0].cells;
    assert_eq!(cells[0].value, format!("{name}#{}", row.discriminator), "the User cell is the handle");
    let entity = EntityRef { kind: "player".to_string(), id: id.clone() };
    assert_eq!(cells[0].link, Some(Link::Entity(entity)), "the User cell opens the player");
    assert_eq!((cells[3].kind, cells[3].value.as_str()), (CellKind::Ref, "password"), "Identities tags password");
    assert!(row_ids(&table(guest.unwrap())).is_empty(), "the guest filter drops a password-only player");
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
    let id = svc.register(email, "pw".into(), "Counted".into(), test_client()).await.unwrap().player_id;
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

const EXPIRE_REFRESH: &str = "UPDATE accounts.refresh_tokens SET expires_at = now() - interval '1 second' \
                               WHERE family_id = $1::uuid";
/// Past `exp` plus `ACCESS_EXP_LEEWAY`, so the family's access token no longer verifies.
const EXPIRE_ACCESS: &str = "UPDATE accounts.token_families \
                                SET access_expires_at = now() - interval '1 minute' WHERE id = $1::uuid";
const EXPIRE_PLAYER_REFRESH: &str = "UPDATE accounts.refresh_tokens \
                                        SET expires_at = now() - interval '1 second' WHERE player_id = $1::uuid";
const EXPIRE_PLAYER_ACCESS: &str = "UPDATE accounts.token_families \
                                       SET access_expires_at = now() - interval '1 minute' \
                                     WHERE player_id = $1::uuid";

#[tokio::test]
async fn sessions_panel_lists_the_families_revoke_all_counts_newest_first() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("sess-{}@test.local", suffix());
    let registered = svc.register(email.clone(), "pw".into(), "Sess".into(), test_client()).await.unwrap();
    let id = registered.player_id.clone();
    // A rotated family stays one live family.
    let rotated = svc.refresh(registered.refresh_token.clone()).await.unwrap();
    let second = svc.login(email.clone(), "pw".into(), test_client()).await.unwrap();
    let expired = svc.login(email.clone(), "pw".into(), test_client()).await.unwrap();
    let access_only = svc.login(email, "pw".into(), test_client()).await.unwrap();
    let (a, b, c, d) = (
        family_of(&pool, &rotated.refresh_token).await,
        family_of(&pool, &second.refresh_token).await,
        family_of(&pool, &expired.refresh_token).await,
        family_of(&pool, &access_only.refresh_token).await,
    );
    // c is past both expiries; d's refresh token expired but its access lives on.
    for (sql, family) in [(EXPIRE_REFRESH, &c), (EXPIRE_ACCESS, &c), (EXPIRE_REFRESH, &d)] {
        sqlx::query(sql)
            .bind(family)
            .execute(&pool)
            .await
            .unwrap();
    }
    let admin = AccountsAdmin::new(Arc::clone(&svc));
    let body = admin.panel(player_query("sessions", &id)).await;
    let modal = admin
        .prepare(PrepareCall {
            module: "accounts".to_string(),
            action: REVOKE_ALL.to_string(),
            entity: Some(EntityRef { kind: "player".to_string(), id: id.clone() }),
            item: None,
            operator: "op".to_string(),
        })
        .await;
    let client_name: String =
        sqlx::query_scalar("SELECT display_name FROM accounts.oidc_clients WHERE client_id = $1")
            .bind(test_client())
            .fetch_one(&pool)
            .await
            .unwrap();
    cleanup_player(&pool, &id).await;

    let body = body.unwrap();
    assert_eq!(body.meta.as_deref(), Some("3"), "the meta counts live families, not tokens");
    let Then::Confirm(modal) = modal.unwrap() else { panic!("revoke-all prepares a Confirm") };
    assert_eq!(modal.impact[0].1.value, "3", "the Revoke all modal counts the families the panel lists");
    let t = table(body);
    let items: Vec<&str> = t.rows.iter().map(|r| r.item.as_deref().unwrap()).collect();
    assert_eq!(
        items,
        vec![d.as_str(), b.as_str(), a.as_str()],
        "families still granting access newest first, the access-only one included; the expired one is gone",
    );
    for (row, family) in t.rows.iter().zip([&d, &b, &a]) {
        assert_eq!(row.cells[0].value, client_name, "Client is the family's client's display name");
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
    let id = svc.register(email.clone(), "pw".into(), "Ident".into(), test_client()).await.unwrap().player_id;
    let linked = svc.link_identity(&id, "epic", &format!("ident-{}", suffix())).await.map(|_| ());
    let body = AccountsAdmin::new(Arc::clone(&svc)).panel(player_query("identities", &id)).await;
    cleanup_player(&pool, &id).await;

    linked.unwrap();
    let body = body.unwrap();
    assert_eq!(body.meta.as_deref(), Some("2"));
    let t = table(body);
    let row = &t.rows[0];
    assert_eq!(row.cells[0].value, "password");
    assert_eq!(row.cells[1].value, mask(&email), "the email subject is masked");
    assert!(!row.cells[1].value.contains(&email[4..email.len() - 4]), "the masked middle never leaks");
    assert_eq!(row.cells[3].value, "now", "the registration session used password just now");
    assert_eq!(t.rows[1].cells[0].value, "epic");
    assert_eq!(t.rows[1].cells[3].value, "—", "no session went through the linked identity");
    assert!(row.actions.is_empty(), "no operator action exists on a linked identity");
    assert!(row.item.is_none(), "nothing acts on an identity row");
}

#[test]
fn manifest_declares_only_the_actions_the_portal_can_refuse() {
    let m = super::manifest();
    let mut ids: Vec<&str> = m.actions.iter().map(|a| a.id.as_str()).collect();
    ids.sort_unstable();
    assert_eq!(
        ids,
        [
            "create-account",
            "create-client",
            "delete-account",
            "delete-client",
            "device-code-ttl",
            "reset-password",
            "revoke-all",
            "revoke-session",
            "suspend",
            "suspend-header"
        ]
    );
    assert_eq!(m.pages[0].head_actions, [CREATE_ACCOUNT], "Create account is the page's only head action");
    assert!(m.writes, "the session revocations write");
    let action = |id: &str| m.actions.iter().find(|a| a.id == id).unwrap();
    let create = action(CREATE_ACCOUNT);
    assert!(create.implemented && create.prepare && matches!(create.then, Then::Form(_)));
    assert!(matches!(create.point, controlplane::ActionPoint::PageHead(ref p) if p == "players"));
    let reset = action(RESET_PASSWORD);
    assert!(reset.implemented && reset.prepare && reset.danger && matches!(reset.then, Then::Confirm(_)));
    assert!(matches!(reset.point, controlplane::ActionPoint::RowAction(ref k) if k == "player"));
    let all = action(REVOKE_ALL);
    assert!(all.implemented && all.danger && all.prepare, "Revoke all is a prepared danger Confirm");
    assert!(matches!(all.then, Then::Confirm(_)));
    let one = action(REVOKE_SESSION);
    assert!(one.implemented && !one.prepare && matches!(one.then, Then::Invoke));
    let ttl = action(SET_DEVICE_CODE_TTL);
    assert!(ttl.implemented && ttl.prepare && !ttl.danger && matches!(ttl.then, Then::Form(_)));
    assert!(matches!(ttl.point, controlplane::ActionPoint::RowAction(ref k) if k == "oidc_client"));
}

#[tokio::test]
async fn a_removed_action_is_not_found_and_a_declared_one_is_forbidden() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let admin = AccountsAdmin::new(svc);
    let call = |action: &str| controlplane::InvokeCall {
        module: "accounts".to_string(),
        action: action.to_string(),
        entity: None,
        input: BTreeMap::new(),
        expected_revision: None,
        reason: None,
        operator: "op".to_string(),
        item: None,
        request_id: None,
    };
    for gone in ["export-csv", "edit-profile", "link-identity", "unlink-identity"] {
        assert_eq!(admin.invoke(call(gone)).await.unwrap_err().status, Status::NotFound, "{gone}");
    }
    assert_eq!(admin.invoke(call("suspend")).await.unwrap_err().status, Status::Forbidden);
}

#[tokio::test]
async fn player_panels_draw_the_handle_and_refuse_a_missing_or_absent_player() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("Head-{}@test.local", suffix());
    let id = svc.register(email, "pw".into(), "Head".into(), test_client()).await.unwrap().player_id;
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
    let id = svc.register(email, "pw".into(), name.clone(), test_client()).await.unwrap().player_id;
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

#[tokio::test]
async fn active_tile_and_profile_draw_nothing_the_store_cannot_back() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("prof-{}@test.local", suffix());
    let id = svc.register(email, "pw".into(), "Prof".into(), test_client()).await.unwrap().player_id;
    let admin = AccountsAdmin::new(Arc::clone(&svc));
    let tile = admin.panel(query("tile.active")).await;
    let profile = admin.panel(player_query("profile", &id)).await;
    cleanup_player(&pool, &id).await;

    let Primitive::KpiTile(tile) = tile.unwrap().primitive else {
        panic!("the active tile is a KpiTile");
    };
    assert!(tile.delta.is_none(), "no daily history exists to compare against");
    assert!(tile.series.is_empty(), "no daily history exists to draw");
    let Primitive::PropertyList(list) = profile.unwrap().primitive else {
        panic!("the profile panel is a PropertyList");
    };
    let labels: Vec<&str> = list.items.iter().map(|(label, _)| label.as_str()).collect();
    assert!(!labels.contains(&"Country"), "no country is stored for an account: {labels:?}");
}

fn revoke(action: &str, player: Option<&str>, item: Option<&str>) -> InvokeCall {
    InvokeCall {
        module: "accounts".to_string(),
        action: action.to_string(),
        entity: player.map(|id| EntityRef { kind: "player".to_string(), id: id.to_string() }),
        input: BTreeMap::new(),
        expected_revision: None,
        reason: None,
        operator: "op".to_string(),
        item: item.map(str::to_string),
        request_id: None,
    }
}

/// Refresh tokens and the family row left of one family.
async fn family_rows(pool: &PgPool, family: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM accounts.refresh_tokens WHERE family_id = $1::uuid) \
              + (SELECT count(*) FROM accounts.token_families WHERE id = $1::uuid)",
    )
    .bind(family)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Refresh tokens and family rows left to one player.
async fn player_rows(pool: &PgPool, player: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM accounts.refresh_tokens WHERE player_id = $1::uuid) \
              + (SELECT count(*) FROM accounts.token_families WHERE player_id = $1::uuid)",
    )
    .bind(player)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Polls until a backend waits on a lock held by `holder`; false when none does within
/// about twenty seconds, which only happens when nothing contends for the held row.
async fn waits_on(pool: &PgPool, holder: i32) -> bool {
    for _ in 0..4000 {
        let waiting: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_stat_activity \
              WHERE datname = current_database() AND wait_event_type = 'Lock' \
                AND $1::int = ANY(pg_blocking_pids(pid))",
        )
        .bind(holder)
        .fetch_one(pool)
        .await
        .unwrap();
        if waiting > 0 {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    false
}

async fn backend_pid(conn: &mut sqlx::PgConnection) -> i32 {
    sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(conn).await.unwrap()
}

/// Polls until some backend waits on a lock held by `holder` and answers its pid; `None`
/// when none does within about twenty seconds.
async fn waiter_of(pool: &PgPool, holder: i32) -> Option<i32> {
    for _ in 0..4000 {
        let waiter: Option<i32> = sqlx::query_scalar(
            "SELECT pid FROM pg_stat_activity \
              WHERE datname = current_database() AND wait_event_type = 'Lock' \
                AND $1::int = ANY(pg_blocking_pids(pid)) \
              LIMIT 1",
        )
        .bind(holder)
        .fetch_optional(pool)
        .await
        .unwrap();
        if waiter.is_some() {
            return waiter;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    None
}

/// Polls until a backend other than the `holders` waits on a lock held by any of them;
/// false when none does within about twenty seconds.
async fn waits_on_any(pool: &PgPool, holders: &[i32]) -> bool {
    for _ in 0..4000 {
        let waiting: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_stat_activity \
              WHERE datname = current_database() AND wait_event_type = 'Lock' \
                AND pid <> ALL($1::int[]) AND pg_blocking_pids(pid) && $1::int[]",
        )
        .bind(holders)
        .fetch_one(pool)
        .await
        .unwrap();
        if waiting > 0 {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    false
}

#[tokio::test]
async fn revoke_session_ends_one_family_and_a_repeat_or_a_foreign_family_is_noop() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("rev1-{}@test.local", suffix());
    let first = svc.register(email.clone(), "pw".into(), "RevOne".into(), test_client()).await.unwrap();
    let second = svc.login(email, "pw".into(), test_client()).await.unwrap();
    let id = first.player_id.clone();
    let other_email = format!("rev1o-{}@test.local", suffix());
    let other = svc.register(other_email, "pw".into(), "RevOther".into(), test_client()).await.unwrap();
    let (a, b, foreign) = (
        family_of(&pool, &first.refresh_token).await,
        family_of(&pool, &second.refresh_token).await,
        family_of(&pool, &other.refresh_token).await,
    );
    let admin = AccountsAdmin::new(Arc::clone(&svc));
    let revoked = admin.invoke(revoke(REVOKE_SESSION, Some(&id), Some(&a))).await;
    let rows = (family_rows(&pool, &a).await, family_rows(&pool, &b).await);
    let refresh = svc.refresh(first.refresh_token.clone()).await.map(|_| ());
    let verified = svc.tokens.verify_access(&first.token).await;
    let repeat = admin.invoke(revoke(REVOKE_SESSION, Some(&id), Some(&a))).await;
    let cross = admin.invoke(revoke(REVOKE_SESSION, Some(&id), Some(&foreign))).await;
    let foreign_rows = family_rows(&pool, &foreign).await;
    cleanup_player(&pool, &id).await;
    cleanup_player(&pool, &other.player_id).await;

    assert!(matches!(revoked.unwrap(), InvokeOutcome::Ok { ref message, .. } if message == "Session revoked"));
    assert_eq!(rows.0, 0, "the revoked family keeps no token and no family row");
    assert_eq!(rows.1, 2, "the player's other login keeps its token and family");
    assert_eq!(refresh.unwrap_err().status, Status::Unauthorized, "the revoked refresh token is dead");
    assert_eq!(verified.unwrap(), None, "the revoked access token is dead");
    assert!(matches!(repeat.unwrap(), InvokeOutcome::NoOp { .. }), "a repeat finds nothing to revoke");
    assert!(matches!(cross.unwrap(), InvokeOutcome::NoOp { .. }), "another player's family is not this player's");
    assert_eq!(foreign_rows, 2, "another player's family survives a revoke bound to this player");
}

#[tokio::test]
async fn revoke_all_ends_every_family_of_the_player_only_and_a_repeat_is_noop() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("rev2-{}@test.local", suffix());
    let first = svc.register(email.clone(), "pw".into(), "RevAll".into(), test_client()).await.unwrap();
    svc.login(email, "pw".into(), test_client()).await.unwrap();
    let id = first.player_id.clone();
    let other_email = format!("rev2o-{}@test.local", suffix());
    let other = svc.register(other_email, "pw".into(), "RevAllOther".into(), test_client()).await.unwrap();
    let admin = AccountsAdmin::new(Arc::clone(&svc));
    let before = player_rows(&pool, &id).await;
    let revoked = admin.invoke(revoke(REVOKE_ALL, Some(&id), None)).await;
    let after = player_rows(&pool, &id).await;
    let others = player_rows(&pool, &other.player_id).await;
    let repeat = admin.invoke(revoke(REVOKE_ALL, Some(&id), None)).await;
    let player_kept = svc.store.player_row(&id).await.unwrap().is_some();
    cleanup_player(&pool, &id).await;
    cleanup_player(&pool, &other.player_id).await;

    assert_eq!(before, 4, "two logins: two tokens and two families");
    assert!(matches!(revoked.unwrap(), InvokeOutcome::Ok { ref message, .. } if message == "All sessions revoked"));
    assert_eq!(after, 0, "every token and family of the player is gone");
    assert_eq!(others, 2, "another player's login is untouched");
    assert!(matches!(repeat.unwrap(), InvokeOutcome::NoOp { .. }), "a repeat finds nothing to revoke");
    assert!(player_kept, "revoking sessions keeps the account");
}

#[tokio::test]
async fn revoke_actions_refuse_a_missing_or_malformed_binding_and_an_overlong_input() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let admin = AccountsAdmin::new(svc);
    let status = |r: Result<InvokeOutcome, opsapi::Error>| r.unwrap_err().status;
    for action in [REVOKE_ALL, REVOKE_SESSION] {
        let item = Some(NO_PLAYER);
        assert_eq!(status(admin.invoke(revoke(action, None, item)).await), Status::Invalid, "{action}");
        assert_eq!(status(admin.invoke(revoke(action, Some("not-a-uuid"), item)).await), Status::Invalid);
        assert_eq!(status(admin.invoke(revoke(action, Some(NO_PLAYER), item)).await), Status::NotFound);
        let mut long = revoke(action, Some(NO_PLAYER), item);
        long.input.insert("x".to_string(), "a".repeat(controlplane::MAX_INPUT_VALUE_BYTES + 1));
        assert_eq!(status(admin.invoke(long).await), Status::Invalid, "{action}");
    }
    let no_item = admin.invoke(revoke(REVOKE_SESSION, Some(NO_PLAYER), None)).await;
    assert_eq!(status(no_item), Status::Invalid);
    let bad_item = admin.invoke(revoke(REVOKE_SESSION, Some(NO_PLAYER), Some("fam"))).await;
    assert_eq!(status(bad_item), Status::Invalid);
}

#[tokio::test]
async fn revoke_all_prepare_names_the_player_and_counts_live_sessions() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("rev3-{}@test.local", suffix());
    let first = svc.register(email.clone(), "pw".into(), "RevPrep".into(), test_client()).await.unwrap();
    svc.login(email, "pw".into(), test_client()).await.unwrap();
    let id = first.player_id.clone();
    let handle = svc.store.player_row(&id).await.unwrap().map(|r| {
        crate::store::render_handle(&r.display_name, &r.discriminator)
    });
    let call = |id: &str| PrepareCall {
        module: "accounts".to_string(),
        action: REVOKE_ALL.to_string(),
        entity: Some(EntityRef { kind: "player".to_string(), id: id.to_string() }),
        item: None,
        operator: "op".to_string(),
    };
    let admin = AccountsAdmin::new(Arc::clone(&svc));
    let spec = admin.prepare(call(&id)).await;
    let rows = player_rows(&pool, &id).await;
    let missing = admin.prepare(call(NO_PLAYER)).await;
    cleanup_player(&pool, &id).await;

    let Then::Confirm(spec) = spec.unwrap() else { panic!("revoke-all prepares a Confirm") };
    assert_eq!(spec.title, format!("Revoke all sessions of {}?", handle.unwrap()));
    assert_eq!(spec.impact.len(), 2);
    assert_eq!(spec.impact[0].0, "Active sessions");
    assert_eq!(spec.impact[0].1.value, "2");
    assert_eq!(spec.impact[1].0, "Browser sessions");
    assert_eq!(spec.impact[1].1.value, "0");
    assert_eq!(rows, 4, "prepare writes nothing");
    assert_eq!(missing.unwrap_err().status, Status::NotFound);
}

/// A login holding the player row (FOR KEY SHARE through its family insert) when Revoke all
/// starts: the revoke waits for it, then takes its family too. The real `login` is parked at
/// its identity stamp by a third transaction holding the identity row.
#[tokio::test(flavor = "multi_thread")]
async fn revoke_all_waits_for_a_racing_login_and_revokes_it_whole() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("rev4-{}@test.local", suffix());
    let id = svc.register(email.clone(), "pw".into(), "RevRace".into(), test_client()).await.unwrap().player_id;
    let mut stamp_gate = pool.begin().await.unwrap();
    let gate_pid = backend_pid(&mut stamp_gate).await;
    sqlx::query("SELECT 1 FROM accounts.identities WHERE player_id = $1::uuid FOR UPDATE")
        .bind(&id)
        .execute(&mut *stamp_gate)
        .await
        .unwrap();
    let login = {
        let svc = Arc::clone(&svc);
        tokio::spawn(async move { svc.login(email, "pw".into(), test_client()).await })
    };
    let login_pid = waiter_of(&pool, gate_pid).await;
    let revoke = {
        let admin = AccountsAdmin::new(Arc::clone(&svc));
        let id = id.clone();
        tokio::spawn(async move { admin.invoke(revoke(REVOKE_ALL, Some(&id), None)).await })
    };
    let waited = login_pid.is_some() && waits_on(&pool, login_pid.unwrap()).await;
    stamp_gate.rollback().await.unwrap();
    let session = login.await.unwrap();
    let revoked = revoke.await.unwrap();
    let left = player_rows(&pool, &id).await;
    cleanup_player(&pool, &id).await;

    assert!(login_pid.is_some(), "the login must park at its identity stamp");
    assert!(waited, "Revoke all must wait on the login holding the player row");
    let session = session.unwrap();
    assert_eq!(session.player_id, id);
    assert!(matches!(revoked.unwrap(), InvokeOutcome::Ok { .. }));
    assert_eq!(left, 0, "the login that committed first is revoked whole: token and family");
}

/// A refresh that has taken its position when Revoke all starts: before the fix the refresh
/// held its token row and then wanted the player row Revoke all held, while Revoke all held
/// the player row and wanted the token, and Postgres aborted one with a deadlock. Now the
/// refresh takes the player row first, so Revoke all waits for it and then revokes the
/// rotated family whole. A third transaction holds the token row so the refresh parks there.
#[tokio::test(flavor = "multi_thread")]
async fn a_refresh_racing_revoke_all_finishes_first_and_is_revoked_whole() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("rev6-{}@test.local", suffix());
    let first = svc.register(email, "pw".into(), "RevRefresh".into(), test_client()).await.unwrap();
    let id = first.player_id.clone();
    let mut token_gate = pool.begin().await.unwrap();
    let gate_pid = backend_pid(&mut token_gate).await;
    sqlx::query("SELECT 1 FROM accounts.refresh_tokens WHERE token = $1 FOR UPDATE")
        .bind(&first.refresh_token)
        .execute(&mut *token_gate)
        .await
        .unwrap();
    let refresh = {
        let svc = Arc::clone(&svc);
        let token = first.refresh_token.clone();
        tokio::spawn(async move { svc.refresh(token).await })
    };
    let refresh_pid = waiter_of(&pool, gate_pid).await;
    let revoke = {
        let admin = AccountsAdmin::new(Arc::clone(&svc));
        let id = id.clone();
        tokio::spawn(async move { admin.invoke(revoke(REVOKE_ALL, Some(&id), None)).await })
    };
    let revoke_parked = waits_on_any(&pool, &[gate_pid, refresh_pid.unwrap_or(gate_pid)]).await;
    token_gate.rollback().await.unwrap();
    let rotated = refresh.await.unwrap();
    let revoked = revoke.await.unwrap();
    let left = player_rows(&pool, &id).await;
    cleanup_player(&pool, &id).await;

    assert!(refresh_pid.is_some(), "the refresh must park on the held token row");
    assert!(revoke_parked, "Revoke all must wait while the refresh holds its position");
    rotated.expect("the refresh finishes without a deadlock");
    assert!(matches!(revoked.expect("Revoke all finishes without a deadlock"), InvokeOutcome::Ok { .. }));
    assert_eq!(left, 0, "the rotated family is revoked whole: both tokens and the family");
}

/// A player whose only family is past both expiries has nothing Revoke all would end: the
/// modal counts 0 and the action is NoOp, deleting nothing.
#[tokio::test]
async fn revoke_all_counts_and_revokes_only_families_that_still_grant_access() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("rev7-{}@test.local", suffix());
    let id = svc.register(email, "pw".into(), "RevDead".into(), test_client()).await.unwrap().player_id;
    for sql in [EXPIRE_PLAYER_REFRESH, EXPIRE_PLAYER_ACCESS] {
        sqlx::query(sql)
        .bind(&id)
        .execute(&pool)
        .await
        .unwrap();
    }
    let admin = AccountsAdmin::new(Arc::clone(&svc));
    let spec = admin
        .prepare(PrepareCall {
            module: "accounts".to_string(),
            action: REVOKE_ALL.to_string(),
            entity: Some(EntityRef { kind: "player".to_string(), id: id.clone() }),
            item: None,
            operator: "op".to_string(),
        })
        .await;
    let outcome = admin.invoke(revoke(REVOKE_ALL, Some(&id), None)).await;
    let left = player_rows(&pool, &id).await;
    cleanup_player(&pool, &id).await;

    let Then::Confirm(spec) = spec.unwrap() else { panic!("revoke-all prepares a Confirm") };
    assert_eq!(spec.impact[0].1.value, "0");
    assert!(matches!(outcome.unwrap(), InvokeOutcome::NoOp { .. }));
    assert_eq!(left, 2, "a NoOp deletes nothing");
}

/// A player whose only login is a browser session: the modal counts it and Revoke all
/// ends it instead of answering NoOp and rolling the delete back.
#[tokio::test]
async fn revoke_all_counts_and_ends_a_browser_session() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("rev6-{}@test.local", suffix());
    let id = svc.register(email, "pw".into(), "RevBrowser".into(), test_client()).await.unwrap().player_id;
    let mut tx = pool.begin().await.unwrap();
    svc.store.kill_player_families_tx(&mut tx, &id).await.unwrap();
    tx.commit().await.unwrap();
    let browser = svc.store.create_browser_session(&id).await.unwrap().unwrap();
    let admin = AccountsAdmin::new(Arc::clone(&svc));
    let spec = admin
        .prepare(PrepareCall {
            module: "accounts".to_string(),
            action: REVOKE_ALL.to_string(),
            entity: Some(EntityRef { kind: "player".to_string(), id: id.clone() }),
            item: None,
            operator: "op".to_string(),
        })
        .await;
    let outcome = admin.invoke(revoke(REVOKE_ALL, Some(&id), None)).await;
    let read = svc.store.browser_session(&browser.token).await.unwrap();
    let left: i64 =
        sqlx::query_scalar("SELECT count(*) FROM accounts.browser_sessions WHERE player_id = $1::uuid")
            .bind(&id)
            .fetch_one(&pool)
            .await
            .unwrap();
    cleanup_player(&pool, &id).await;

    let Then::Confirm(spec) = spec.unwrap() else { panic!("revoke-all prepares a Confirm") };
    assert_eq!(spec.impact[0].1.value, "0", "no family grants access");
    assert_eq!(spec.impact[1].1.value, "1", "the modal counts the browser session");
    assert!(matches!(outcome.unwrap(), InvokeOutcome::Ok { .. }));
    assert!(read.is_none());
    assert_eq!(left, 0, "Revoke all deletes the browser session");
}

/// Revoke all holding the player row when a login starts: the login waits, then commits
/// after it, whole.
#[tokio::test(flavor = "multi_thread")]
async fn a_login_racing_revoke_all_waits_and_survives_whole() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("rev5-{}@test.local", suffix());
    let id = svc.register(email.clone(), "pw".into(), "RevLate".into(), test_client()).await.unwrap().player_id;
    let mut gate = pool.begin().await.unwrap();
    let gate_pid = backend_pid(&mut gate).await;
    let killed = svc.store.kill_player_families_tx(&mut gate, &id).await.unwrap();
    let task = {
        let svc = Arc::clone(&svc);
        tokio::spawn(async move { svc.login(email, "pw".into(), test_client()).await })
    };
    let waited = waits_on(&pool, gate_pid).await;
    gate.commit().await.unwrap();
    let session = task.await.unwrap();
    let left = player_rows(&pool, &id).await;
    cleanup_player(&pool, &id).await;

    assert_eq!(killed, Some(1), "the registration's family is revoked");
    assert!(waited, "the login must wait on Revoke all's player row lock");
    session.unwrap();
    assert_eq!(left, 2, "the login after the revoke survives whole: token and family");
}

#[test]
fn fakes_draw_pinned_values_from_the_shared_helper() {
    assert_eq!(fake_ip("fam-1"), "198.51.100.64");
}

#[test]
fn thousands_groups_digits_and_mask_hides_a_short_subject() {
    assert_eq!(thousands(1_234_567), "1,234,567");
    assert_eq!(thousands(999), "999");
    assert_eq!(thousands(-3), "0");
    assert_eq!(mask("short"), "…", "a subject of eight characters or fewer shows nothing");
}

fn create_call(email: &str, display: &str) -> InvokeCall {
    InvokeCall {
        input: BTreeMap::from([
            (FIELD_EMAIL.to_string(), email.to_string()),
            (FIELD_DISPLAY_NAME.to_string(), display.to_string()),
        ]),
        ..revoke(CREATE_ACCOUNT, None, None)
    }
}

fn prepare_call(action: &str, player: Option<&str>) -> PrepareCall {
    PrepareCall {
        module: "accounts".to_string(),
        action: action.to_string(),
        entity: player.map(|id| EntityRef { kind: "player".to_string(), id: id.to_string() }),
        item: None,
        operator: "op".to_string(),
    }
}

/// The password a reveal shows once, from an `Ok` outcome with no message and no redirect.
fn revealed(outcome: InvokeOutcome) -> (String, String) {
    let InvokeOutcome::Ok { message, reveal: Some(reveal), redirect: None } = outcome else {
        panic!("a password action answers a reveal: {outcome:?}");
    };
    assert_eq!(message, "", "the reveal modal is the whole answer");
    assert_eq!(reveal.value.len(), 20, "a 20-character password");
    assert!(reveal.value.bytes().all(|b| b.is_ascii_alphanumeric()), "an alphanumeric password");
    (reveal.label, reveal.value)
}

#[tokio::test]
async fn create_account_reveals_a_password_that_signs_in_and_a_taken_email_is_a_field_error() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let admin = AccountsAdmin::new(Arc::clone(&svc));
    let lower = format!("opmade-{}@test.local", suffix()).to_ascii_lowercase();
    let mixed = format!("  {}  ", lower.to_ascii_uppercase());
    let form = admin.prepare(prepare_call(CREATE_ACCOUNT, None)).await;
    let created = admin.invoke(create_call(&mixed, "OpMade")).await.unwrap();
    let (label, password) = revealed(created);
    let stored: (String, String) = sqlx::query_as(
        "SELECT player_id::text, secret_hash FROM accounts.identities WHERE provider = 'password' AND subject = $1",
    )
    .bind(&lower)
    .fetch_one(&pool)
    .await
    .unwrap();
    let id = stored.0.clone();
    let families_after_create: i64 =
        sqlx::query_scalar("SELECT count(*) FROM accounts.token_families WHERE player_id = $1::uuid")
            .bind(&id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let registered = asyncevents::testing::events_count(&pool, "player.registered", "player_id", &id)
        .await
        .unwrap();
    let login = svc.login(lower.clone(), password.clone(), test_client()).await;
    let same = admin.invoke(create_call(&lower, "OpMadeAgain")).await;
    let other_case = admin.invoke(create_call(&lower.to_ascii_uppercase(), "OpMadeAgain")).await;
    let blank = admin.invoke(create_call("  ", "OpMade")).await;
    let no_at = admin.invoke(create_call("not-an-address", "OpMade")).await;
    let no_name = admin.invoke(create_call(&lower, " ")).await;
    let players: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM accounts.identities WHERE provider = 'password' AND subject = $1",
    )
    .bind(&lower)
    .fetch_one(&pool)
    .await
    .unwrap();
    cleanup_player(&pool, &id).await;

    let Then::Form(form) = form.unwrap() else { panic!("create-account prepares a Form") };
    let keys: Vec<&str> = form.fields.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(keys, [FIELD_EMAIL, FIELD_DISPLAY_NAME], "no password field: it is generated");
    assert_eq!(label, format!("Account created — {lower}"), "the email is normalised");
    assert!(stored.1.starts_with("$argon2id$"), "only the argon2 hash is stored");
    assert!(!stored.1.contains(&password));
    assert_eq!(families_after_create, 0, "Create account mints no token family");
    assert_eq!(registered, 1, "one player.registered rides the create's tx");
    assert_eq!(login.unwrap().player_id, id, "the revealed password signs in");
    let email_error = |outcome: Result<InvokeOutcome, opsapi::Error>, key: &str| match outcome.unwrap() {
        InvokeOutcome::FieldErrors(errors) => errors.contains_key(key),
        other => panic!("a refused create is a field error: {other:?}"),
    };
    assert!(email_error(same, FIELD_EMAIL), "a taken email is a field error with no reveal");
    assert!(email_error(other_case, FIELD_EMAIL), "the same email in other case is taken");
    assert!(email_error(blank, FIELD_EMAIL));
    assert!(email_error(no_at, FIELD_EMAIL));
    assert!(email_error(no_name, FIELD_DISPLAY_NAME));
    assert_eq!(players, 1, "a refused create writes nothing");
}

#[tokio::test]
async fn reset_password_ends_every_login_and_only_the_new_password_signs_in() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("Reset-{}@test.local", suffix());
    let lower = email.to_ascii_lowercase();
    let first = svc.register(email, "old-pw".into(), "ResetMe".into(), test_client()).await.unwrap();
    let id = first.player_id.clone();
    let browser = svc.store.create_browser_session(&id).await.unwrap().unwrap();
    let admin = AccountsAdmin::new(Arc::clone(&svc));
    let spec = admin.prepare(prepare_call(RESET_PASSWORD, Some(&id))).await;
    let reset = admin.invoke(revoke(RESET_PASSWORD, Some(&id), None)).await.unwrap();
    let (label, password) = revealed(reset);
    let refreshed = svc.refresh(first.refresh_token.clone()).await;
    let bearer = svc.tokens.verify_access(&first.token).await.unwrap();
    let browser_left = svc.store.browser_session(&browser.token).await.unwrap();
    let old_login = svc.login(lower.clone(), "old-pw".into(), test_client()).await;
    let new_login = svc.login(lower.clone(), password.clone(), test_client()).await;
    let again = admin.invoke(revoke(RESET_PASSWORD, Some(&id), None)).await.unwrap();
    let (_, second) = revealed(again);
    cleanup_player(&pool, &id).await;

    let Then::Confirm(spec) = spec.unwrap() else { panic!("reset-password prepares a Confirm") };
    assert!(spec.title.starts_with("Reset the password of ResetMe#"), "{}", spec.title);
    assert_eq!(spec.impact[0].1.value, lower, "the modal names the sign-in email");
    assert_eq!(label, format!("Password reset — {lower}"));
    assert_eq!(refreshed.unwrap_err().status, Status::Unauthorized, "the old refresh token fails");
    assert!(bearer.is_none(), "the old access token is refused");
    assert!(browser_left.is_none(), "the browser session is gone");
    assert_eq!(old_login.unwrap_err().status, Status::Unauthorized, "the old password fails");
    assert_eq!(new_login.unwrap().player_id, id, "the revealed password signs in");
    assert_ne!(second, password, "every reset generates a new password");
}

/// A password checked against the old hash just before a reset commits mints nothing
/// after it, through either mint. Ordered by hand: verify, reset, then mint.
#[tokio::test]
async fn a_password_verified_before_a_reset_mints_no_session_after_it() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("stale-{}@test.local", suffix());
    let lower = email.to_ascii_lowercase();
    let id = svc.register(email, "old-pw".into(), "Stale".into(), test_client()).await.unwrap().player_id;
    let verified = svc.verify_password(&lower, "old-pw".into()).await.unwrap();
    let admin = AccountsAdmin::new(Arc::clone(&svc));
    admin.invoke(revoke(RESET_PASSWORD, Some(&id), None)).await.unwrap();
    let bearer = svc.issue_password_session(&verified, TEST_CLIENT_ID).await;
    let browser = svc.issue_password_browser_session(&verified, None).await;
    let rows: (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM accounts.refresh_tokens WHERE player_id = $1::uuid), \
                (SELECT count(*) FROM accounts.token_families WHERE player_id = $1::uuid), \
                (SELECT count(*) FROM accounts.browser_sessions WHERE player_id = $1::uuid)",
    )
    .bind(&id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let fresh = svc.verify_password(&lower, "old-pw".into()).await;
    cleanup_player(&pool, &id).await;

    assert_eq!(bearer.unwrap_err().status, Status::Unauthorized, "the bearer mint is refused");
    assert_eq!(browser.err().map(|e| e.status), Some(Status::Unauthorized), "the browser mint is refused");
    assert_eq!(rows, (0, 0, 0), "no refresh token, family or browser session is left");
    assert_eq!(fresh.err().map(|e| e.status), Some(Status::Unauthorized), "the old password fails anew");
}

#[tokio::test]
async fn reset_password_refuses_a_player_without_a_password_and_a_missing_player() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let mut tx = pool.begin().await.unwrap();
    let subject = format!("epic-only-{}", suffix());
    let epic = svc
        .store
        .insert_player_with_identity_tx(&mut tx, "epic", &subject, "EpicOnly", None)
        .await
        .unwrap_or_else(|_| panic!("an epic-only player"));
    let session = svc
        .issue_session_tx(
            &mut tx,
            &epic,
            crate::MintedVia { provider: "epic", subject: &subject, secret_hash: None },
            TEST_CLIENT_ID,
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let browser = svc.store.create_browser_session(&epic.id).await.unwrap().unwrap();
    let admin = AccountsAdmin::new(Arc::clone(&svc));
    let spec = admin.prepare(prepare_call(RESET_PASSWORD, Some(&epic.id))).await;
    let reset = admin.invoke(revoke(RESET_PASSWORD, Some(&epic.id), None)).await;
    let bearer = svc.tokens.verify_access(&session.token).await.unwrap();
    let browser_left = svc.store.browser_session(&browser.token).await.unwrap();
    let refresh_left: i64 = sqlx::query_scalar("SELECT count(*) FROM accounts.refresh_tokens WHERE token = $1")
        .bind(&session.refresh_token)
        .fetch_one(&pool)
        .await
        .unwrap();
    let hash: Option<String> =
        sqlx::query_scalar("SELECT secret_hash FROM accounts.identities WHERE provider = 'epic' AND subject = $1")
            .bind(&subject)
            .fetch_one(&pool)
            .await
            .unwrap();
    let missing = admin.invoke(revoke(RESET_PASSWORD, Some(NO_PLAYER), None)).await;
    let missing_spec = admin.prepare(prepare_call(RESET_PASSWORD, Some(NO_PLAYER))).await;
    let unbound = admin.invoke(revoke(RESET_PASSWORD, None, None)).await;
    cleanup_player(&pool, &epic.id).await;

    assert_eq!(spec.unwrap_err().status, Status::Conflict, "no password to reset: refused before the modal");
    assert_eq!(reset.unwrap_err().status, Status::Conflict);
    assert!(hash.is_none(), "a refused reset writes no hash onto another identity");
    assert!(bearer.is_some(), "a refused reset keeps the access token");
    assert_eq!(refresh_left, 1, "a refused reset keeps the refresh family");
    assert!(browser_left.is_some(), "a refused reset keeps the browser session");
    assert_eq!(missing.unwrap_err().status, Status::NotFound);
    assert_eq!(missing_spec.unwrap_err().status, Status::NotFound);
    assert_eq!(unbound.unwrap_err().status, Status::Invalid);
}

/// Owner decision 6: the Sessions panel lists live browser sessions beside the refresh
/// families, with nothing to act on and no token shown.
#[tokio::test]
async fn sessions_panel_lists_live_browser_sessions_without_their_token() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("brw-{}@test.local", suffix());
    let id = svc.register(email, "pw".into(), "BrowserRow".into(), test_client()).await.unwrap().player_id;
    let browser = svc.store.create_browser_session(&id).await.unwrap().unwrap();
    let expired = svc.store.create_browser_session(&id).await.unwrap().unwrap();
    sqlx::query("UPDATE accounts.browser_sessions SET expires_at = now() - interval '1 minute' WHERE token_digest = $1")
        .bind(crate::web::session::digest(&expired.token))
        .execute(&pool)
        .await
        .unwrap();
    let admin = AccountsAdmin::new(Arc::clone(&svc));
    let body = admin.panel(player_query("sessions", &id)).await.unwrap();
    cleanup_player(&pool, &id).await;

    assert_eq!(body.meta.as_deref(), Some("2"), "one family and one live browser session");
    let t = table(body);
    assert_eq!(t.rows.len(), 2, "the expired browser session is not listed");
    let row = &t.rows[0];
    assert_eq!(row.cells[0].value, "Browser sign-in");
    assert!(row.cells[3].value.starts_with("in "), "{}", row.cells[3].value);
    assert!(row.item.is_none() && row.actions.is_empty(), "nothing acts on one browser session");
    for cell in &row.cells {
        assert!(!cell.value.contains(&browser.token) && !cell.value.contains(&browser.csrf_token));
    }
}

/// Families and browser sessions share one newest-first list capped at SESSIONS_SHOWN.
#[test]
fn sessions_panel_merges_both_kinds_newest_first_under_one_cap() {
    let shown = SESSIONS_SHOWN as usize;
    // Families start on even microseconds, browser sessions on odd ones, interleaved.
    let families: Vec<FamilyRow> = (0..shown as i64)
        .map(|i| FamilyRow {
            family_id: format!("family-{i}"),
            started: String::new(),
            started_us: 2 * i,
            expires_in_secs: 60,
            client: String::new(),
        })
        .collect();
    let browsers: Vec<BrowserSessionRow> = (0..shown as i64)
        .map(|i| BrowserSessionRow { started: String::new(), started_us: 2 * i + 1, expires_in_secs: 60 })
        .collect();
    let rows = session_rows(&families, &browsers);

    assert_eq!(rows.len(), shown, "one cap over both kinds");
    assert_eq!(rows[0].cells[0].value, "Browser sign-in", "the newest start comes first");
    let newest_family = format!("family-{}", shown - 1);
    assert_eq!(rows[1].item.as_deref(), Some(newest_family.as_str()));
    let oldest_kept = format!("family-{}", shown / 2);
    assert_eq!(rows[shown - 1].item.as_deref(), Some(oldest_kept.as_str()), "the oldest half is cut");
}

/// The conformance probes of Create account's caps answer at and over each cap through
/// `invoke`: at the cap the value passes every check and reaches the (dead) store.
#[test]
fn create_account_caps_refuse_one_byte_over_and_pass_at_the_cap() {
    use crate::conformance::{
        conformance_admin_create_display_name_rejected as display,
        conformance_admin_create_email_rejected as email,
    };
    assert!(!email(crate::MAX_EMAIL_BYTES) && email(crate::MAX_EMAIL_BYTES + 1));
    let cap = accountsapi::MAX_DISPLAY_NAME_BYTES;
    assert!(!display(cap) && display(cap + 1));
}

/// A create-client call naming `display`, picking `grants` (a JSON list) and setting each
/// `(field, value)` of `fields`: confidential, audience, redirect URIs, root domains, scopes.
fn create_client_call(display: &str, grants: Option<&str>, fields: &[(&str, &str)]) -> InvokeCall {
    let mut input = BTreeMap::from([(FIELD_DISPLAY_NAME.to_string(), display.to_string())]);
    if let Some(grants) = grants {
        input.insert(FIELD_GRANTS.to_string(), grants.to_string());
    }
    for (key, value) in fields {
        input.insert(key.to_string(), value.to_string());
    }
    InvokeCall { input, ..revoke(CREATE_CLIENT, None, None) }
}

async fn secret_digest_of(pool: &PgPool, client_id: &str) -> Option<String> {
    sqlx::query_scalar("SELECT secret_digest FROM accounts.oidc_clients WHERE client_id = $1")
        .bind(client_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn delete_client_call(id: &str) -> InvokeCall {
    InvokeCall {
        entity: Some(EntityRef { kind: "oidc_client".to_string(), id: id.to_string() }),
        ..revoke(DELETE_CLIENT, None, None)
    }
}

#[tokio::test]
async fn create_client_refuses_a_blank_name_and_no_grants_with_a_field_error() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let admin = AccountsAdmin::new(svc);
    let all = r#"["password"]"#;
    let refused = [
        (create_client_call("", Some(all), &[]), FIELD_DISPLAY_NAME),
        (create_client_call("   ", Some(all), &[]), FIELD_DISPLAY_NAME),
        (create_client_call(&format!("no-grants-{}", suffix()), None, &[]), FIELD_GRANTS),
        (create_client_call(&format!("no-grants-{}", suffix()), Some(""), &[]), FIELD_GRANTS),
        (create_client_call(&format!("no-grants-{}", suffix()), Some("[]"), &[]), FIELD_GRANTS),
        (
            create_client_call(&format!("bad-grant-{}", suffix()), Some(r#"["implicit"]"#), &[]),
            FIELD_GRANTS,
        ),
    ];
    for (call, key) in refused {
        let input = call.input.clone();
        match admin.invoke(call).await.unwrap() {
            InvokeOutcome::FieldErrors(errors) => {
                assert_eq!(errors.keys().collect::<Vec<_>>(), [key], "{input:?}");
            }
            other => panic!("a refused create is a field error on {key}: {input:?} gave {other:?}"),
        }
    }
}

#[tokio::test]
async fn create_client_stores_a_swarog_client_and_delete_removes_it_once() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let admin = AccountsAdmin::new(svc);
    let name = format!("admin-client-{}", suffix());
    let call = create_client_call(&format!("  {name}  "), Some(r#"["refresh_token","password"]"#), &[]);
    let created = admin.invoke(call).await.unwrap();
    let InvokeOutcome::Ok { message, reveal: None, redirect: None } = created else {
        panic!("a create answers a message: {created:?}");
    };
    let id = message.strip_prefix("Client created — ").expect("the message names the id");
    let id = id.to_string();
    assert!(id.starts_with("c_") && id.len() == 24, "a generated c_ id: {id}");
    let row = admin.svc.store.client(&id).await.unwrap().expect("the created client is stored");
    assert_eq!(row.display_name, name, "the name is trimmed");
    assert_eq!(row.audience, accountsapi::SWAROG_AUDIENCE);
    assert_eq!(row.grant_types, ["password", "refresh_token"], "grants in grant_types::ALL order");
    assert!(!row.confidential, "a create not marked confidential is public");
    assert_eq!(row.device_code_ttl_secs, 600, "a create without a lifetime stores 10 minutes");
    assert_eq!(secret_digest_of(&pool, &id).await, None, "a public client stores no secret digest");

    let deleted = admin.invoke(delete_client_call(&id)).await.unwrap();
    let InvokeOutcome::Ok { message, .. } = deleted else {
        panic!("a delete answers a message: {deleted:?}");
    };
    assert_eq!(message, "Client deleted");
    assert!(admin.svc.store.client(&id).await.unwrap().is_none(), "the row is gone");
    let again = admin.invoke(delete_client_call(&id)).await.unwrap_err();
    assert_eq!(again.status, Status::NotFound, "a repeat delete names no client");
}

#[tokio::test]
async fn create_client_reveals_a_confidential_secret_once_and_stores_only_its_digest() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let admin = AccountsAdmin::new(svc);
    let call = create_client_call(
        &format!("confidential-{}", suffix()),
        Some(r#"["refresh_token","authorization_code"]"#),
        &[(FIELD_CONFIDENTIAL, "true"), (FIELD_REDIRECT_URIS, "https://game.example/callback")],
    );
    let created = admin.invoke(call).await.unwrap();
    let InvokeOutcome::Ok { message, reveal: Some(reveal), redirect: None } = created else {
        panic!("a confidential create reveals its secret: {created:?}");
    };
    let id = reveal.label.strip_prefix("Client created — ").expect("the label names the id");
    let id = id.to_string();
    let stored = secret_digest_of(&pool, &id).await;
    let row = admin.svc.store.client(&id).await.unwrap().expect("the created client is stored");
    delete_client(&pool, &id).await;

    assert_eq!(message, "", "the reveal modal is the whole answer");
    assert!(reveal.value.starts_with(crate::secret::CLIENT_SECRET_PREFIX), "a cs_ secret");
    assert_eq!(
        stored.as_deref(),
        Some(crate::secret::digest(&reveal.value).as_str()),
        "the stored digest is the revealed secret's, never the secret"
    );
    assert!(row.confidential, "the row reads back confidential");
    assert_eq!(row.grant_types, ["authorization_code", "refresh_token"], "grants in grant_types::ALL order");
    assert_eq!(row.redirect_uris, ["https://game.example/callback"]);
}

/// 63 + 1 + 63 + 1 + 63 + 1 + 61 bytes: a valid root domain of exactly the 253-byte cap,
/// made distinct by `lead`.
fn longest_root_domain(lead: char) -> String {
    format!("{lead}{}.{}.{}.{}", "a".repeat(62), "b".repeat(63), "c".repeat(63), "d".repeat(61))
}

#[tokio::test]
async fn create_client_refuses_each_bad_field_with_a_field_error_and_stores_nothing() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let admin = AccountsAdmin::new(svc);
    let name = || format!("refused-client-{}", suffix());
    let password = Some(r#"["password"]"#);
    let long_audience = "a".repeat(129);
    let domains: Vec<String> = "abcdefgh".chars().map(longest_root_domain).collect();
    assert_eq!(domains.len(), crate::clients::MAX_ROOT_DOMAINS, "as many domains as a client may list");
    assert!(domains.iter().all(|d| d.len() == crate::clients::MAX_ROOT_DOMAIN_BYTES));
    assert!(domains.iter().all(|d| crate::clients::root_domain_refusal(d).is_none()), "each domain is valid");
    let domains = domains.join("\n");
    let refused = [
        (
            create_client_call(&name(), password, &[(FIELD_REDIRECT_URIS, "ftp://game.example/cb")]),
            FIELD_REDIRECT_URIS,
            "neither https",
        ),
        (
            create_client_call(&name(), password, &[(FIELD_AUDIENCE, long_audience.as_str())]),
            FIELD_AUDIENCE,
            "at most 128 bytes",
        ),
        (
            create_client_call(&name(), Some(r#"["authorization_code"]"#), &[]),
            FIELD_REDIRECT_URIS,
            "needs at least one redirect URI",
        ),
        (
            create_client_call(
                &name(),
                Some(r#"["password","refresh_token"]"#),
                &[(FIELD_CONFIDENTIAL, "true"), (FIELD_REDIRECT_URIS, "https://game.example/cb")],
            ),
            FIELD_GRANTS,
            "only authorization_code and refresh_token",
        ),
        (
            create_client_call(
                &name(),
                Some(r#"["refresh_token","urn:ietf:params:oauth:grant-type:device_code"]"#),
                &[(FIELD_CONFIDENTIAL, "true")],
            ),
            FIELD_GRANTS,
            "only authorization_code and refresh_token",
        ),
        (
            create_client_call(&name(), password, &[(FIELD_DEVICE_CODE_TTL, "119s")]),
            FIELD_DEVICE_CODE_TTL,
            "from 2m to 30m",
        ),
        (
            create_client_call(&name(), password, &[(FIELD_DEVICE_CODE_TTL, "31m")]),
            FIELD_DEVICE_CODE_TTL,
            "from 2m to 30m",
        ),
        (
            create_client_call(&name(), password, &[(FIELD_DEVICE_CODE_TTL, "10")]),
            FIELD_DEVICE_CODE_TTL,
            "digits followed by",
        ),
        (
            create_client_call(&name(), password, &[(FIELD_ROOT_DOMAINS, domains.as_str())]),
            FIELD_ROOT_DOMAINS,
            "a verifier accepts",
        ),
    ];
    for (call, key, says) in refused {
        let display = call.input[FIELD_DISPLAY_NAME].clone();
        let outcome = admin.invoke(call).await.unwrap();
        let InvokeOutcome::FieldErrors(errors) = &outcome else {
            panic!("a refused create is a field error on {key}: {outcome:?}");
        };
        assert_eq!(errors.keys().collect::<Vec<_>>(), [key], "{errors:?}");
        assert!(errors[key].contains(says), "{key}: {:?} names {says:?}", errors[key]);
        let stored: i64 =
            sqlx::query_scalar("SELECT count(*) FROM accounts.oidc_clients WHERE display_name = $1")
                .bind(&display)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(stored, 0, "a refused create stores no client: {key}");
    }
}

/// A create that picks a lifetime stores it; the Device code lifetime row action opens filled
/// with it, stores a lifetime at either bound, and refuses one outside them, a blank one, an
/// unbound call and a missing client.
#[tokio::test]
async fn device_code_lifetime_is_picked_on_create_and_changed_by_the_row_action() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let admin = AccountsAdmin::new(svc);
    let name = format!("device-client-{}", suffix());
    let grants = Some(r#"["refresh_token","urn:ietf:params:oauth:grant-type:device_code"]"#);
    let created = admin.invoke(create_client_call(&name, grants, &[(FIELD_DEVICE_CODE_TTL, "15m")])).await.unwrap();
    let InvokeOutcome::Ok { message, reveal: None, .. } = created else {
        panic!("a public create answers a message: {created:?}");
    };
    let id = message.strip_prefix("Client created — ").expect("the message names the id").to_string();
    let row = admin.svc.store.client(&id).await.unwrap().unwrap();
    assert_eq!(row.grant_types, ["refresh_token", accountsapi::grant_types::DEVICE_CODE]);
    assert_eq!(row.device_code_ttl_secs, 900, "the picked lifetime is stored");

    let bound = Some(EntityRef { kind: "oidc_client".to_string(), id: id.clone() });
    let prepare = PrepareCall { entity: bound.clone(), ..prepare_call(SET_DEVICE_CODE_TTL, None) };
    let Then::Form(form) = admin.prepare(prepare).await.unwrap() else {
        panic!("the row action opens a form");
    };
    assert_eq!(form.title.as_deref(), Some(format!("Device code lifetime of {name}").as_str()));
    assert_eq!(form.fields.len(), 1);
    assert_eq!((form.fields[0].key.as_str(), form.fields[0].default.as_deref()), (FIELD_DEVICE_CODE_TTL, Some("15m")));

    let set = |raw: &str, entity: Option<EntityRef>| InvokeCall {
        entity,
        input: BTreeMap::from([(FIELD_DEVICE_CODE_TTL.to_string(), raw.to_string())]),
        ..revoke(SET_DEVICE_CODE_TTL, None, None)
    };
    let stored = || async { admin.svc.store.client(&id).await.unwrap().unwrap().device_code_ttl_secs };
    for (raw, secs, shown) in [("120s", 120, "2m"), ("30m", 1800, "30m"), ("1h", 1800, "")] {
        let outcome = admin.invoke(set(raw, bound.clone())).await.unwrap();
        if shown.is_empty() {
            let InvokeOutcome::FieldErrors(errors) = &outcome else { panic!("{raw} is refused: {outcome:?}") };
            assert!(errors[FIELD_DEVICE_CODE_TTL].contains("from 2m to 30m"), "{errors:?}");
        } else {
            let InvokeOutcome::Ok { message, .. } = &outcome else { panic!("{raw} is stored: {outcome:?}") };
            assert_eq!(message, &format!("Device code lifetime set to {shown}"));
        }
        assert_eq!(stored().await, secs, "{raw}");
    }
    for raw in ["119s", "31m", "", "99999999999999999999d"] {
        let outcome = admin.invoke(set(raw, bound.clone())).await.unwrap();
        let InvokeOutcome::FieldErrors(errors) = &outcome else { panic!("{raw:?} is refused: {outcome:?}") };
        assert_eq!(errors.keys().collect::<Vec<_>>(), [FIELD_DEVICE_CODE_TTL], "{raw:?}");
        assert_eq!(stored().await, 1800, "a refused {raw:?} changes nothing");
    }
    assert_eq!(admin.invoke(set("5m", None)).await.unwrap_err().status, Status::Invalid, "no client bound");
    let missing = Some(EntityRef { kind: "oidc_client".to_string(), id: "c_no_such_client".to_string() });
    assert_eq!(admin.invoke(set("5m", missing)).await.unwrap_err().status, Status::NotFound);

    delete_client(&pool, &id).await;
}

/// The table's CHECK holds the same bounds as the portal: a lifetime one second outside
/// either bound is refused, one at each bound stored.
#[tokio::test]
async fn the_clients_table_refuses_a_device_code_lifetime_outside_the_bounds() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, _svc) = wired(&pool).await;
    let id = insert_client(&pool, &["password"]).await;
    let set = |secs: i32| {
        sqlx::query("UPDATE accounts.oidc_clients SET device_code_ttl_secs = $2 WHERE client_id = $1")
            .bind(&id)
            .bind(secs)
            .execute(&pool)
    };
    use crate::clients::{MAX_DEVICE_CODE_TTL_SECS as MAX, MIN_DEVICE_CODE_TTL_SECS as MIN};
    for secs in [MIN - 1, MAX + 1] {
        let err = set(secs).await.expect_err("out of bounds");
        let code = err.as_database_error().and_then(|e| e.code()).map(|c| c.to_string());
        assert_eq!(code.as_deref(), Some("23514"), "{secs}: a check violation, {err}");
    }
    for secs in [MIN, MAX] {
        set(secs).await.unwrap();
    }
    delete_client(&pool, &id).await;
}

async fn client_families(pool: &PgPool, client_id: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM accounts.token_families WHERE client_id = $1")
        .bind(client_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// A login holding its client row (FOR KEY SHARE) when the client is deleted: the delete
/// waits for it, then takes the login's family by cascade. The real `login` is parked right
/// after its client check, at the family insert's FK check, by a third transaction holding
/// the player row: the family insert has not locked the client yet.
#[tokio::test(flavor = "multi_thread")]
async fn a_client_delete_waits_for_a_racing_login_and_cascades_its_family() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let fk_checks: Vec<String> = sqlx::query_scalar(
        "SELECT a.attname::text FROM pg_trigger t \
           JOIN pg_constraint c ON c.oid = t.tgconstraint \
           JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = c.conkey[1] \
          WHERE t.tgrelid = 'accounts.token_families'::regclass AND c.contype = 'f' \
            AND t.tgfoid = '\"RI_FKey_check_ins\"'::regproc \
          ORDER BY t.tgname",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        fk_checks,
        ["player_id", "client_id"],
        "the family insert checks its player FK before its client FK, so the player gate parks it first",
    );
    let grants = [accountsapi::grant_types::PASSWORD, accountsapi::grant_types::REFRESH_TOKEN];
    let client = insert_client(&pool, &grants).await;
    let email = format!("delcl1-{}@test.local", suffix());
    let id = svc.register(email.clone(), "pw".into(), "DelClLogin".into(), client.clone()).await.unwrap().player_id;
    let mut player_gate = pool.begin().await.unwrap();
    let gate_pid = backend_pid(&mut player_gate).await;
    sqlx::query("SELECT 1 FROM accounts.players WHERE id = $1::uuid FOR UPDATE")
        .bind(&id)
        .execute(&mut *player_gate)
        .await
        .unwrap();
    let login = {
        let svc = Arc::clone(&svc);
        let client = client.clone();
        tokio::spawn(async move { svc.login(email, "pw".into(), client).await })
    };
    let login_pid = waiter_of(&pool, gate_pid).await;
    let delete = {
        let admin = AccountsAdmin::new(Arc::clone(&svc));
        let client = client.clone();
        tokio::spawn(async move { admin.invoke(delete_client_call(&client)).await })
    };
    let waited = login_pid.is_some() && waits_on(&pool, login_pid.unwrap()).await;
    player_gate.rollback().await.unwrap();
    let session = login.await.unwrap();
    let deleted = delete.await.unwrap();
    let families = client_families(&pool, &client).await;
    let left = player_rows(&pool, &id).await;
    cleanup_player(&pool, &id).await;

    assert!(login_pid.is_some(), "the login must park at its family insert");
    assert!(waited, "the client delete must wait on the login holding the client row");
    assert_eq!(session.expect("the login commits first").player_id, id);
    assert!(matches!(deleted.expect("the delete finishes without a 500"), InvokeOutcome::Ok { .. }));
    assert_eq!(families, 0, "the delete cascades the family the login committed");
    assert_eq!(left, 0, "no token or session of the deleted client is left");
}

/// A refresh holding its family row (FOR KEY SHARE) when the client is deleted: the
/// delete's cascade waits for it, then takes the rotated family whole. A third
/// transaction holds the token row so the refresh parks there.
#[tokio::test(flavor = "multi_thread")]
async fn a_client_delete_waits_for_a_racing_refresh_and_cascades_its_family() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let grants = [accountsapi::grant_types::PASSWORD, accountsapi::grant_types::REFRESH_TOKEN];
    let client = insert_client(&pool, &grants).await;
    let email = format!("delcl2-{}@test.local", suffix());
    let first = svc.register(email, "pw".into(), "DelClRefresh".into(), client.clone()).await.unwrap();
    let id = first.player_id.clone();
    let mut token_gate = pool.begin().await.unwrap();
    let gate_pid = backend_pid(&mut token_gate).await;
    sqlx::query("SELECT 1 FROM accounts.refresh_tokens WHERE token = $1 FOR UPDATE")
        .bind(&first.refresh_token)
        .execute(&mut *token_gate)
        .await
        .unwrap();
    let refresh = {
        let svc = Arc::clone(&svc);
        let token = first.refresh_token.clone();
        tokio::spawn(async move { svc.refresh(token).await })
    };
    let refresh_pid = waiter_of(&pool, gate_pid).await;
    let delete = {
        let admin = AccountsAdmin::new(Arc::clone(&svc));
        let client = client.clone();
        tokio::spawn(async move { admin.invoke(delete_client_call(&client)).await })
    };
    let waited = refresh_pid.is_some() && waits_on(&pool, refresh_pid.unwrap()).await;
    token_gate.rollback().await.unwrap();
    let rotated = refresh.await.unwrap();
    let deleted = delete.await.unwrap();
    let families = client_families(&pool, &client).await;
    let left = player_rows(&pool, &id).await;
    cleanup_player(&pool, &id).await;

    assert!(refresh_pid.is_some(), "the refresh must park on the held token row");
    assert!(waited, "the client delete must wait on the refresh holding the family row");
    rotated.expect("the refresh commits first");
    assert!(matches!(deleted.expect("the delete finishes without a 500"), InvokeOutcome::Ok { .. }));
    assert_eq!(families, 0, "the delete cascades the rotated family");
    assert_eq!(left, 0, "no token or session of the deleted client is left");
}

/// An admin action that deletes several token families at once.
#[derive(Clone, Copy)]
enum FamilyDeleter {
    RevokeAll,
    ResetPassword,
    DeleteClient,
}

fn spawn_deleter(
    svc: &Arc<crate::Service>,
    deleter: FamilyDeleter,
    player: &str,
    client: &str,
) -> tokio::task::JoinHandle<Result<(), String>> {
    let svc = Arc::clone(svc);
    let (player, client) = (player.to_string(), client.to_string());
    tokio::spawn(async move {
        let admin = AccountsAdmin::new(Arc::clone(&svc));
        let outcome = match deleter {
            FamilyDeleter::RevokeAll => admin.invoke(revoke(REVOKE_ALL, Some(&player), None)).await,
            FamilyDeleter::DeleteClient => admin.invoke(delete_client_call(&client)).await,
            FamilyDeleter::ResetPassword => {
                return svc.reset_password(&player).await.map(drop).map_err(|e| format!("{e:?}"));
            }
        };
        match outcome {
            Ok(InvokeOutcome::Ok { .. }) => Ok(()),
            other => Err(format!("{other:?}")),
        }
    })
}

/// Two deletes racing over seven live families of one player under one client. The
/// families are inserted in descending id, so a scan without an order meets the largest
/// first, and a third transaction holds the largest (FOR KEY SHARE). `first` parks there,
/// and taking families in ascending id it has locked every other one by then, which a probe
/// skipping locked rows sees as none left to lock. `second` then waits behind `first`,
/// and both finish without a deadlock.
async fn race_families(first: FamilyDeleter, second: FamilyDeleter) {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client = insert_client(&pool, &[accountsapi::grant_types::PASSWORD]).await;
    let email = format!("racefam-{}@test.local", suffix());
    let id = svc.register(email, "pw".into(), "RaceFamilies".into(), test_client()).await.unwrap().player_id;
    let ids: Vec<String> = sqlx::query_scalar(
        "SELECT id::text FROM (SELECT gen_random_uuid() AS id FROM generate_series(1, 7)) AS ids \
          ORDER BY id DESC",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let mut conn = pool.acquire().await.unwrap();
    for family in &ids {
        sqlx::query(
            "INSERT INTO accounts.token_families (id, player_id, client_id, access_expires_at) \
             VALUES ($1::uuid, $2::uuid, $3, now() + interval '1 hour')",
        )
        .bind(family)
        .bind(&id)
        .bind(&client)
        .execute(&mut *conn)
        .await
        .unwrap();
    }
    drop(conn);
    let mut listing = pool.begin().await.unwrap();
    // Without sorts or index scans the planner groups by hashing over a sequential scan, so
    // only the query's own ORDER BY can return the ids in order.
    for off in ["enable_sort", "enable_indexscan", "enable_indexonlyscan"] {
        sqlx::query(&format!("SET LOCAL {off} = off")).execute(&mut *listing).await.unwrap();
    }
    let listed = svc.store.revocable_families_tx(&mut listing, &id).await.unwrap();
    listing.rollback().await.unwrap();
    let mut ascending = listed.clone();
    ascending.sort();
    let (gated, others) = ids.split_first().unwrap();
    let mut gate = pool.begin().await.unwrap();
    let gate_pid = backend_pid(&mut gate).await;
    sqlx::query("SELECT 1 FROM accounts.token_families WHERE id = $1::uuid FOR KEY SHARE")
        .bind(gated)
        .execute(&mut *gate)
        .await
        .unwrap();
    let first_task = spawn_deleter(&svc, first, &id, &client);
    let first_pid = waiter_of(&pool, gate_pid).await;
    let unlocked: Option<i64> = match first_pid {
        Some(_) => Some(
            sqlx::query_scalar(
                "SELECT count(*) FROM (SELECT 1 FROM accounts.token_families \
                   WHERE id = ANY($1::uuid[]) FOR UPDATE SKIP LOCKED) AS free",
            )
            .bind(others)
            .fetch_one(&pool)
            .await
            .unwrap(),
        ),
        None => None,
    };
    let second_task = spawn_deleter(&svc, second, &id, &client);
    let queued = match first_pid {
        Some(pid) => waits_on_any(&pool, &[gate_pid, pid]).await,
        None => false,
    };
    gate.rollback().await.unwrap();
    let first_done = first_task.await.unwrap();
    let second_done = second_task.await.unwrap();
    let families: i64 =
        sqlx::query_scalar("SELECT count(*) FROM accounts.token_families WHERE id = ANY($1::uuid[])")
            .bind(&ids)
            .fetch_one(&pool)
            .await
            .unwrap();
    cleanup_player(&pool, &id).await;
    delete_client(&pool, &client).await;

    assert_eq!(listed, ascending, "revoke all takes the families it reads in ascending id");
    assert!(first_pid.is_some(), "the first delete must park on the held family");
    assert_eq!(unlocked, Some(0), "the first delete locked every smaller family before the held one");
    assert!(queued, "the second delete must wait behind the first");
    first_done.expect("the first delete finishes without a deadlock");
    second_done.expect("the second delete finishes without a deadlock");
    assert_eq!(families, 0, "every family is gone");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_delete_and_a_racing_revoke_all_take_two_families_in_one_order() {
    race_families(FamilyDeleter::DeleteClient, FamilyDeleter::RevokeAll).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn revoke_all_and_a_racing_client_delete_take_two_families_in_one_order() {
    race_families(FamilyDeleter::RevokeAll, FamilyDeleter::DeleteClient).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_password_reset_and_a_racing_client_delete_take_two_families_in_one_order() {
    race_families(FamilyDeleter::ResetPassword, FamilyDeleter::DeleteClient).await;
}

/// What the player's family in [`prune_race`] holds, so that exactly one clause of the
/// prune's lock pass reaches it.
#[derive(Clone, Copy)]
enum PruneChild {
    /// A live family holding an expired refresh token.
    Refresh,
    /// A stale family with no rows under it.
    Childless,
}

/// Polls until backend `waiter` waits on a lock held by `holder`, directly or queued behind
/// backends that do (another test's prune parked on the same gate); false when it does not
/// within about twenty seconds.
async fn parked_behind(pool: &PgPool, waiter: i32, holder: i32) -> bool {
    for _ in 0..4000 {
        let parked: bool = sqlx::query_scalar(
            "WITH RECURSIVE blockers(pid) AS ( \
                 SELECT unnest(pg_blocking_pids($1)) \
                 UNION SELECT unnest(pg_blocking_pids(b.pid)) FROM blockers b) \
             SELECT $2::int IN (SELECT pid FROM blockers)",
        )
        .bind(waiter)
        .bind(holder)
        .fetch_one(pool)
        .await
        .unwrap();
        if parked {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    false
}

/// The prune racing a password reset over a family of the player that the prune reaches
/// through `child`. A third transaction holds a stale family of another player with a
/// LARGER id, so the prune, taking families in ascending id, parks there holding the
/// player's family and no child row yet: a probe skipping locked rows sees the family taken
/// and the child still free. The reset then waits behind the prune, and both finish. A
/// prune that deleted the child before it locked the family would hold the child the
/// reset's cascade waits on, then wait on the family the reset holds, and Postgres would
/// abort one of them with a deadlock. The prune is told apart from any other prune parked
/// on the same gate by the pid of the connection it runs on.
async fn prune_race(child: PruneChild) {
    // The prune is global: the three cases hold one gate each, so they run one at a time.
    static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _turn = ONE_AT_A_TIME.lock().await;
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("prunerace-{}@test.local", suffix());
    let player = svc.register(email, "pw".into(), "PruneRace".into(), test_client()).await.unwrap().player_id;
    let email = format!("prunegate-{}@test.local", suffix());
    let other = svc.register(email, "pw".into(), "PruneGate".into(), test_client()).await.unwrap().player_id;
    let (target, gated): (String, String) = sqlx::query_as(
        "SELECT least(a, b)::text, greatest(a, b)::text \
           FROM (SELECT gen_random_uuid() AS a, gen_random_uuid() AS b) AS ids",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let target_access = match child {
        PruneChild::Refresh => "1 hour",
        PruneChild::Childless => "-1 hour",
    };
    for (family, owner, access) in [(&target, &player, target_access), (&gated, &other, "-1 hour")] {
        sqlx::query(
            "INSERT INTO accounts.token_families (id, player_id, client_id, access_expires_at) \
             VALUES ($1::uuid, $2::uuid, $3, now() + $4::interval)",
        )
        .bind(family)
        .bind(owner)
        .bind(TEST_CLIENT_ID)
        .bind(access)
        .execute(&pool)
        .await
        .unwrap();
    }
    let child_row = match child {
        PruneChild::Refresh => Some("accounts.refresh_tokens"),
        PruneChild::Childless => None,
    }
    .map(|table| (table, crate::store::new_token()));
    if let Some((table, token)) = &child_row {
        sqlx::query(&format!(
            "INSERT INTO {table} (token, player_id, family_id, expires_at) \
             VALUES ($1, $2::uuid, $3::uuid, now() - interval '1 hour')"
        ))
        .bind(token)
        .bind(&player)
        .bind(&target)
        .execute(&pool)
        .await
        .unwrap();
    }
    let mut gate = pool.begin().await.unwrap();
    let gate_pid = backend_pid(&mut gate).await;
    sqlx::query("SELECT 1 FROM accounts.token_families WHERE id = $1::uuid FOR KEY SHARE")
        .bind(&gated)
        .execute(&mut *gate)
        .await
        .unwrap();
    let mut prune_tx = pool.begin().await.unwrap();
    let prune_pid = backend_pid(&mut prune_tx).await;
    let prune = {
        let svc = Arc::clone(&svc);
        tokio::spawn(async move {
            svc.store.prune_expired_token_rows(&mut prune_tx).await?;
            prune_tx.commit().await
        })
    };
    let parked = parked_behind(&pool, prune_pid, gate_pid).await;
    let family_free: Option<i64> = if parked {
        Some(
            sqlx::query_scalar(
                "SELECT count(*) FROM (SELECT 1 FROM accounts.token_families \
                   WHERE id = $1::uuid FOR UPDATE SKIP LOCKED) AS free",
            )
            .bind(&target)
            .fetch_one(&pool)
            .await
            .unwrap(),
        )
    } else {
        None
    };
    let child_free: Option<i64> = match (&child_row, parked) {
        (Some((table, token)), true) => Some(
            sqlx::query_scalar(&format!(
                "SELECT count(*) FROM (SELECT 1 FROM {table} \
                   WHERE token = $1 FOR UPDATE SKIP LOCKED) AS free"
            ))
            .bind(token)
            .fetch_one(&pool)
            .await
            .unwrap(),
        ),
        _ => None,
    };
    let mut reset = spawn_deleter(&svc, FamilyDeleter::ResetPassword, &player, TEST_CLIENT_ID);
    // The reset either finishes or parks on the prune; only then is the gate released.
    let reset_early = if parked {
        tokio::select! {
            done = &mut reset => Some(done.unwrap()),
            _ = waits_on(&pool, prune_pid) => None,
        }
    } else {
        None
    };
    gate.rollback().await.unwrap();
    let reset_done = match reset_early {
        Some(done) => done,
        None => reset.await.unwrap(),
    };
    let pruned = prune.await.unwrap();
    let families_left: i64 =
        sqlx::query_scalar("SELECT count(*) FROM accounts.token_families WHERE id IN ($1::uuid, $2::uuid)")
            .bind(&target)
            .bind(&gated)
            .fetch_one(&pool)
            .await
            .unwrap();
    let child_left: i64 = match &child_row {
        Some((table, token)) => sqlx::query_scalar(&format!("SELECT count(*) FROM {table} WHERE token = $1"))
            .bind(token)
            .fetch_one(&pool)
            .await
            .unwrap(),
        None => 0,
    };
    cleanup_player(&pool, &player).await;
    cleanup_player(&pool, &other).await;

    assert!(parked, "the prune must park on the held family");
    assert_eq!(family_free, Some(0), "the prune locked the player's family before the held one");
    if child_row.is_some() {
        assert_eq!(child_free, Some(1), "the prune locked no child row before its families");
    }
    reset_done.expect("the password reset finishes without a deadlock");
    pruned.expect("the prune finishes without a deadlock");
    assert_eq!(families_left, 0, "the player's family and the held stale family are gone");
    assert_eq!(child_left, 0, "the expired child row is gone");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_prune_locks_a_family_with_an_expired_refresh_token_before_a_racing_password_reset() {
    prune_race(PruneChild::Refresh).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_prune_locks_a_stale_family_before_a_racing_password_reset() {
    prune_race(PruneChild::Childless).await;
}
