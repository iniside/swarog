//! Reads back every seeded kind as the seeded players (I5: reads, and the idempotent
//! `markRead`, only). Event-fed reads poll until the deadline (I7); every check that fails is
//! recorded as an error of the op that showed it, never an abort.

use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::client::{Caller, Gateway};
use crate::{expect, ops, SavePlan, Session, MAX_PAGES, PAGE_LIMIT};

pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(250);
// Seeded by `WALLET_DEV_SEED` (`modules/wallet`); without it the catalog is empty.
const DEV_CURRENCIES: &[&str] = &["gold", "gems"];
const KIND_FRIEND_ACCEPTED: &str = "friend.accepted";
const DIRECTION_INCOMING: &str = "incoming";
const STATE_REQUESTED: &str = "requested";

// Local shapes (I4): the fields Verified facts lists for each op. A shape-only read never reads
// its fields back, since deserializing them is the check.
#[derive(Deserialize)]
struct Page<T> {
    items: Vec<T>,
    next_cursor: String,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct HoldingRow {
    item_id: String,
}

#[derive(Deserialize)]
struct CurrencyRow {
    code: String,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct BalanceRow {
    currency: String,
    amount: i64,
}

#[derive(Deserialize)]
struct ScoreRow {
    player: String,
    #[allow(dead_code)]
    wins: i64,
}

#[derive(Deserialize)]
struct FriendRow {
    player_id: String,
    edge_id: String,
    direction: String,
}

#[derive(Deserialize)]
struct MemberRow {
    player_id: String,
    state: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct NotificationRow {
    pub(crate) id: String,
    pub(crate) kind: String,
    pub(crate) created_at: String,
    /// Empty while unread.
    pub(crate) read_at: String,
}

#[derive(Deserialize)]
struct StoredObject {
    owner_id: String,
    key: String,
    acl: String,
}

#[derive(Deserialize)]
struct ObjectSummary {
    key: String,
}

/// The deadline of one poll (I7).
struct Deadline(Instant);

impl Deadline {
    fn after(timeout: Duration) -> Self {
        Deadline(Instant::now() + timeout)
    }

