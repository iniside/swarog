//! The operator grant and revoke (`item.grant`, `item.grant-tool`, `holding.revoke`) driven
//! through `InventoryAdmin`'s `invoke` and `prepare` against the live Postgres.

use super::*;
use std::collections::BTreeMap;
use controlplane::{FieldKind, InvokeCall, InvokeOutcome, PrepareCall, Then};
use crate::admin::{
    InventoryAdmin, FIELD_CHARACTER, FIELD_ITEM, FIELD_QUANTITY, HOLDING_KIND, HOLDING_REVOKE, ITEM_GRANT,
    ITEM_GRANT_TOOL,
};

const DEAD_DSN: &str = "postgres://gamebackend:gamebackend@127.0.0.1:1/gamebackend?sslmode=disable";

fn admin_over(pool: PgPool) -> InventoryAdmin {
    InventoryAdmin::new(inner_with(pool, Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY))))
}

/// An admin over a store that cannot connect: a call its guards admit answers `Internal`.
fn dead_admin() -> InventoryAdmin {
    let pool = PgPool::connect_lazy(DEAD_DSN).expect("lazy pool");
    InventoryAdmin::new(Arc::new(Inner {
        store: Store { pool },
        dev_grant: false,
        ownership: OnceLock::new(),
        cfg: OnceLock::new(),
    }))
}

fn player_ref(id: &str) -> Option<EntityRef> {
    Some(EntityRef { kind: "player".to_string(), id: id.to_string() })
}

fn holding_ref(id: &str) -> Option<EntityRef> {
    Some(EntityRef { kind: HOLDING_KIND.to_string(), id: id.to_string() })
}

fn call(action: &str, entity: Option<EntityRef>, input: BTreeMap<String, String>, request_id: Option<&str>) -> InvokeCall {
    InvokeCall {
        module: "inventory".to_string(),
        action: action.to_string(),
        entity,
        input,
        expected_revision: None,
        reason: None,
        operator: "tester".to_string(),
        item: None,
        request_id: request_id.map(String::from),
    }
}

fn grant_input(character: &str, item: &str, qty: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        (FIELD_CHARACTER.to_string(), character.to_string()),
        (FIELD_ITEM.to_string(), item.to_string()),
        (FIELD_QUANTITY.to_string(), qty.to_string()),
    ])
}

fn grant_call(player: &str, character: &str, item: &str, qty: &str, request_id: &str) -> InvokeCall {
    call(ITEM_GRANT, player_ref(player), grant_input(character, item, qty), Some(request_id))
}

fn prepare_call(action: &str, entity: Option<EntityRef>) -> PrepareCall {
    PrepareCall { module: "inventory".to_string(), action: action.to_string(), entity, item: None, operator: "tester".to_string() }
}

fn assert_ok(outcome: Result<InvokeOutcome, Error>, what: &str) {
    match outcome {
        Ok(InvokeOutcome::Ok { .. }) => {}
        other => panic!("{what}: expected Ok, got {other:?}"),
    }
}

fn assert_noop(outcome: Result<InvokeOutcome, Error>, what: &str) {
    match outcome {
        Ok(InvokeOutcome::NoOp { .. }) => {}
        other => panic!("{what}: expected NoOp, got {other:?}"),
    }
}

/// The outcome is field errors on exactly `key`.
fn assert_field_error(outcome: Result<InvokeOutcome, Error>, key: &str, what: &str) {
    match outcome {
        Ok(InvokeOutcome::FieldErrors(errors)) => {
            assert_eq!(errors.keys().collect::<Vec<_>>(), vec![key], "{what}: {errors:?}");
        }
        other => panic!("{what}: expected a field error on {key}, got {other:?}"),
    }
}

fn assert_status<T: std::fmt::Debug>(outcome: Result<T, Error>, status: Status, what: &str) {
    match outcome {
        Err(e) => assert_eq!(e.status, status, "{what}: {e:?}"),
        Ok(v) => panic!("{what}: expected {status:?}, got {v:?}"),
    }
}

/// Records `character` as `player`'s the way the `character.created` handler does.
async fn own(pool: &PgPool, character: &str, player: &str) {
    let inner = inner_with(pool.clone(), Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY)));
    let mut tx = pool.begin().await.unwrap();
    inner.record_character_owner(&mut tx, character, player).await.unwrap();
    tx.commit().await.unwrap();
}

