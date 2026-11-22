//! The expect list of every call this tool makes, one `const` per op and use, named after the op
//! (a suffix such as `_FRESH` or `_DELETED` names a second use of the same op).
//! `ALL` pairs each with its op name; a unit test runs `client::check_expect` over `ALL` and
//! checks that `ALL` holds every `const` of this file, so a 2xx that is not the op's declared
//! success fails without a live server.

use crate::ops;

// The throwaway client scenarios (`scenarios.rs`) act only on players and rows they create in
// the same scenario, so their lists hold the declared success alone: a `*_FRESH` list is the
// strict twin of a seed list that accepts a convergent 409, and a 409 there is an error.
pub const ACCOUNTS_BEGIN_DELETE: &[u16] = &[200];
pub const ACCOUNTS_CREATE_GUEST: &[u16] = &[201];
pub const ACCOUNTS_DELETE_ACCOUNT: &[u16] = &[200];
// Re-linking the caller's own guest identity is an idempotent success (`AlreadyLinked`).
pub const ACCOUNTS_LINK: &[u16] = &[200];
pub const ACCOUNTS_LOGIN: &[u16] = &[200];
pub const ACCOUNTS_LOGIN_FEDERATED: &[u16] = &[200];
pub const ACCOUNTS_ME: &[u16] = &[200];
// A deleted player's bearer: the session died with the player, so the gateway answers 401.
pub const ACCOUNTS_ME_DELETED: &[u16] = &[401];
// Used once per login, never replayed: a replay outside the grace window revokes the family.
pub const ACCOUNTS_REFRESH: &[u16] = &[200];
// A re-run finds the seeded account: 409 "already registered" is the convergent answer.
pub const ACCOUNTS_REGISTER: &[u16] = &[201, 409];
pub const ACCOUNTS_REGISTER_FRESH: &[u16] = &[201];
// A create or grant runs only after the list showed the row missing or the quantity short, so
// a 409 there is a cap, ceiling or tombstone refusal, not "already exists", and is an error.
pub const CHARACTERS_CREATE: &[u16] = &[201];
pub const CHARACTERS_DELETE: &[u16] = &[204];
pub const CHARACTERS_LIST: &[u16] = &[200];
// Called only on an edge the request answered pending, so a re-run never accepts again; an edge
// no longer pending answers 404 (`modules/friends/src/service.rs` `accept`), never 409.
pub const FRIENDS_ACCEPT: &[u16] = &[204];
pub const FRIENDS_DECLINE: &[u16] = &[204];
pub const FRIENDS_LIST: &[u16] = &[200];
pub const FRIENDS_PENDING: &[u16] = &[200];
pub const FRIENDS_REMOVE: &[u16] = &[204];
// A repeat request on an existing edge answers 201 with that edge (`modules/friends/src/service.rs`
// `request_tx`, `find_pair_tx` branch), so no 409 is convergent here.
pub const FRIENDS_REQUEST: &[u16] = &[201];
pub const GROUPS_CREATE: &[u16] = &[201];
// 409 on a re-run: the subject is already a member (`modules/groups/src/service.rs` `decide`).
pub const GROUPS_DECIDE: &[u16] = &[204, 409];
pub const GROUPS_DECIDE_FRESH: &[u16] = &[204];
// 409 on a re-run: the target already holds a row in the group (`invite_tx`).
pub const GROUPS_INVITE: &[u16] = &[201, 409];
pub const GROUPS_INVITE_FRESH: &[u16] = &[201];
// 409 on a re-run: the caller already holds a row in the group (`join_tx`); a pruned request row
// is joined again with 200.
pub const GROUPS_JOIN: &[u16] = &[200, 409];
pub const GROUPS_JOIN_FRESH: &[u16] = &[200];
// Members leave before the owner: the last admin leaving a group with other members is 409.
pub const GROUPS_LEAVE: &[u16] = &[204];
pub const GROUPS_LIST_MINE: &[u16] = &[200];
pub const GROUPS_MEMBERS: &[u16] = &[200];
pub const GROUPS_PENDING: &[u16] = &[200];
// 409: the invitation was answered between the list and this respond.
pub const GROUPS_RESPOND: &[u16] = &[204, 409];
pub const GROUPS_RESPOND_FRESH: &[u16] = &[204];
pub const INVENTORY_GRANT: &[u16] = &[200];
pub const INVENTORY_LIST_CHARACTER: &[u16] = &[200];
pub const INVENTORY_LIST_MINE: &[u16] = &[200];
pub const LEADERBOARD_TOP_SCORES: &[u16] = &[200];
// 409 when a `ReportId` repeats with other players; the same players answer 202.
pub const MATCH_REPORT: &[u16] = &[202, 409];
pub const MATCH_REPORT_FRESH: &[u16] = &[202];
pub const NOTIFICATIONS_DELETE: &[u16] = &[204];
pub const NOTIFICATIONS_LIST: &[u16] = &[200];
// Idempotent: a row already read answers 204 again and keeps its first read time.
pub const NOTIFICATIONS_MARK_READ: &[u16] = &[204];
pub const PARTY_ACCEPT: &[u16] = &[200];
pub const PARTY_CREATE: &[u16] = &[201];
pub const PARTY_DECLINE: &[u16] = &[204];
pub const PARTY_GET: &[u16] = &[200];
pub const PARTY_HEARTBEAT: &[u16] = &[200];
pub const PARTY_INVITE: &[u16] = &[201];
pub const PARTY_INVITES: &[u16] = &[200];
pub const PARTY_KICK: &[u16] = &[200];
pub const PARTY_LEAVE: &[u16] = &[200];
pub const PARTY_PROMOTE: &[u16] = &[200];
pub const PARTY_SET_READY: &[u16] = &[200];
pub const STORAGE_DELETE: &[u16] = &[204];
pub const STORAGE_GET: &[u16] = &[200];
// A read after the delete: the object is gone.
pub const STORAGE_GET_DELETED: &[u16] = &[404];
pub const STORAGE_LIST: &[u16] = &[200];
// `expected_version: 0` creates; a re-run finds the object and the CAS answers 409.
pub const STORAGE_PUT: &[u16] = &[200, 409];
pub const STORAGE_PUT_FRESH: &[u16] = &[200];
pub const WALLET_LIST_CURRENCIES: &[u16] = &[200];
pub const WALLET_MY_BALANCES: &[u16] = &[200];