    /// Sleeps one poll interval and answers true, or answers false once the deadline passed.
    async fn wait(&self) -> bool {
        if Instant::now() >= self.0 {
            return false;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
        true
    }
}

/// The unread row `markRead` takes: the oldest by `(created_at, id)`, so the choice does not
/// depend on the order the page lists rows in.
pub(crate) fn first_unread(rows: &[NotificationRow]) -> Option<&NotificationRow> {
    rows.iter().filter(|row| row.read_at.is_empty()).min_by(|a, b| (&a.created_at, &a.id).cmp(&(&b.created_at, &b.id)))
}

/// Every row of a `{items, next_cursor}` list op, `body` carrying the op's other keys. `None`
/// when a page failed (already recorded).
async fn all_pages<T: DeserializeOwned>(
    gateway: &Gateway,
    op: &'static str,
    path_params: &[(&str, &str)],
    caller: Caller<'_>,
    mut body: Value,
    expect: &[u16],
) -> Option<Vec<T>> {
    let mut items = Vec::new();
    let mut cursor = String::new();
    for _ in 0..MAX_PAGES {
        body["cursor"] = json!(cursor);
        body["limit"] = json!(PAGE_LIMIT);
        let (_, page) = gateway.call(op, path_params, caller, Some(&body), expect).await.ok()?;
        let page: Page<T> = gateway.typed(op, page)?;
        items.extend(page.items);
        if page.next_cursor.is_empty() {
            return Some(items);
        }
        cursor = page.next_cursor;
    }
    gateway.shape_error(op, format!("more than {MAX_PAGES} pages"));
    None
}

/// Polls each character's inventory until its starter item shows. Only the `character.created`
/// starter grant writes a character's inventory, and its item is configurable
/// (`inventory/starter_item`), so any holding is the starter item.
pub(crate) async fn starter_items(gateway: &Gateway, session: &Session, characters: &[crate::CharacterRow], timeout: Duration) {
    let caller = Caller::Player(&session.token);
    for character in characters {
        let id = [("id", character.id.as_str())];
        let deadline = Deadline::after(timeout);
        loop {
            let held = gateway
                .call(ops::INVENTORY_LIST_CHARACTER, &id, caller, None, expect::INVENTORY_LIST_CHARACTER)
                .await
                .ok()
                .and_then(|(_, body)| gateway.typed::<Vec<HoldingRow>>(ops::INVENTORY_LIST_CHARACTER, body));
            let Some(held) = held else { break };
            if !held.is_empty() {
                break;
            }
            if !deadline.wait().await {
                gateway.timed_out(
                    ops::INVENTORY_LIST_CHARACTER,
                    format!("the starter item of character {} ({}) after {timeout:?}", character.name, character.id),
                );
                break;
            }
        }
    }
}

/// The currency catalog must hold the dev currencies; the balances are checked for shape only,
/// since a registered player's starter currency depends on the wallet config.
pub(crate) async fn wallet(gateway: &Gateway, session: &Session) {
    let caller = Caller::Player(&session.token);
    let currencies = gateway
        .call(ops::WALLET_LIST_CURRENCIES, &[], caller, None, expect::WALLET_LIST_CURRENCIES)
        .await
        .ok()
        .and_then(|(_, body)| gateway.typed::<Vec<CurrencyRow>>(ops::WALLET_LIST_CURRENCIES, body));
    if let Some(currencies) = currencies {
        let missing: Vec<&str> =
            DEV_CURRENCIES.iter().copied().filter(|code| !currencies.iter().any(|c| c.code == *code)).collect();
        if !missing.is_empty() {
            gateway.shape_error(
                ops::WALLET_LIST_CURRENCIES,
                format!("currencies {missing:?} missing; the fleet runs without WALLET_DEV_SEED"),
            );
        }
    }
    if let Some((_, body)) =
        gateway.call(ops::WALLET_MY_BALANCES, &[], caller, None, expect::WALLET_MY_BALANCES).await.ok()
    {
        gateway.typed::<Vec<BalanceRow>>(ops::WALLET_MY_BALANCES, body);
    }
}

/// Reads the player's own private save back and lists its save collection, which must show
/// both planned keys.
pub(crate) async fn own_saves(gateway: &Gateway, player: &crate::PlayerPlan, session: &Session) {
    let caller = Caller::Player(&session.token);
    let save = &player.save;
    let params = [("collection", save.collection.as_str()), ("key", save.key.as_str())];
    let read = gateway
        .call(ops::STORAGE_GET, &params, caller, Some(&json!({"owner_id": ""})), expect::STORAGE_GET)
        .await
        .ok()
        .and_then(|(_, body)| gateway.typed::<StoredObject>(ops::STORAGE_GET, body));
    if let Some(read) = read {
        if read.owner_id != session.player_id || read.key != save.key {
            gateway.shape_error(
                ops::STORAGE_GET,
                format!("own read of {}/{} answered owner {} key {}", save.collection, save.key, read.owner_id, read.key),
            );
        }
    }
    let body = json!({"collection": save.collection});
    let Some(listed) = all_pages::<ObjectSummary>(gateway, ops::STORAGE_LIST, &[], caller, body, expect::STORAGE_LIST).await
    else {
        return;
    };
    for planned in [&player.save, &player.public_save] {
        if planned.collection == save.collection && !listed.iter().any(|o| o.key == planned.key) {
            gateway.shape_error(ops::STORAGE_LIST, format!("collection {} does not list key {}", save.collection, planned.key));
        }
    }
}

/// `reader` reads `owner`'s public save by `owner_id`.
pub(crate) async fn public_save(gateway: &Gateway, save: &SavePlan, owner: &Session, reader: &Session) {
    let params = [("collection", save.collection.as_str()), ("key", save.key.as_str())];
    let body = json!({"owner_id": owner.player_id});
    let read = gateway
        .call(ops::STORAGE_GET, &params, Caller::Player(&reader.token), Some(&body), expect::STORAGE_GET)
        .await
        .ok()
        .and_then(|(_, body)| gateway.typed::<StoredObject>(ops::STORAGE_GET, body));
    if let Some(read) = read {
        if read.owner_id != owner.player_id || read.acl != save.acl {
            gateway.shape_error(
                ops::STORAGE_GET,
                format!("public read answered owner {} acl {}, want {} {}", read.owner_id, read.acl, owner.player_id, save.acl),
            );
        }
    }
}

/// One `topScores` read; `None` when it failed (recorded).
async fn top_scores(gateway: &Gateway) -> Option<Vec<ScoreRow>> {
    gateway
        .call(ops::LEADERBOARD_TOP_SCORES, &[], Caller::Anonymous, None, expect::LEADERBOARD_TOP_SCORES)
        .await
        .ok()
        .and_then(|(_, body)| gateway.typed::<Vec<ScoreRow>>(ops::LEADERBOARD_TOP_SCORES, body))
}

/// A shape-only `topScores` read, for a run that planned no match and so has no winner to
/// look for.
pub(crate) async fn leaderboard_shape(gateway: &Gateway) {
    top_scores(gateway).await;
}

/// Polls the leaderboard until `player_id` ranks (it is fed by `match.finished`).
pub(crate) async fn leaderboard(gateway: &Gateway, player_id: &str, timeout: Duration) {
    let deadline = Deadline::after(timeout);
    loop {
        let Some(scores) = top_scores(gateway).await else { return };
        if scores.iter().any(|score| score.player == player_id) {
            return;
        }
        if !deadline.wait().await {
            gateway.timed_out(ops::LEADERBOARD_TOP_SCORES, format!("the top planned winner {player_id} after {timeout:?}"));
            return;
        }
    }
}

/// The target's pending list must show the request as incoming. `None` when it does not
/// (recorded) or the list failed.
pub(crate) async fn incoming_request(gateway: &Gateway, target: &Session, edge_id: &str) -> Option<()> {
    let caller = Caller::Player(&target.token);
    let rows = all_pages::<FriendRow>(gateway, ops::FRIENDS_PENDING, &[], caller, json!({}), expect::FRIENDS_PENDING).await?;
    if rows.iter().any(|row| row.edge_id == edge_id && row.direction == DIRECTION_INCOMING) {
        return Some(());
    }
    gateway.shape_error(ops::FRIENDS_PENDING, format!("{} has no incoming request {edge_id}", target.handle));
    None
}

/// `viewer`'s friend list must show `friend`.
pub(crate) async fn friend_listed(gateway: &Gateway, viewer: &Session, friend: &Session) {
    let caller = Caller::Player(&viewer.token);
    let Some(rows) = all_pages::<FriendRow>(gateway, ops::FRIENDS_LIST, &[], caller, json!({}), expect::FRIENDS_LIST).await
    else {
        return;
    };
    if !rows.iter().any(|row| row.player_id == friend.player_id) {
        gateway.shape_error(ops::FRIENDS_LIST, format!("{} does not list friend {}", viewer.handle, friend.handle));
    }
}

/// As the owner: the member list must show `joined` and the pending list `requested` as
/// join requests.
pub(crate) async fn group_rows(gateway: &Gateway, group_id: &str, owner: &Session, joined: &[&Session], requested: &[&Session]) {
    let (caller, id) = (Caller::Player(&owner.token), [("id", group_id)]);
    if let Some(rows) =
        all_pages::<MemberRow>(gateway, ops::GROUPS_MEMBERS, &id, caller, json!({}), expect::GROUPS_MEMBERS).await
    {
        for member in joined {
            if !rows.iter().any(|row| row.player_id == member.player_id) {
                gateway.shape_error(ops::GROUPS_MEMBERS, format!("group {group_id} does not list member {}", member.handle));
            }
        }
    }
    if let Some(rows) =
        all_pages::<MemberRow>(gateway, ops::GROUPS_PENDING, &id, caller, json!({}), expect::GROUPS_PENDING).await
    {
        for asker in requested {
            if !rows.iter().any(|row| row.player_id == asker.player_id && row.state == STATE_REQUESTED) {
                gateway.shape_error(ops::GROUPS_PENDING, format!("group {group_id} does not list the request of {}", asker.handle));
            }
        }
    }
}

/// The `friend.accepted` rows the inbox holds now, read before this run accepts any
/// friendship so `inbox` can tell this run's rows from older ones. `None` when the list
/// failed (recorded).
pub(crate) async fn accepted_rows(gateway: &Gateway, session: &Session) -> Option<usize> {
    let caller = Caller::Player(&session.token);
    let rows =
        all_pages::<NotificationRow>(gateway, ops::NOTIFICATIONS_LIST, &[], caller, json!({}), expect::NOTIFICATIONS_LIST)
            .await?;
    Some(rows.iter().filter(|row| row.kind == KIND_FRIEND_ACCEPTED).count())
}

/// Polls the inbox until it holds `fresh` `friend.accepted` rows over the `baseline` that
/// `accepted_rows` read before the friendships (`fresh` is the friendships this run accepted
/// on the player's request; a row carries nothing naming its run, so only the count over the
/// baseline tells a new row from an old one), then marks the first unread row read. With
/// `fresh` 0 nothing is owed and the first list is enough.
pub(crate) async fn inbox(gateway: &Gateway, session: &Session, baseline: usize, fresh: usize, timeout: Duration) {
    let caller = Caller::Player(&session.token);
    let deadline = Deadline::after(timeout);
    let owed = baseline + fresh;
    let rows = loop {
        let Some(rows) =
            all_pages::<NotificationRow>(gateway, ops::NOTIFICATIONS_LIST, &[], caller, json!({}), expect::NOTIFICATIONS_LIST)
                .await
        else {
            return;
        };
        if fresh == 0 || rows.iter().filter(|row| row.kind == KIND_FRIEND_ACCEPTED).count() >= owed {
            break rows;
        }
        if !deadline.wait().await {
            gateway.timed_out(
                ops::NOTIFICATIONS_LIST,
                format!(
                    "{fresh} new {KIND_FRIEND_ACCEPTED} rows over {baseline} for {} after {timeout:?}",
                    session.handle
                ),
            );
            break rows;
        }
    };
    if let Some(row) = first_unread(&rows) {
        let id = [("id", row.id.as_str())];
        gateway.call(ops::NOTIFICATIONS_MARK_READ, &id, caller, None, expect::NOTIFICATIONS_MARK_READ).await;
    }
}
