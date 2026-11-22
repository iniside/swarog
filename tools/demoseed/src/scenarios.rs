//! Throwaway client scenarios: each one plays a short client life over the public ops the seeded
//! phases never call, or call only when a row is missing, with players it creates itself and
//! deletes at its end (I5). So every catalog op answers its declared success on every run, a
//! re-run included (I3). Steps inside one scenario run in order; a lost step is recorded and the
//! steps that need its result are skipped with that cause (I2), never the whole run.

use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use crate::client::{Caller, Gateway};
use crate::readback::{self, Deadline, NotificationRow, KIND_FRIEND_ACCEPTED};
use crate::{expect, ops, or_skip, CharacterRow, FriendBody, GroupRow, MeBody, Session, SessionBody, PASSWORD};

const GUEST: &str = "guest";
const KIND_FRIEND_REQUESTED: &str = "friend.requested";
const STATE_PENDING: &str = "pending";
const STATE_MEMBER: &str = "member";
const STATE_REQUESTED: &str = "requested";
const PARTY_READY: &str = "ready";
// Storage is per player and every scenario's A is a fresh guest, so fixed names never clash.
const SAVE_COLLECTION: &str = "sim";
const SAVE_KEY: &str = "slot";
const STARTER_GRANT_ITEM: &str = "coin";

/// One scenario: `nonce` names this run, `index` this scenario inside it.
pub(crate) struct Client {
    pub(crate) nonce: String,
    pub(crate) index: usize,
    pub(crate) timeout: Duration,
}

impl Client {
    pub(crate) fn name(&self) -> String {
        format!("client {}", self.index)
    }

    /// Unique per run and scenario, so a re-run registers a fresh account.
    fn email(&self) -> String {
        format!("sim-{}-{}@demo.local", self.nonce, self.index)
    }

    /// A group name, a report id: unique per run and scenario, at most 64 bytes.
    fn tag(&self, what: &str) -> String {
        format!("sim-{}-{}-{what}", self.nonce, self.index)
    }
}

// Local shapes (I4): the fields Verified facts lists for each op.
#[derive(Deserialize)]
struct GuestBody {
    player_id: String,
    token: String,
    // Shape only: A rotates the refresh token of its federated sign-in instead.
    #[allow(dead_code)]
    refresh_token: String,
    device_secret: String,
}