pub const ALL: &[(&str, &[u16])] = &[
    (ops::ACCOUNTS_BEGIN_DELETE, ACCOUNTS_BEGIN_DELETE),
    (ops::ACCOUNTS_CREATE_GUEST, ACCOUNTS_CREATE_GUEST),
    (ops::ACCOUNTS_DELETE_ACCOUNT, ACCOUNTS_DELETE_ACCOUNT),
    (ops::ACCOUNTS_LINK, ACCOUNTS_LINK),
    (ops::ACCOUNTS_LOGIN, ACCOUNTS_LOGIN),
    (ops::ACCOUNTS_LOGIN_FEDERATED, ACCOUNTS_LOGIN_FEDERATED),
    (ops::ACCOUNTS_ME, ACCOUNTS_ME),
    (ops::ACCOUNTS_ME, ACCOUNTS_ME_DELETED),
    (ops::ACCOUNTS_REFRESH, ACCOUNTS_REFRESH),
    (ops::ACCOUNTS_REGISTER, ACCOUNTS_REGISTER),
    (ops::ACCOUNTS_REGISTER, ACCOUNTS_REGISTER_FRESH),
    (ops::CHARACTERS_CREATE, CHARACTERS_CREATE),
    (ops::CHARACTERS_DELETE, CHARACTERS_DELETE),
    (ops::CHARACTERS_LIST, CHARACTERS_LIST),
    (ops::FRIENDS_ACCEPT, FRIENDS_ACCEPT),
    (ops::FRIENDS_DECLINE, FRIENDS_DECLINE),
    (ops::FRIENDS_LIST, FRIENDS_LIST),
    (ops::FRIENDS_PENDING, FRIENDS_PENDING),
    (ops::FRIENDS_REMOVE, FRIENDS_REMOVE),
    (ops::FRIENDS_REQUEST, FRIENDS_REQUEST),
    (ops::GROUPS_CREATE, GROUPS_CREATE),
    (ops::GROUPS_DECIDE, GROUPS_DECIDE),
    (ops::GROUPS_DECIDE, GROUPS_DECIDE_FRESH),
    (ops::GROUPS_INVITE, GROUPS_INVITE),
    (ops::GROUPS_INVITE, GROUPS_INVITE_FRESH),
    (ops::GROUPS_JOIN, GROUPS_JOIN),
    (ops::GROUPS_JOIN, GROUPS_JOIN_FRESH),
    (ops::GROUPS_LEAVE, GROUPS_LEAVE),
    (ops::GROUPS_LIST_MINE, GROUPS_LIST_MINE),
    (ops::GROUPS_MEMBERS, GROUPS_MEMBERS),
    (ops::GROUPS_PENDING, GROUPS_PENDING),
    (ops::GROUPS_RESPOND, GROUPS_RESPOND),
    (ops::GROUPS_RESPOND, GROUPS_RESPOND_FRESH),
    (ops::INVENTORY_GRANT, INVENTORY_GRANT),
    (ops::INVENTORY_LIST_CHARACTER, INVENTORY_LIST_CHARACTER),
    (ops::INVENTORY_LIST_MINE, INVENTORY_LIST_MINE),
    (ops::LEADERBOARD_TOP_SCORES, LEADERBOARD_TOP_SCORES),
    (ops::MATCH_REPORT, MATCH_REPORT),
    (ops::MATCH_REPORT, MATCH_REPORT_FRESH),
    (ops::NOTIFICATIONS_DELETE, NOTIFICATIONS_DELETE),
    (ops::NOTIFICATIONS_LIST, NOTIFICATIONS_LIST),
    (ops::NOTIFICATIONS_MARK_READ, NOTIFICATIONS_MARK_READ),
    (ops::PARTY_ACCEPT, PARTY_ACCEPT),
    (ops::PARTY_CREATE, PARTY_CREATE),
    (ops::PARTY_DECLINE, PARTY_DECLINE),
    (ops::PARTY_GET, PARTY_GET),
    (ops::PARTY_HEARTBEAT, PARTY_HEARTBEAT),
    (ops::PARTY_INVITE, PARTY_INVITE),
    (ops::PARTY_INVITES, PARTY_INVITES),
    (ops::PARTY_KICK, PARTY_KICK),
    (ops::PARTY_LEAVE, PARTY_LEAVE),
    (ops::PARTY_PROMOTE, PARTY_PROMOTE),
    (ops::PARTY_SET_READY, PARTY_SET_READY),
    (ops::STORAGE_DELETE, STORAGE_DELETE),
    (ops::STORAGE_GET, STORAGE_GET),
    (ops::STORAGE_GET, STORAGE_GET_DELETED),
    (ops::STORAGE_LIST, STORAGE_LIST),
    (ops::STORAGE_PUT, STORAGE_PUT),
    (ops::STORAGE_PUT, STORAGE_PUT_FRESH),
    (ops::WALLET_LIST_CURRENCIES, WALLET_LIST_CURRENCIES),
    (ops::WALLET_MY_BALANCES, WALLET_MY_BALANCES),
];