async fn character_qty(pool: &PgPool, character: &str, item: &str) -> Option<i64> {
    sqlx::query_scalar(
        "SELECT quantity::bigint FROM inventory.holdings \
          WHERE owner_type = 'character' AND owner_id = $1::uuid AND item_id = $2",
    )
    .bind(character)
    .bind(item)
    .fetch_optional(pool)
    .await
    .unwrap()
}

async fn forget_requests(pool: &PgPool, ids: &[&str]) {
    for id in ids {
        let _ = sqlx::query("DELETE FROM inventory.operator_grants WHERE request_id = $1")
            .bind(*id)
            .execute(pool)
            .await;
    }
}

#[tokio::test]
async fn admin_grant_applies_once_per_request_id() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let admin = admin_over(pool.clone());
    let player = unique_uuid(&pool).await;
    let ch = unique_uuid(&pool).await;
    let (first, second) = (unique_uuid(&pool).await, unique_uuid(&pool).await);
    own(&pool, &ch, &player).await;

    assert_ok(admin.invoke(grant_call(&player, &ch, "coin", "5", &first)).await, "first grant");
    assert_eq!(character_qty(&pool, &ch, "coin").await, Some(5));
    assert_noop(admin.invoke(grant_call(&player, &ch, "coin", "5", &first)).await, "repeat of the same submit");
    assert_eq!(character_qty(&pool, &ch, "coin").await, Some(5), "a repeat must not add to the stack");
    let tool = call(ITEM_GRANT_TOOL, player_ref(&player), grant_input(&ch, "coin", "5"), Some(&second));
    assert_ok(admin.invoke(tool).await, "a new request id through the tab tool");
    assert_eq!(character_qty(&pool, &ch, "coin").await, Some(10), "a new request id adds to the stack");

    cleanup_owner(&pool, &ch).await;
    forget_requests(&pool, &[&first, &second]).await;
}

#[tokio::test]
async fn admin_grant_refusal_gives_back_its_request_id() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let admin = admin_over(pool.clone());
    let player = unique_uuid(&pool).await;
    let other = unique_uuid(&pool).await;
    let mine = unique_uuid(&pool).await;
    let theirs = unique_uuid(&pool).await;
    let rid = unique_uuid(&pool).await;
    own(&pool, &mine, &player).await;
    own(&pool, &theirs, &other).await;

    assert_field_error(
        admin.invoke(grant_call(&player, &theirs, "coin", "1", &rid)).await,
        FIELD_CHARACTER,
        "another player's character",
    );
    assert_eq!(character_qty(&pool, &theirs, "coin").await, None, "a refused grant adds no holding");
    assert_ok(admin.invoke(grant_call(&player, &mine, "coin", "1", &rid)).await, "the corrected resubmit");
    assert_eq!(character_qty(&pool, &mine, "coin").await, Some(1));

    cleanup_owner(&pool, &mine).await;
    cleanup_owner(&pool, &theirs).await;
    forget_requests(&pool, &[&rid]).await;
}

#[tokio::test]
async fn admin_grant_of_an_unknown_item_is_a_field_error() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let admin = admin_over(pool.clone());
    let player = unique_uuid(&pool).await;
    let ch = unique_uuid(&pool).await;
    let rid = unique_uuid(&pool).await;
    own(&pool, &ch, &player).await;

    assert_field_error(
        admin.invoke(grant_call(&player, &ch, "no_such_item", "1", &rid)).await,
        FIELD_ITEM,
        "an item outside the catalog",
    );
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM inventory.holdings WHERE owner_id = $1::uuid")
        .bind(&ch)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(left, 0);

    cleanup_owner(&pool, &ch).await;
    forget_requests(&pool, &[&rid]).await;
}

#[tokio::test]
async fn admin_grant_checks_its_input_before_the_store() {
    let admin = dead_admin();
    let player = "00000000-0000-4000-8000-000000000001";
    let ch = "00000000-0000-4000-8000-000000000002";
    let over = (MAX_HOLDING_QTY + 1).to_string();
    for qty in ["0", "-1", "abc", "", over.as_str()] {
        assert_field_error(admin.invoke(grant_call(player, ch, "coin", qty, "r")).await, FIELD_QUANTITY, qty);
    }
    for character in ["not-a-uuid", "", "00000000-0000-4000-8000-00000000000z"] {
        assert_field_error(
            admin.invoke(grant_call(player, character, "coin", "1", "r")).await,
            FIELD_CHARACTER,
            character,
        );
    }
    assert_field_error(admin.invoke(grant_call(player, ch, "", "1", "r")).await, FIELD_ITEM, "empty item");

    let input = grant_input(ch, "coin", "1");
    for request_id in [None, Some("")] {
        assert_status(
            admin.invoke(call(ITEM_GRANT, player_ref(player), input.clone(), request_id)).await,
            Status::Invalid,
            "no request id",
        );
    }
    assert_status(admin.invoke(grant_call("nope", ch, "coin", "1", "r")).await, Status::Invalid, "malformed player");
    assert_status(
        admin.invoke(call(ITEM_GRANT, None, input.clone(), Some("r"))).await,
        Status::Invalid,
        "no player",
    );
    let mut oversize = input;
    oversize.insert("extra".to_string(), "a".repeat(controlplane::MAX_INPUT_VALUE_BYTES + 1));
    assert_status(
        admin.invoke(call(ITEM_GRANT, player_ref(player), oversize, Some("r"))).await,
        Status::Invalid,
        "oversize input",
    );
}