#[derive(Deserialize)]
struct RefreshableBody {
    player_id: String,
    token: String,
    refresh_token: String,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct TicketBody {
    ticket: String,
    expires_at: String,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct ReceiptBody {
    player_id: String,
    handle: String,
    deleted_at: String,
}

#[derive(Deserialize)]
struct StoredBody {
    owner_id: String,
    key: String,
}

#[derive(Deserialize)]
struct JoinBody {
    state: String,
}

#[derive(Deserialize)]
struct PartyBody {
    party_id: String,
    leader_id: String,
    members: Vec<PartyMember>,
}

#[derive(Deserialize)]
struct PartyMember {
    player_id: String,
    ready: String,
}

#[derive(Deserialize)]
struct InviteBody {
    invite_id: String,
}

#[derive(Deserialize)]
struct InviteListBody {
    invites: Vec<InviteBody>,
}

#[derive(Deserialize)]
struct HeartbeatBody {
    lease_until: String,
}

/// An account this scenario created and holds a bearer for: deleted at its end.
pub(crate) struct Held {
    pub(crate) label: &'static str,
    pub(crate) player_id: String,
    pub(crate) token: String,
}

/// Runs one scenario: sessions (guests A, B, C and a registered D), then characters and
/// storage as A, friends, groups, a match and a party as A, B and C, and last the deletion of
/// every account it created.
pub(crate) async fn run_client(gateway: &Gateway, client: &Client) {
    let (mut held, [a, b, c]) = guests(gateway).await;
    held.extend(registered(gateway, client).await);
    match &a {
        Some(a) => {
            characters(gateway, client, a).await;
            storage(gateway, a).await;
        }
        None => gateway.skip("characters and storage skipped: guest A has no session"),
    }
    match (&a, &b, &c) {
        (Some(a), Some(b), Some(c)) => {
            friends(gateway, client, a, b, c).await;
            groups(gateway, client, a, b, c).await;
            report_match(gateway, client, a, b).await;
            party(gateway, a, b, c).await;
        }
        _ => gateway.skip("friends, groups, match and party skipped: guests A, B and C do not all have a session"),
    }
    for account in &held {
        delete_account(gateway, account).await;
    }
}

/// Creates guests A, B and C; A then signs in again with its device secret, rotates its refresh
/// token once and links its own guest identity again; last `me` reads each guest's handle.
/// Returns every account to delete (with its newest bearer) and, per guest, its session.
async fn guests(gateway: &Gateway) -> (Vec<Held>, [Option<Session>; 3]) {
    const LABELS: [&str; 3] = ["A", "B", "C"];
    let mut made = Vec::with_capacity(LABELS.len());
    for label in LABELS {
        let guest = gateway
            .call(ops::ACCOUNTS_CREATE_GUEST, &[], Caller::Anonymous, None, expect::ACCOUNTS_CREATE_GUEST)
            .await
            .ok()
            .and_then(|(_, body)| gateway.typed::<GuestBody>(ops::ACCOUNTS_CREATE_GUEST, body));
        made.push(or_skip(gateway, guest, &format!("guest {label} skipped: its creation failed")));
    }
    let a_token = match &made[0] {
        Some(a) => Some(sign_in_again(gateway, a).await),
        None => None,
    };
    let mut held = Vec::new();
    let mut sessions: [Option<Session>; 3] = [None, None, None];
    for (slot, guest) in made.into_iter().enumerate() {
        let Some(guest) = guest else { continue };
        let token = match (slot, &a_token) {
            (0, Some(token)) => token.clone(),
            _ => guest.token,
        };
        sessions[slot] = me(gateway, &token)
            .await
            .map(|handle| Session { player_id: guest.player_id.clone(), token: token.clone(), handle });
        held.push(Held { label: LABELS[slot], player_id: guest.player_id, token });
    }
    (held, sessions)
}

/// The guest's federated sign-in with its device secret, one refresh of that sign-in's refresh
/// token (never replayed: a replay outside the grace window revokes the family), then a link of
/// its own guest identity. Returns the newest bearer.
async fn sign_in_again(gateway: &Gateway, guest: &GuestBody) -> String {
    let credential = json!({"provider": GUEST, "credential": guest.device_secret});
    let signed_in = gateway
        .call(ops::ACCOUNTS_LOGIN_FEDERATED, &[], Caller::Anonymous, Some(&credential), expect::ACCOUNTS_LOGIN_FEDERATED)
        .await
        .ok()
        .and_then(|(_, body)| gateway.typed::<RefreshableBody>(ops::ACCOUNTS_LOGIN_FEDERATED, body));
    let mut token = guest.token.clone();
    match signed_in {
        Some(session) => {
            same_player(gateway, ops::ACCOUNTS_LOGIN_FEDERATED, &guest.player_id, &session.player_id);
            token = session.token;
            let body = json!({"refresh_token": session.refresh_token});
            let refreshed = gateway
                .call(ops::ACCOUNTS_REFRESH, &[], Caller::Anonymous, Some(&body), expect::ACCOUNTS_REFRESH)
                .await
                .ok()
                .and_then(|(_, body)| gateway.typed::<RefreshableBody>(ops::ACCOUNTS_REFRESH, body));
            if let Some(refreshed) = refreshed {
                same_player(gateway, ops::ACCOUNTS_REFRESH, &guest.player_id, &refreshed.player_id);
                token = refreshed.token;
            }
        }
        None => gateway.skip("refresh skipped: the guest sign-in failed"),
    }
    if let Some((_, body)) = gateway
        .call(ops::ACCOUNTS_LINK, &[], Caller::Player(&token), Some(&credential), expect::ACCOUNTS_LINK)
        .await
        .ok()
    {
        gateway.typed::<MeBody>(ops::ACCOUNTS_LINK, body);
    }
    token
}

fn same_player(gateway: &Gateway, op: &'static str, want: &str, got: &str) {
    if want != got {
        gateway.shape_error(op, format!("answered player {got}, want the guest {want}"));
    }
}

/// The caller's handle, `None` when `me` failed (recorded).
async fn me(gateway: &Gateway, token: &str) -> Option<String> {
    let (_, body) =
        gateway.call(ops::ACCOUNTS_ME, &[], Caller::Player(token), None, expect::ACCOUNTS_ME).await.ok()?;
    gateway.typed::<MeBody>(ops::ACCOUNTS_ME, body).map(|me| me.handle)
}

/// Registers account D with a fresh email, logs in and reads `me`. `None` when the
/// registration failed, since nothing then holds a bearer for D.
async fn registered(gateway: &Gateway, client: &Client) -> Option<Held> {
    let email = client.email();
    let register = json!({"email": email, "password": PASSWORD, "displayName": format!("Sim {}", client.index)});
    let session = gateway
        .call(ops::ACCOUNTS_REGISTER, &[], Caller::Anonymous, Some(&register), expect::ACCOUNTS_REGISTER_FRESH)
        .await
        .ok()
        .and_then(|(_, body)| gateway.typed::<SessionBody>(ops::ACCOUNTS_REGISTER, body));
    let session = or_skip(gateway, session, "login, profile read and deletion of D skipped: the registration failed")?;
    let mut held = Held { label: "D", player_id: session.player_id, token: session.token };
    let login = json!({"email": email, "password": PASSWORD});
    let logged_in = gateway
        .call(ops::ACCOUNTS_LOGIN, &[], Caller::Anonymous, Some(&login), expect::ACCOUNTS_LOGIN)
        .await
        .ok()
        .and_then(|(_, body)| gateway.typed::<SessionBody>(ops::ACCOUNTS_LOGIN, body));
    if let Some(logged_in) = logged_in {
        same_player(gateway, ops::ACCOUNTS_LOGIN, &held.player_id, &logged_in.player_id);
        held.token = logged_in.token;
    }
    me(gateway, &held.token).await;
    Some(held)
}

/// A grant, a character whose starter item is polled for, its delete, and a list without it.
async fn characters(gateway: &Gateway, client: &Client, a: &Session) {
    let caller = Caller::Player(&a.token);
    let grant = json!({"item_id": STARTER_GRANT_ITEM, "qty": 1});
    gateway.call(ops::INVENTORY_GRANT, &[], caller, Some(&grant), expect::INVENTORY_GRANT).await;
    let body = json!({"name": format!("Sim {}", client.index), "class": "warrior"});
    let created = gateway
        .call(ops::CHARACTERS_CREATE, &[], caller, Some(&body), expect::CHARACTERS_CREATE)
        .await
        .ok()
        .and_then(|(_, body)| gateway.typed::<CharacterRow>(ops::CHARACTERS_CREATE, body));
    let Some(character) =
        or_skip(gateway, created, "starter item read and character delete skipped: the character create failed")
    else {
        return;
    };
    readback::starter_items(gateway, a, std::slice::from_ref(&character), client.timeout).await;
    let id = [("id", character.id.as_str())];
    let deleted = gateway.call(ops::CHARACTERS_DELETE, &id, caller, None, expect::CHARACTERS_DELETE).await.ok();
    if or_skip(gateway, deleted, "character list after the delete skipped: the delete failed").is_none() {
        return;
    }
    let listed = gateway
        .call(ops::CHARACTERS_LIST, &[], caller, None, expect::CHARACTERS_LIST)
        .await
        .ok()
        .and_then(|(_, body)| gateway.typed::<Vec<CharacterRow>>(ops::CHARACTERS_LIST, body));
    if listed.is_some_and(|rows| rows.iter().any(|row| row.id == character.id)) {
        gateway.shape_error(ops::CHARACTERS_LIST, format!("deleted character {} is still listed", character.id));
    }
}

/// A put, a read of it, its delete, and a read that must answer 404.
async fn storage(gateway: &Gateway, a: &Session) {
    let caller = Caller::Player(&a.token);
    let params = [("collection", SAVE_COLLECTION), ("key", SAVE_KEY)];
    let put = json!({"value": json!({"level": 1}).to_string(), "expected_version": 0, "acl": "private"});
    let stored = gateway.call(ops::STORAGE_PUT, &params, caller, Some(&put), expect::STORAGE_PUT_FRESH).await.ok();
    if or_skip(gateway, stored, "storage read and delete skipped: the put failed").is_none() {
        return;
    }
    let own = json!({"owner_id": ""});
    let read = gateway
        .call(ops::STORAGE_GET, &params, caller, Some(&own), expect::STORAGE_GET)
        .await
        .ok()
        .and_then(|(_, body)| gateway.typed::<StoredBody>(ops::STORAGE_GET, body));
    if let Some(read) = read {
        if read.owner_id != a.player_id || read.key != SAVE_KEY {
            gateway.shape_error(ops::STORAGE_GET, format!("own read answered owner {} key {}", read.owner_id, read.key));
        }
    }
    let deleted = gateway.call(ops::STORAGE_DELETE, &params, caller, None, expect::STORAGE_DELETE).await.ok();
    if or_skip(gateway, deleted, "storage read after the delete skipped: the delete failed").is_some() {
        gateway.call(ops::STORAGE_GET, &params, caller, Some(&own), expect::STORAGE_GET_DELETED).await;
    }
}

/// A's request B declines; A's request C accepts, A's inbox shows `friend.accepted` and A
/// unfriends C; B's inbox shows A's `friend.requested`, which B marks read and deletes.
async fn friends(gateway: &Gateway, client: &Client, a: &Session, b: &Session, c: &Session) {
    let to_b = request(gateway, a, b).await;
    match &to_b {
        Some(edge) => {
            let id = [("id", edge.as_str())];
            gateway.call(ops::FRIENDS_DECLINE, &id, Caller::Player(&b.token), None, expect::FRIENDS_DECLINE).await;
        }
        None => gateway.skip("decline skipped: the request to B failed"),
    }
    match request(gateway, a, c).await {
        Some(edge) => {
            let id = [("id", edge.as_str())];
            let accepted =
                gateway.call(ops::FRIENDS_ACCEPT, &id, Caller::Player(&c.token), None, expect::FRIENDS_ACCEPT).await.ok();
            if or_skip(gateway, accepted, "accepted-row read and unfriend skipped: the accept failed").is_some() {
                inbox_row(gateway, a, KIND_FRIEND_ACCEPTED, client.timeout).await;
                gateway.call(ops::FRIENDS_REMOVE, &id, Caller::Player(&a.token), None, expect::FRIENDS_REMOVE).await;
            }
        }
        None => gateway.skip("accept and unfriend skipped: the request to C failed"),
    }
    if to_b.is_none() {
        gateway.skip("B's requested-row read, mark and delete skipped: the request to B failed");
        return;
    }
    let Some(row) = inbox_row(gateway, b, KIND_FRIEND_REQUESTED, client.timeout).await else {
        gateway.skip("mark read and delete skipped: B's inbox does not show the request");
        return;
    };
    let id = [("id", row.id.as_str())];
    let caller = Caller::Player(&b.token);
    gateway.call(ops::NOTIFICATIONS_MARK_READ, &id, caller, None, expect::NOTIFICATIONS_MARK_READ).await;
    gateway.call(ops::NOTIFICATIONS_DELETE, &id, caller, None, expect::NOTIFICATIONS_DELETE).await;
}

/// `from` requests `to`; the edge id of the pending request, `None` when it failed (recorded).
async fn request(gateway: &Gateway, from: &Session, to: &Session) -> Option<String> {
    let body = json!({"target_handle": to.handle});
    let (_, body) = gateway
        .call(ops::FRIENDS_REQUEST, &[], Caller::Player(&from.token), Some(&body), expect::FRIENDS_REQUEST)
        .await
        .ok()?;
    let friend: FriendBody = gateway.typed(ops::FRIENDS_REQUEST, body)?;
    if friend.state != STATE_PENDING {
        gateway.shape_error(ops::FRIENDS_REQUEST, format!("a first request between fresh guests answered state {}", friend.state));
        return None;
    }
    Some(friend.edge_id)
}

/// Polls the inbox until a row of `kind` shows (it is fed by events, I7). The scenario's
/// players are fresh, so any such row is this scenario's.
async fn inbox_row(gateway: &Gateway, session: &Session, kind: &str, timeout: Duration) -> Option<NotificationRow> {
    let caller = Caller::Player(&session.token);
    let deadline = Deadline::after(timeout);
    loop {
        let rows = readback::all_pages::<NotificationRow>(
            gateway,
            ops::NOTIFICATIONS_LIST,
            &[],
            caller,
            json!({}),
            expect::NOTIFICATIONS_LIST,
        )
        .await?;
        if let Some(row) = rows.into_iter().find(|row| row.kind == kind) {
            return Some(row);
        }
        if !deadline.wait().await {
            gateway.timed_out(ops::NOTIFICATIONS_LIST, format!("a {kind} row for {} after {timeout:?}", session.handle));
            return None;
        }
    }
}

/// One group per join policy, each emptied again: an open one B joins, an invite one C
/// accepts, a request one A approves B into. Members leave before the owner, whose leave as
/// the sole member deletes the group.
async fn groups(gateway: &Gateway, client: &Client, a: &Session, b: &Session, c: &Session) {
    if let Some(id) = create_group(gateway, client, a, "open").await {
        let holders: Vec<&Session> = join(gateway, &id, b, STATE_MEMBER).await.into_iter().map(|_| b).collect();
        leave_all(gateway, &id, a, &holders).await;
    }
    if let Some(id) = create_group(gateway, client, a, "invite").await {
        let path = [("id", id.as_str())];
        let invite = json!({"target_handle": c.handle});
        let invited = gateway
            .call(ops::GROUPS_INVITE, &path, Caller::Player(&a.token), Some(&invite), expect::GROUPS_INVITE_FRESH)
            .await
            .ok();
        let mut holders = Vec::new();
        match invited {
            Some(_) => {
                let accept = json!({"decision": "accept"});
                gateway
                    .call(ops::GROUPS_RESPOND, &path, Caller::Player(&c.token), Some(&accept), expect::GROUPS_RESPOND_FRESH)
                    .await;
                // Accepted or still invited, C holds a row that its leave drops.
                holders.push(c);
            }
            None => gateway.skip("invite response skipped: the group invite failed"),
        }
        leave_all(gateway, &id, a, &holders).await;
    }
    if let Some(id) = create_group(gateway, client, a, "request").await {
        let path = [("id", id.as_str())];
        let mut holders = Vec::new();
        match join(gateway, &id, b, STATE_REQUESTED).await {
            Some(()) => {
                let decide = json!({"subject_id": b.player_id, "decision": "accept"});
                gateway
                    .call(ops::GROUPS_DECIDE, &path, Caller::Player(&a.token), Some(&decide), expect::GROUPS_DECIDE_FRESH)
                    .await;
                holders.push(b);
            }
            None => gateway.skip("join decision skipped: the join request failed"),
        }
        leave_all(gateway, &id, a, &holders).await;
    }
}

async fn create_group(gateway: &Gateway, client: &Client, owner: &Session, policy: &str) -> Option<String> {
    let body = json!({"name": client.tag(policy), "join_policy": policy});
    let created = gateway
        .call(ops::GROUPS_CREATE, &[], Caller::Player(&owner.token), Some(&body), expect::GROUPS_CREATE)
        .await
        .ok()
        .and_then(|(_, body)| gateway.typed::<GroupRow>(ops::GROUPS_CREATE, body));
    or_skip(gateway, created, &format!("the {policy} group flow skipped: the group was not created")).map(|row| row.id)
}

/// `member` joins; `Some` when it now holds a row, which must be in `state`.
async fn join(gateway: &Gateway, group_id: &str, member: &Session, state: &str) -> Option<()> {
    let (_, body) = gateway
        .call(ops::GROUPS_JOIN, &[("id", group_id)], Caller::Player(&member.token), None, expect::GROUPS_JOIN_FRESH)
        .await
        .ok()?;
    if let Some(joined) = gateway.typed::<JoinBody>(ops::GROUPS_JOIN, body) {
        if joined.state != state {
            gateway.shape_error(ops::GROUPS_JOIN, format!("join answered state {}, want {state}", joined.state));
        }
    }
    Some(())
}

/// Every holder leaves, then the owner; the owner's leave is skipped while a holder's leave
/// failed, since the last admin of a group with other members gets 409.
async fn leave_all(gateway: &Gateway, group_id: &str, owner: &Session, holders: &[&Session]) {
    let path = [("id", group_id)];
    let mut all_left = true;
    for holder in holders {
        let left =
            gateway.call(ops::GROUPS_LEAVE, &path, Caller::Player(&holder.token), None, expect::GROUPS_LEAVE).await.ok();
        all_left &= left.is_some();
    }
    if !all_left {
        gateway.skip("owner leave skipped: a member's leave failed");
        return;
    }
    gateway.call(ops::GROUPS_LEAVE, &path, Caller::Player(&owner.token), None, expect::GROUPS_LEAVE).await;
}

/// A match A wins over B, under this scenario's own `ReportId`.
async fn report_match(gateway: &Gateway, client: &Client, a: &Session, b: &Session) {
    let body = json!({"ReportId": client.tag("match"), "Winner": a.player_id, "Loser": b.player_id});
    gateway.call(ops::MATCH_REPORT, &[], Caller::Server, Some(&body), expect::MATCH_REPORT_FRESH).await;
}

/// A creates a party and invites B and C; B sees and accepts its invite, C declines; A renews
/// its lease, B readies, A hands B the lead, B kicks A, reads the party and leaves it.
async fn party(gateway: &Gateway, a: &Session, b: &Session, c: &Session) {
    let created = gateway
        .call(ops::PARTY_CREATE, &[], Caller::Player(&a.token), None, expect::PARTY_CREATE)
        .await
        .ok()
        .and_then(|(_, body)| gateway.typed::<PartyBody>(ops::PARTY_CREATE, body));
    let Some(created) = or_skip(gateway, created, "the party flow skipped: the party create failed") else {
        return;
    };
    if created.leader_id != a.player_id {
        gateway.shape_error(ops::PARTY_CREATE, format!("new party led by {}, want the creator {}", created.leader_id, a.player_id));
    }
    let to_b = party_invite(gateway, a, b).await;
    let to_c = party_invite(gateway, a, c).await;
    let joined = match to_b {
        Some(invite) => party_accept(gateway, b, &invite, &created.party_id).await,
        None => {
            gateway.skip("B's accept skipped: the party invite to B failed");
            false
        }
    };
    match to_c {
        Some(invite) => {
            let body = json!({"invite_id": invite});
            gateway.call(ops::PARTY_DECLINE, &[], Caller::Player(&c.token), Some(&body), expect::PARTY_DECLINE).await;
        }
        None => gateway.skip("C's decline skipped: the party invite to C failed"),
    }
    let renewed = gateway
        .call(ops::PARTY_HEARTBEAT, &[], Caller::Player(&a.token), None, expect::PARTY_HEARTBEAT)
        .await
        .ok()
        .and_then(|(_, body)| gateway.typed::<HeartbeatBody>(ops::PARTY_HEARTBEAT, body));
    if renewed.is_some_and(|beat| beat.lease_until.is_empty()) {
        gateway.shape_error(ops::PARTY_HEARTBEAT, "the leader's heartbeat answered no lease".to_string());
    }
    if !joined {
        gateway.skip("ready, promote, kick, read and leave skipped: B is not in the party");
        return;
    }
    let ready = json!({"state": PARTY_READY});
    if let Some(view) = party_call(gateway, ops::PARTY_SET_READY, b, Some(&ready), expect::PARTY_SET_READY).await {
        if !view.members.iter().any(|m| m.player_id == b.player_id && m.ready == PARTY_READY) {
            gateway.shape_error(ops::PARTY_SET_READY, format!("{} is not listed ready", b.handle));
        }
    }
    let to_b = json!({"player_id": b.player_id});
    let promoted = party_call(gateway, ops::PARTY_PROMOTE, a, Some(&to_b), expect::PARTY_PROMOTE).await;
    let led_by_b = match promoted {
        Some(view) if view.leader_id == b.player_id => true,
        Some(view) => {
            gateway.shape_error(ops::PARTY_PROMOTE, format!("party led by {} after promoting {}", view.leader_id, b.player_id));
            false
        }
        None => false,
    };
    let mut kicked = false;
    if led_by_b {
        let to_a = json!({"player_id": a.player_id});
        if let Some(view) = party_call(gateway, ops::PARTY_KICK, b, Some(&to_a), expect::PARTY_KICK).await {
            kicked = true;
            if view.members.iter().any(|m| m.player_id == a.player_id) {
                gateway.shape_error(ops::PARTY_KICK, format!("{} is still listed after the kick", a.handle));
            }
        }
    } else {
        gateway.skip("kick skipped: B does not lead the party");
    }
    if let Some(view) = party_call(gateway, ops::PARTY_GET, b, None, expect::PARTY_GET).await {
        let mut want = if kicked { vec![b.player_id.as_str()] } else { vec![a.player_id.as_str(), b.player_id.as_str()] };
        want.sort_unstable();
        let mut listed: Vec<&str> = view.members.iter().map(|m| m.player_id.as_str()).collect();
        listed.sort_unstable();
        if view.party_id != created.party_id || listed != want {
            gateway.shape_error(ops::PARTY_GET, format!("B reads party {} with members {listed:?}", view.party_id));
        }
    }
    if let Some(view) = party_call(gateway, ops::PARTY_LEAVE, b, None, expect::PARTY_LEAVE).await {
        if !view.party_id.is_empty() {
            gateway.shape_error(ops::PARTY_LEAVE, format!("B still reads party {} after leaving", view.party_id));
        }
    }
}

/// A party op answering a `PartyView`; `None` when it failed (recorded).
async fn party_call(
    gateway: &Gateway,
    op: &'static str,
    caller: &Session,
    body: Option<&serde_json::Value>,
    expect: &[u16],
) -> Option<PartyBody> {
    let (_, view) = gateway.call(op, &[], Caller::Player(&caller.token), body, expect).await.ok()?;
    gateway.typed(op, view)
}

/// The leader invites `to`; the invite id, `None` when it failed (recorded).
async fn party_invite(gateway: &Gateway, leader: &Session, to: &Session) -> Option<String> {
    let body = json!({"target_handle": to.handle});
    let (_, invite) = gateway
        .call(ops::PARTY_INVITE, &[], Caller::Player(&leader.token), Some(&body), expect::PARTY_INVITE)
        .await
        .ok()?;
    gateway.typed::<InviteBody>(ops::PARTY_INVITE, invite).map(|invite| invite.invite_id)
}

/// The invitee's invite list must show `invite`; it then accepts it. True once it is in
/// `party_id`.
async fn party_accept(gateway: &Gateway, invitee: &Session, invite: &str, party_id: &str) -> bool {
    let caller = Caller::Player(&invitee.token);
    let listed = gateway
        .call(ops::PARTY_INVITES, &[], caller, None, expect::PARTY_INVITES)
        .await
        .ok()
        .and_then(|(_, body)| gateway.typed::<InviteListBody>(ops::PARTY_INVITES, body));
    if listed.is_some_and(|list| !list.invites.iter().any(|row| row.invite_id == invite)) {
        gateway.shape_error(ops::PARTY_INVITES, format!("{} does not list invite {invite}", invitee.handle));
    }
    let body = json!({"invite_id": invite});
    let Some(view) = party_call(gateway, ops::PARTY_ACCEPT, invitee, Some(&body), expect::PARTY_ACCEPT).await else {
        return false;
    };
    if view.party_id != party_id {
        gateway.shape_error(ops::PARTY_ACCEPT, format!("accept joined party {}, want {party_id}", view.party_id));
    }
    true
}

/// `beginDelete`, `deleteAccount` with its ticket, then `me` on the dead bearer, which must
/// answer 401.
pub(crate) async fn delete_account(gateway: &Gateway, account: &Held) {
    let caller = Caller::Player(&account.token);
    let label = account.label;
    let ticket = gateway
        .call(ops::ACCOUNTS_BEGIN_DELETE, &[], caller, None, expect::ACCOUNTS_BEGIN_DELETE)
        .await
        .ok()
        .and_then(|(_, body)| gateway.typed::<TicketBody>(ops::ACCOUNTS_BEGIN_DELETE, body));
    let Some(ticket) = or_skip(gateway, ticket, &format!("deletion of {label} skipped: beginDelete failed")) else {
        return;
    };
    let body = json!({"ticket": ticket.ticket});
    let receipt = gateway
        .call(ops::ACCOUNTS_DELETE_ACCOUNT, &[], caller, Some(&body), expect::ACCOUNTS_DELETE_ACCOUNT)
        .await
        .ok()
        .and_then(|(_, body)| gateway.typed::<ReceiptBody>(ops::ACCOUNTS_DELETE_ACCOUNT, body));
    let Some(receipt) = or_skip(gateway, receipt, &format!("profile read after the deletion of {label} skipped: the delete failed"))
    else {
        return;
    };
    if receipt.player_id != account.player_id {
        gateway.shape_error(
            ops::ACCOUNTS_DELETE_ACCOUNT,
            format!("deleting {label} answered player {}, want {}", receipt.player_id, account.player_id),
        );
    }
    gateway.call(ops::ACCOUNTS_ME, &[], caller, None, expect::ACCOUNTS_ME_DELETED).await;
}

#[cfg(test)]
#[path = "scenarios_tests.rs"]
mod tests;