#[tokio::test]
async fn admin_grant_to_a_deleted_player_is_a_conflict() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    ensure_schema(&pool).await;
    let inner = inner_with(pool.clone(), Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY)));
    let admin = InventoryAdmin::new(inner.clone());
    let player = unique_uuid(&pool).await;
    let ch = unique_uuid(&pool).await;
    let rid = unique_uuid(&pool).await;
    own(&pool, &ch, &player).await;
    let mut conn = pool.acquire().await.unwrap();
    inner.plant_purged_player(&mut conn, &player, "Gone#0001").await.unwrap();
    drop(conn);
    stamp_purge_swept(&pool, &player).await;

    assert_status(
        admin.invoke(grant_call(&player, &ch, "coin", "1", &rid)).await,
        Status::Conflict,
        "a deleted player",
    );
    assert_eq!(character_qty(&pool, &ch, "coin").await, None);

    cleanup_owner(&pool, &ch).await;
    cleanup_purge_row(&pool, &player).await;
    forget_requests(&pool, &[&rid]).await;
}

#[tokio::test]
async fn admin_grant_to_a_wiped_character_is_a_field_error() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let inner = inner_with(pool.clone(), Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY)));
    let admin = InventoryAdmin::new(inner.clone());
    let player = unique_uuid(&pool).await;
    let ch = unique_uuid(&pool).await;
    let rid = unique_uuid(&pool).await;
    own(&pool, &ch, &player).await;
    let mut tx = pool.begin().await.unwrap();
    inner.wipe_character(&mut tx, &ch).await.unwrap();
    tx.commit().await.unwrap();

    assert_field_error(
        admin.invoke(grant_call(&player, &ch, "coin", "1", &rid)).await,
        FIELD_CHARACTER,
        "a wiped character",
    );
    assert_eq!(character_qty(&pool, &ch, "coin").await, None);

    cleanup_owner(&pool, &ch).await;
    forget_requests(&pool, &[&rid]).await;
}

/// The grant takes the character lock the wipe holds before it reads the owner record, so a
/// grant racing an open wipe parks, then sees the wipe and adds nothing.
#[tokio::test(flavor = "multi_thread")]
async fn admin_grant_parks_behind_an_open_wipe_then_refuses_the_character() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let inner = inner_with(pool.clone(), Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY)));
    let player = unique_uuid(&pool).await;
    let ch = unique_uuid(&pool).await;
    let rid = unique_uuid(&pool).await;
    own(&pool, &ch, &player).await;
    let grant_app = racer_app(&pool, "admin-grant").await;

    let mut wipe = pool.begin().await.unwrap();
    inner.wipe_character(&mut wipe, &ch).await.unwrap();
    let racer = admin_over(racer_pool(&grant_app));
    let grant_call = grant_call(&player, &ch, "coin", "1", &rid);
    let grant = tokio::spawn(async move { racer.invoke(grant_call).await });
    let parked = wait_advisory_parked(&pool, &grant_app, lock_key(&ch), &grant).await;
    wipe.commit().await.unwrap();
    let outcome = finish("admin grant", grant).await;

    assert!(parked, "the grant must park on the character lock the open wipe holds");
    assert_field_error(outcome, FIELD_CHARACTER, "a grant that waited out the wipe");
    assert_eq!(character_qty(&pool, &ch, "coin").await, None, "the wiped character must keep no holding");

    cleanup_owner(&pool, &ch).await;
    forget_requests(&pool, &[&rid]).await;
}

#[tokio::test]
async fn admin_revoke_deletes_the_holding_once() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let admin = admin_over(pool.clone());
    let player = unique_uuid(&pool).await;
    let ch = unique_uuid(&pool).await;
    let rid = unique_uuid(&pool).await;
    own(&pool, &ch, &player).await;
    assert_ok(admin.invoke(grant_call(&player, &ch, "coin", "3", &rid)).await, "seed grant");
    let id = format!("character:{ch}:coin");

    assert_ok(admin.invoke(call(HOLDING_REVOKE, holding_ref(&id), BTreeMap::new(), None)).await, "revoke");
    assert_eq!(character_qty(&pool, &ch, "coin").await, None, "the whole row is gone");
    assert_noop(
        admin.invoke(call(HOLDING_REVOKE, holding_ref(&id), BTreeMap::new(), None)).await,
        "revoke of a gone holding",
    );

    cleanup_owner(&pool, &ch).await;
    forget_requests(&pool, &[&rid]).await;
}

#[tokio::test]
async fn admin_revoke_checks_its_holding_before_the_store() {
    let admin = dead_admin();
    let ch = "00000000-0000-4000-8000-000000000002";
    let malformed: [&str; 4] = ["bogus", "character:not-a-uuid:coin", &format!("weapon:{ch}:coin"), &format!("character:{ch}:")];
    for id in malformed {
        assert_status(
            admin.invoke(call(HOLDING_REVOKE, holding_ref(id), BTreeMap::new(), None)).await,
            Status::Invalid,
            id,
        );
        assert_status(admin.prepare(prepare_call(HOLDING_REVOKE, holding_ref(id))).await, Status::Invalid, id);
    }
    assert_status(
        admin.invoke(call(HOLDING_REVOKE, player_ref(ch), BTreeMap::new(), None)).await,
        Status::Invalid,
        "a player ref",
    );
}

#[tokio::test]
async fn admin_prepare_grant_lists_the_players_characters_and_the_catalog() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let admin = admin_over(pool.clone());
    let player = unique_uuid(&pool).await;
    let other = unique_uuid(&pool).await;
    let (a, b, foreign) = (unique_uuid(&pool).await, unique_uuid(&pool).await, unique_uuid(&pool).await);
    own(&pool, &a, &player).await;
    own(&pool, &b, &player).await;
    own(&pool, &foreign, &other).await;

    let Then::Form(form) = admin.prepare(prepare_call(ITEM_GRANT, player_ref(&player))).await.unwrap() else {
        panic!("the grant prepares a form");
    };
    let mut want = vec![a.clone(), b.clone()];
    want.sort();
    assert_eq!(form.fields[0].key, FIELD_CHARACTER);
    assert_eq!(form.fields[0].kind, FieldKind::Enum(want), "exactly the player's characters, in id order");
    assert_eq!(form.fields[1].key, FIELD_ITEM);
    let FieldKind::Enum(items) = &form.fields[1].kind else { panic!("the item field is a choice") };
    for seeded in ["coin", "health_potion", "starter_sword"] {
        assert!(items.iter().any(|i| i == seeded), "{seeded} missing from {items:?}");
    }
    assert!(items.windows(2).all(|w| w[0] < w[1]), "item ids in order: {items:?}");

    let lonely = unique_uuid(&pool).await;
    assert_status(
        admin.prepare(prepare_call(ITEM_GRANT, player_ref(&lonely))).await,
        Status::Invalid,
        "a player with no character",
    );

    for id in [&a, &b, &foreign] {
        cleanup_owner(&pool, id).await;
    }
}

#[tokio::test]
async fn admin_prepare_revoke_names_the_holding() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let admin = admin_over(pool.clone());
    let player = unique_uuid(&pool).await;
    let ch = unique_uuid(&pool).await;
    let rid = unique_uuid(&pool).await;
    own(&pool, &ch, &player).await;
    assert_ok(admin.invoke(grant_call(&player, &ch, "health_potion", "4", &rid)).await, "seed grant");
    let id = format!("character:{ch}:health_potion");

    let Then::Confirm(spec) = admin.prepare(prepare_call(HOLDING_REVOKE, holding_ref(&id))).await.unwrap() else {
        panic!("the revoke prepares a confirm");
    };
    assert_eq!(spec.title, "Revoke Health Potion?");
    let impact: Vec<(&str, &str)> = spec.impact.iter().map(|(k, c)| (k.as_str(), c.value.as_str())).collect();
    assert_eq!(&impact[..2], &[("Item", "health_potion"), ("Quantity", "4")]);
    assert_eq!(impact[2].0, "Owner");

    cleanup_owner(&pool, &ch).await;
    assert_status(
        admin.prepare(prepare_call(HOLDING_REVOKE, holding_ref(&id))).await,
        Status::NotFound,
        "a gone holding",
    );
    forget_requests(&pool, &[&rid]).await;
}
