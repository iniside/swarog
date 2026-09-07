# `groups` — membership, roles and invites (the 16th fortress)

Revision 2 · 2026-09-06 · first half of the chat/groups pair the user asked for in the
Nakama shape. `chat` gets its own plan after this lands, because its channel table's
shape depends on what membership actually ships.

Revision 2 answers a REJECT verdict with twenty findings; the punch list and what each
changed are recorded in *Review response* at the end. Two of them were holes that made
`invite` and `decide` unreachable as specified.

Delivers a social-group fortress: a group, its members, two roles, three membership
states, and a **wire-only** `role_of` capability that `chat` will consume to authorize a
group channel. No messages, no channels — those are `chat`'s.

---

## Context — the overlapping systems, and why a new fortress

`CLAUDE.md`'s *Research before planning* rule requires this section first.

**Why not extend `friends`.** Its schema is built on exactly two parties per row:
`CHECK (low_id < high_id)`, a `UNIQUE (low_id, high_id)` pair index, and every store
method canonicalising with `least`/`greatest` (`modules/friends/src/lib.rs:39-54`,
`store.rs:102,144-146,165-166`). The ordered-pair addressing has no n-ary analogue —
group membership is not a degenerate friendship, and bending that table to hold it would
destroy the property that makes a crossing pair of requests one relation instead of a
read-then-write.

**Why not extend `notifications`.** It is sink-only: no `api/notifications/events` crate
exists, and its RPC surface is `list`/`mark_read`/`delete` with no post side
(`modules/notifications/src/lib.rs:1-8`). Groups needs both a publish surface and
synchronous writes.

**Why not extend `mail`.** It has no `api/mail/api` crate at all, its ingress is a durable
command topic, and its state machine (`pending/sent/parked/cancelled`, lease-by-committed
UPDATE, `generation` CAS) is oriented entirely around one-shot delivery retry
(`modules/mail/src/lib.rs:1-13`). No conversation, no membership, no player identity.

**Why `groups` and `chat` are two fortresses, not one.** The user chose the Nakama split.
It also survives the fortress test on its own: a group is durable social state that
outlives every message in it, while chat is an append-heavy log with retention. Different
write rates, different retention, different failure surfaces — and `chat` needs `groups`
only through one synchronous predicate, which is exactly what a contract crate is for.

---

## Findings that shape the design

**A. `friends` provides no synchronous relation check, and `groups` must not repeat that.**
`friends` registers exactly one capability, `dyn friendsapi::Player` under
`key("friends","player")` (`modules/friends/src/lib.rs:108-109`), and every one of its six
methods is `#[http]` with a leading `Identity`. There is no way for another module to ask
"are these two accepted friends" — a consumer would have to page a hundred rows at a time
holding the wrong player's identity. `chat` will hit exactly this wall for group
membership, so **`groups` ships its wire-only capability in the same rollout as its player
face**, not as a later retrofit. `walletapi::Wallet` beside `walletapi::Player` and
`charactersapi::Ownership` beside its player face are the shipped precedents.

**B. The C# generator's type lattice bans most scalars.**
`tools/csharp-client-gen/src/scrape.rs:401-437` models `String`, `i64`, `i32`, `Vec<T>`,
`()` and DTO structs — and nothing else. `bool`, `u32`, `usize` and every other width are
a hard `Err` on any type transitively reachable from an `#[http]` method, and
`codegen-freshness` is blocking. So: **no `is_admin: bool`, no `member_count: u32`, no
Rust enum for a role.** A role is a `String` with exported consts, the way
`friendsapi::STATE_PENDING`/`DIRECTION_INCOMING` are (`api/friends/api/src/lib.rs:38-47`),
and absence is the empty string, the way `Notification::read_at` encodes "unread"
(`api/notifications/api/src/lib.rs:61-64`).

**C. There is no query-parameter source in the `#[http]` grammar.** `ArgSource` is a
closed two-variant enum — `Body` or `Path { wildcard }` (`core/opsapi/src/lib.rs:343-354`)
— so a paged read carries its cursor in the JSON body and is therefore `POST`, with
`#[retry_safe]` restoring the replay semantics a GET would have had for free. Both
`notifications::list` and `friends::list` are that shape.

**D. DTO names are globally unique across every `api/*/api` crate.**
`csharp-client-gen` keeps a flat cross-crate registry, which is why `friendsapi` had to
name its page `FriendPage` — `notificationsapi::Page` already existed. `groups` must name
defensively from the start: `GroupPage`, `MemberPage`, `GroupSummary`, `MemberSummary`.

**E. The session budget absorbs two more fortresses without re-derivation.** processctl
reserves 91 of 131 today (15 DB-backed services, one with the scheduler's extra fire
session); `groups` and `chat` take it to 103. weles' fixture goes 105 → 119 against 147
usable. Next free pairs: **HTTP 8096 / edge 9015** for `groups`, 8097/9016 reserved for
`chat`.

**F. `friends` is missing from the gateway's address table, and it is worse than cosmetic.**
`friends` is the only one of the eight `#[http(` domains with no `<NAME>_EDGE_ADDR` row in
`cmd/gateway-svc/src/addrs.rs`'s `ADDR_SPECS`; the gateway falls back to the literal
`127.0.0.1:9014` at `cmd/gateway-svc/src/lib.rs:141`. `cmd/admin-svc/src/main.rs:54` does
read the variable — only the gateway drops it. Two live consequences, not one: `processctl`
actively sets `FRIENDS_EDGE_ADDR` for the gateway (`fleet.rs:829`) and it is **discarded**;
and in **managed** mode `ADDR_SPECS` is the only thing the agent is asked to resolve, so a
managed gateway never resolves friends at all and dials loopback on whatever host it runs
on. Step 7 adds `groups` **and** closes friends' gap.

---

## Contract shape (settled here, not during implementation)

### Vocabulary

`api/groups/api/src/lib.rs`:

```rust
pub const MAX_NAME_BYTES: usize = 64;
pub const MAX_CURSOR_BYTES: usize = 256;
pub const MAX_PAGE_LIMIT: i64 = 100;
pub const DEFAULT_PAGE_LIMIT: i64 = 50;
pub const MAX_MEMBERS: i64 = 500;

/// A membership row's state. Empty string is never a state — absence is no row.
pub const STATE_MEMBER: &str = "member";
pub const STATE_INVITED: &str = "invited";
pub const STATE_REQUESTED: &str = "requested";

/// A member's role. Only a `member` state carries one; `invited`/`requested` rows
/// carry the empty string.
pub const ROLE_ADMIN: &str = "admin";
pub const ROLE_MEMBER: &str = "member";

/// How a non-member gets in. Chosen at create time and immutable in this rollout.
pub const JOIN_OPEN: &str = "open";       // join() admits immediately
pub const JOIN_REQUEST: &str = "request"; // join() records a request an admin decides
pub const JOIN_INVITE: &str = "invite";   // join() is refused; only an invite admits
```

Every one of these is a `String` on the wire for finding B's reason, and each is a
`&'static str` const so a rename is a compile error at every call site rather than a typo
in a literal.

### The player face

Nine ops, not seven. Revision 1 had a group with no way to accept an invitation and no
way for an admin to see a pending request — see *Review response* 1 and 2.

```rust
#[rpc(prefix = "groups")]
#[async_trait]
pub trait Player: Send + Sync {
    #[http(verb = "POST", path = "/groups", auth = "player", success = 201)]
    async fn create(&self, identity: Identity, name: String, join_policy: String)
        -> Result<GroupSummary, Error>;

    /// Every group the caller holds ANY row in — `member`, `invited` and `requested`
    /// alike, each carrying its own state. This is also the invitation inbox: without
    /// pending rows here an invited player has no way to discover the invitation.
    #[http(verb = "POST", path = "/groups/list", auth = "player", success = 200)]
    #[retry_safe]
    async fn list_mine(&self, identity: Identity, cursor: String, limit: i64)
        -> Result<GroupPage, Error>;

    /// `member` rows only. Any member of the group may read it.
    #[http(verb = "POST", path = "/groups/{id}/members/list", auth = "player",
           success = 200, path_args(group_id = "id"))]
    #[retry_safe]
    async fn members(&self, identity: Identity, group_id: String, cursor: String, limit: i64)
        -> Result<MemberPage, Error>;

    /// `requested` and `invited` rows. ADMIN ONLY — a plain member reading the pending
    /// roster is a different decision, and this is the op that hands an admin the
    /// `subject_id` that `decide` needs.
    #[http(verb = "POST", path = "/groups/{id}/pending/list", auth = "player",
           success = 200, path_args(group_id = "id"))]
    #[retry_safe]
    async fn pending(&self, identity: Identity, group_id: String, cursor: String, limit: i64)
        -> Result<MemberPage, Error>;

    #[http(verb = "POST", path = "/groups/{id}/join", auth = "player", success = 200,
           path_args(group_id = "id"))]
    async fn join(&self, identity: Identity, group_id: String) -> Result<MemberSummary, Error>;

    #[http(verb = "POST", path = "/groups/{id}/leave", auth = "player", success = 204,
           path_args(group_id = "id"))]
    async fn leave(&self, identity: Identity, group_id: String) -> Result<(), Error>;

    /// ADMIN ONLY. Creates an `invited` row for the named player.
    #[http(verb = "POST", path = "/groups/{id}/invites", auth = "player", success = 201,
           path_args(group_id = "id"))]
    async fn invite(&self, identity: Identity, group_id: String, target_handle: String)
        -> Result<(), Error>;

    /// The SUBJECT's verdict on its OWN `invited` row. `decision` is `accept` or
    /// `reject`.
    #[http(verb = "POST", path = "/groups/{id}/respond", auth = "player", success = 204,
           path_args(group_id = "id"))]
    async fn respond(&self, identity: Identity, group_id: String, decision: String)
        -> Result<(), Error>;

    /// An ADMIN's verdict on somebody else's `requested` row, and the only way to remove
    /// another member: `accept` promotes a `requested` row to `member`, `reject` deletes
    /// whatever row the subject holds — which is a decline for a pending row and a kick
    /// for a `member` row.
    #[http(verb = "POST", path = "/groups/{id}/decide", auth = "player", success = 204,
           path_args(group_id = "id"))]
    async fn decide(&self, identity: Identity, group_id: String, subject_id: String,
                    decision: String) -> Result<(), Error>;
}
```

**The authorization matrix is part of the contract, not a Step 2 detail.** Every cell:

| op | caller must be | subject row must be | effect |
|---|---|---|---|
| `create` | any player | — | group + creator's `member`/`admin` row |
| `list_mine` | any player | own rows | all three states |
| `members` | `member` of the group | — | `member` rows |
| `pending` | `admin` of the group | — | `invited` + `requested` rows |
| `join`, policy `open` | non-member | none | `member`/`member` |
| `join`, policy `request` | non-member | none | `requested`/`''` |
| `join`, policy `invite` | non-member | none | `Conflict` — only an invite admits |
| `join` | already holds a row | any | `Conflict`, never a silent second row |
| `leave` | holds any row | own | row deleted; see the last-admin rule |
| `invite` | `admin` | subject holds no row | `invited`/`''` |
| `invite` | `admin` | subject already holds a row | `Conflict` |
| `respond` | the subject | own row is `invited` | `accept` ⇒ `member`/`member`; `reject` ⇒ deleted |
| `decide` | `admin` | subject is `requested` | `accept` ⇒ `member`/`member`; `reject` ⇒ deleted |
| `decide` | `admin` | subject is `member` | `accept` ⇒ `Conflict`; `reject` ⇒ deleted (kick) |
| `decide` | `admin` | subject is the caller | `Conflict` — use `leave` |

Anything the caller may not see answers `NotFound`, never `Forbidden`: a non-member's
`members`, a non-admin's `pending`/`invite`/`decide`, and every op naming a group that
does not exist all give the same verdict. A 403 would confirm the id names a real group.

**Errors that are otherwise a 500.** `join_policy` outside the three consts and `decision`
outside `{accept, reject}` are validated in the service and answer `Status::Invalid`; left
to the CHECK constraint they raise `23514`, which has no mapping and surfaces as an
internal error. `MAX_NAME_BYTES` is enforced in the service before the statement, with the
column CHECK as the backstop — the `COLUMN_CAPS` pairing `mail` uses.

**The last-admin and last-member rules**, because "promote/demote is a non-goal" makes
both reachable:
- The **only** member leaving deletes the group row in the same transaction. Nobody is
  stranded and no zombie row survives — `groups.groups` has no other collector.
- The last **admin** leaving a group that still has other members is `Conflict` with a
  message naming the reason. Without this the group freezes forever: no accepts, no
  invites, no kicks, and no delete.

**None of the mutating ops is `#[retry_safe]`.** A replay of `join`/`leave`/`invite`/
`respond`/`decide` after an ambiguous failure cannot be told apart from a second,
intentional call on a row the caller may no longer hold — the reason `friends::accept` and
`notifications::delete` decline it too. `create` mints a fresh id, so a replay would make a
second group. Only the four reads carry it.

### The DTOs

```rust
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupSummary {
    pub id: String,
    pub name: String,
    pub join_policy: String,
    pub created_at: String,   // RFC3339
    pub my_state: String,     // STATE_MEMBER | STATE_INVITED | STATE_REQUESTED
    pub my_role: String,      // ROLE_ADMIN | ROLE_MEMBER, empty unless my_state is member
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberSummary {
    pub player_id: String,
    pub handle: String,       // "Name#1234", empty when accounts has no row for the id
    pub state: String,
    pub role: String,
    pub joined_at: String,    // RFC3339
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupPage { pub items: Vec<GroupSummary>, pub next_cursor: String }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberPage { pub items: Vec<MemberSummary>, pub next_cursor: String }
```

**Handles are resolved once per page, not once per row.** `accountsapi::Directory::players_by_id`
takes up to 256 ids in one call and omits misses rather than erroring, which is exactly
what `friends`' pages do. A missing id yields an empty `handle` — the row still lists,
because a member whose account row vanished must not make the whole page fail.

**No `member_count` on `GroupSummary`.** It would cost a count per row on every page, and
the width question (`i64` — `u32` is banned) is the smaller half of the problem. Recorded
as a non-goal.

### The wire-only face — what `chat` will consume

```rust
#[rpc(prefix = "groups")]
#[async_trait]
pub trait Membership: Send + Sync {
    /// The caller's role in a group, or the empty string when it holds no `member` row.
    /// No `Identity`: this is a server-to-server predicate and must never be reachable
    /// from the front door, where it would be an oracle over group rosters.
    #[retry_safe]
    async fn role_of(&self, group_id: String, player_id: String) -> Result<String, Error>;
}
```

Provided under `registry::key("groups", "membership")`. One index probe. It answers `""` for "not a member" and, deliberately, the
same `""` for "no such group" — **because `chat` wants the same behaviour for both**: a
channel requested for a phantom group and one requested for a group the caller is not in
must both answer `NotFound`, so a second method distinguishing them would only give `chat`
a distinction it must then discard. Stated here rather than left to be discovered, since
retrofitting a second method is a contract change.

### Schema

```sql
CREATE SCHEMA IF NOT EXISTS groups;
CREATE TABLE IF NOT EXISTS groups.groups (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name         text        NOT NULL,
    join_policy  text        NOT NULL,
    creator_id   uuid        NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT groups_name_len_check   CHECK (octet_length(name) <= 64),
    CONSTRAINT groups_join_policy_check CHECK (join_policy IN ('open','request','invite'))
);
CREATE TABLE IF NOT EXISTS groups.memberships (
    group_id   uuid        NOT NULL,
    player_id  uuid        NOT NULL,
    state      text        NOT NULL,
    role       text        NOT NULL DEFAULT '',
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (group_id, player_id),
    CONSTRAINT memberships_state_check CHECK (state IN ('member','invited','requested')),
    CONSTRAINT memberships_role_check
        CHECK ((state = 'member') = (role <> '')
               AND (role = '' OR role IN ('admin','member')))
);
CREATE INDEX IF NOT EXISTS memberships_player_idx
    ON groups.memberships (player_id, state, created_at DESC, group_id DESC);
CREATE INDEX IF NOT EXISTS memberships_group_idx
    ON groups.memberships (group_id, state, created_at DESC, player_id DESC);
CREATE INDEX IF NOT EXISTS memberships_pending_idx
    ON groups.memberships (created_at) WHERE state <> 'member';
```

No cross-module FK to `accounts` (constraint 10) — `player_id` is a plain uuid column, and
a malformed one folds to an empty page rather than a 500, the `22P02` handling
`notifications` and `friends` both carry. The composite primary key is what makes a
double `join` one row: there is no read-then-write anywhere in this module.

The `memberships_role_check` constraint is an **equivalence, not an implication**: a
`member` row must carry a role AND a non-`member` row must not. Revision 1 wrote only the
forward half, which admitted `state='member' AND role=''` — an accept that updated `state`
and forgot `role` would commit, `role_of` would answer `""` for a real member, and `chat`
would deny a member its own group channel: the exact failure the capability exists to
prevent.

`memberships_pending_idx` is partial and exists for Step 5's sweep alone; without it the
prune seq-scans a table whose live rows dominate.

### Durable events

`api/groups/events/src/lib.rs`, all `MinRetention { days: 30 }` matching `friends`:

| topic | payload |
|---|---|
| `group.created` | `{group_id, name, creator_id, join_policy}` |
| `group.member_joined` | `{group_id, player_id, role}` |
| `group.member_left` | `{group_id, player_id, actor_id, reason}` — `reason ∈ {"left","kicked","declined"}` |

`create` emits **both** `group.created` and a `group.member_joined` for the creator's
`admin` row, in the same transaction. Without the second, the ledger shows a group whose
only admin never joined and a later `member_left` for the creator has no matching join.

`actor_id` is on `member_left` because `friendsevents::Removed` carries one and a kick is
otherwise unattributable in a ledger that retains 30 days. `reason` is an open vocabulary
on purpose: `friendsevents` records the rule that an
unrecognised value must be treated as the plain case, so a future ending is an additive
change rather than a new topic.

**Every event is emitted with `emit_tx` inside the transaction that wrote the row.** A
consumer is owed for each topic or `topiccheck --durability-strict` fails: Step 8 adds
`audit`'s three raw sinks, which is what `friends` did and what makes the ledger's
coverage complete rather than selective.

### Push

`groups` pushes nothing in this rollout. A membership change is durable state a client
re-reads; there is no live surface waiting on it yet, and `chat` is what will make one.
Adding a nudge later is additive. Recorded so the omission reads as a decision.

---

## Step 1 — contract crates  `[sonnet]`

**(a) What.** `api/groups/{api,events,rpc}/` and their workspace registration.

**(b) Why now.** Everything imports them; nothing here depends on anything else.

**(c) How.** Copy `api/friends/{api,events,rpc}/` — the closest and most recently reviewed
shape. `groupsapi` carries both traits and every const above; `groupsevents` carries the
three descriptors plus the `#[doc(hidden)] golden_samples()` the contract golden requires.
`groupsrpc` invokes the meta-macro for **both** traits and re-exports
`adminrpc::{register_admin, register_admin_submit}`. **`define(` must be a single line** —
`tools/topiccheck/src/tests.rs:281` scans for the topic literal on the `define(` line and
panics otherwise, which turned a blocking stage red in the mail rollout.

**Also in this step, and it is not optional:** add the two `#[rpc]` traits to
`tools/topiccheck/src/golden.rs`'s `rpc_modules()`. `rpc_modules_from_fs`
(`golden.rs:488-510`) scans `api/*/api/src` for every `#[rpc]` trait and **bails with a
per-entry fix** — not a diff — the moment the hand-list drifts, so landing the contract
crate without it turns the blocking contract-golden stage red for every step until Step 8.
Unlike the durable topics, this entry depends on no consumer, so it belongs here.

**(d) Dispatch.** `[sonnet]` — three crates from three named templates plus one list entry.

## Step 2 — the module: schema, store, service, player ops  `[opus]`

**(a) What.** `modules/groups/{Cargo.toml,src/{lib,store,service}.rs}`. No admin module
yet — Step 6 creates `admin.rs`, and a placeholder that Step 6 must remember to delete is
a rail with no stated death.

**(b) Why now.** The row and the seven ops are what every later step acts on.

**(c) How.** `friends`' service/store split is the template. Non-negotiables:
- **Ownership is a predicate in the statement, never a comparison afterwards.** A group
  the caller cannot see and a group that does not exist answer the same `NotFound`; a 403
  would confirm the id names a real group, which is an enumeration oracle
  (`api/friends/api/src/lib.rs:12-14` states the rule, `modules/notifications/src/store.rs:103-140`
  shows the enforcement).
- The keyset codec is copied per-module, not shared: base64url-no-pad over
  `"{created_at}|{id}"`, **calendar-validated** (`is_cursor_time` rejects 2026-02-30 and
  hour 25 — letting them reach `$2::timestamptz` raises 22008 and answers 500),
  malformed ⇒ `Status::Invalid`, never a silent reset to page 1, and the cap check runs
  **before** the base64 decode.
- `resolve_limit`: `0` means unspecified (the contract has no `Option`), over-ask is
  clamped, only a negative is rejected.
- **`MAX_MEMBERS` needs a per-group advisory lock, not a clever statement.** An
  `INSERT … WHERE (SELECT count(*)) < 500` is a count-then-insert however it is spelled:
  under READ COMMITTED two concurrent joins both see 499 and both commit. The precedent
  says so verbatim — `modules/friends/src/store.rs:42-49`: *"without it two concurrent
  requests by one player both count below the cap (neither committed yet, READ COMMITTED)
  and both land past it"* — and friends therefore takes a `pg_advisory_xact_lock`. Take one
  keyed on the group, namespaced the way friends namespaces its requester key.
- Validate `join_policy`, `decision` and `MAX_NAME_BYTES` **in the service**, before the
  statement; the CHECK constraints are the backstop, and a `23514` reaching a caller is a
  500 with no mapping.
- `invite` resolves `target_handle` through `accountsapi::Directory::find_by_handle`, so
  `groups::requires()` is `vec!["accounts".into()]` — the same dependency `friends` has.

**(d) Dispatch.** `[opus]` — core-implementer, effort *think hard*.

## Step 3 — `cmd/groups-svc` and the boot lists  `[sonnet]`

**(a) What.** `cmd/groups-svc/{Cargo.toml,src/{lib,main}.rs}`, `cmd/server`,
`tools/checkmodules`.

**(b) Why now.** `archcheck` fails a `modules/<name>` with no `cmd/<name>-svc` root, and
`topiccheck` builds each profile's module set from `cmd/server` and `checkmodules` — a
subscription invisible to those lists reports as unsubscribed, which is the ordering
defect the mail rollout hit five steps too late.

**(c) How.** Copy `cmd/friends-svc`. Modules are `[Metrics, Groups, Stub("accounts")]`.
No `PushSender` — this module pushes nothing.

**(d) Dispatch.** `[sonnet]`.

## Step 4 — the wire-only `Membership` capability  `[opus]`

**(a) What.** The `Membership` impl, its `registry::provide` in `register`, and its
`register_server` inside the `EDGE_SLOT` closure; `groupsrpc::remote_factories()`.

**(b) Why now.** Separate from Step 2 because it is the seam `chat` consumes and it must
not be reachable from the front door — a mistake here is a roster oracle, and it deserves
its own diff and its own review.

**(c) How.** `walletapi::Wallet` is the precedent for a wire-only trait beside a player
face. `remote_factories()` returns the `provide_remote` for `Membership` **only** — no
`route_bindings`, because a consuming svc hosts no front door. Note `remote::Stub::new`
with an empty factory list bails in `register`, which is why gateway uses `describe_peer`.

**(d) Dispatch.** `[opus]` — core-implementer, effort *think hard*.

## Step 5 — retention for stale invites and requests  `[sonnet]`

**(a) What.** A `groups.prune-on-scheduler.v1` subscription, `GROUPS_RETENTION_DAYS`, and
the `groups-prune` schedule row.

**(b) Why now.** After the states exist. Before the fleet step so the schedule is
registered once.

**(c) How.** Copy `mail`'s prune verbatim in shape, including the parts that are
load-bearing: `on_tx_raw` on `schedulerevents::FIRED.topic()` filtered to the schedule
name with the mismatch arm returning `Ok(())`; the batched `ctid` + `FOR UPDATE SKIP
LOCKED` loop with `PRUNE_BATCH = 256`; the `created_at` **watermark**, which is load-bearing
because every batch runs inside the one still-open delivery transaction where already-
deleted tuples are neither killable nor prunable, so a watermark-less loop goes quadratic;
and the `PRUNE_BUDGET = 5s` exit. **Delete only `state <> 'member'`** — a member row is
live state and must never be swept. `GROUPS_RETENTION_DAYS` follows the
`NOTIFICATIONS_RETENTION_DAYS` convention exactly: unset takes the compiled default (30),
anything present and unusable fails startup, range `1..=3650`. Add the const to
`schedulerevents::schedule_names`, the seed row to `modules/scheduler`'s `SCHEMA_DDL`, and
the name to `seeded_schedule_names_are_contract`.

**(d) Dispatch.** `[sonnet]` — a named file copied with one named change.

## Step 6 — the "Groups" admin page  `[opus]`

**(a) What.** `modules/groups/src/admin.rs`, the `adminapi::SLOT` contribution, and
`register_admin` + `register_admin_submit` in the existing `EDGE_SLOT` closure.

**(b) Why now.** After the ops it renders; before the fleet step that wires admin-svc.

**(c) How.** `ADMIN_ITEM_ID = "groups"`, `ADMIN_LABEL = "Groups"`,
`ADMIN_SECTION = "Player Support"` (beside `friends` and `notifications`). **Build every
self-link from `adminapi::slug(ADMIN_LABEL)`**, never from the item id — that function is
the portal's route authority and the two differ silently. `admin_data` must never `Err` on
a foreign or malformed param: the portal forwards every page's params to every provider,
so an `Err` degrades an unrelated page in the split; render an error card. Rejections map
`Stale → Conflict`, `Rejected → invalid`, `Internal → internal` — **never `NotFound`**,
which the edge makes indistinguishable from `UnknownMethod`, silently degrading the page
to read-only. Copy `friends`' page, not `audit`'s or `notifications`' — both of those
`.map_err(internal)` in `admin_data`, which the contract forbids.

**(d) Dispatch.** `[opus]` — admin UI is `[opus]` or above by standing rule.

## Step 7 — fleet and topology registration  `[sonnet]`

**(a) What.** `tools/processctl/src/fleet.rs` + `fleet_tests.rs`; `cmd/gateway-svc/src/{lib,addrs,addrs_tests}.rs`;
`cmd/admin-svc/src/{lib,main}.rs`; `weles/fleet.split.toml`; `weles/master/src/{fleet_toml_tests,manifest_tests}.rs`.

**(b) Why now.** Everything above is code that exists; this makes both topologies contain
it, and it must precede the split-proof step.

**(c) How.** `groups-svc` takes **HTTP 8096 / edge 9015**, depends on `accounts-svc`, and
is peered from gateway (`describe_peer`, since it has `#[http]` ops) and from admin-svc.
The weles block goes **before** its consumers — `fleet_toml::validate` enforces that a
peer's provider appears earlier in the file. `fleet_toml_tests.rs`'s service count goes
16 → 17. `manifest_tests.rs` needs the full byte-exact env golden. **Add the
`GROUPS_EDGE_ADDR` row to `ADDR_SPECS` and, in the same commit, the missing
`FRIENDS_EDGE_ADDR` row** — finding F; leaving a known twin of a defect you just avoided
is what the sweep rule forbids. Correct `addrs.rs`'s and `addrs_tests.rs`' "ten addresses"
prose, which becomes false either way.

**(d) Dispatch.** `[sonnet]` — enumerated edits against named files.

## Step 8 — the verification gates  `[opus]`

**(a) What.** `tools/topiccheck/src/{main,golden}.rs`; `modules/audit` (three raw sinks);
`tools/conformance/src/{policy,checks}.rs`; `modules/groups/src/conformance.rs`;
`tools/opscatalog-gen/src/main.rs`; `tools/csharp-client-gen/src/scrape.rs` **and**
`tools/csharp-client-gen/src/tests.rs:60-100` (a hand-written wire-method set AND DTO set
that fails on the new ops); `modules/apikeys`'s `DEV_CLIENT_POLICY`; and the two generated
artifacts themselves — `clients/csharp/Generated/` and `opscatalog/src/generated.rs`,
both regenerated and diffed by the blocking `codegen-freshness` stage
(`tools/verifyctl/src/stages/codegen.rs:17-81`).

**(b) Why now.** These are the blocking stages that fail *because* Steps 1–6 landed, and
each needs a judgement the mechanical lane should not make.

**(c) How.** `defined_topics()` and `event_samples_by_crate()` are hand-enumerated lists
whose own docs call themselves the one conscious edit point. The three `group.*` topics
need consumers or `--durability-strict` fails: add `audit.group-created.v1`,
`audit.group-member-joined.v1`, `audit.group-member-left.v1` as raw sinks, and extend
audit's anti-drift test that diffs its audited topic set against the producers' declared
topics. `opscatalog-gen` and `csharp-client-gen` both carry provider-completeness lists
that are a **build-time hard failure** when a module has `#[http]` ops and is missing.
`DEV_CLIENT_POLICY` must list every player-facing wire method or `dev-key-client` gets 403
and it looks like a key bug; its reverse-containment test fails closed. The `groups()`
conformance entry needs a concrete stance per convention with **CapCases that drive the
real validators** — seq #2a's lesson is that a proof audit deleted both input-cap guards
from a production handler and the gate still printed OK, because the basis field is prose
nothing executes. **Prove at least one CapCase bites**: break the guard, watch the named
failure, restore byte-exactly, report the message verbatim.

**(d) Dispatch.** `[opus]` — this is where a plausible-but-wrong stance silently disables
a gate.

## Step 9 — split-proof assertions `[GR1]`–`[GR6]`  `[test-author]`, `model:"opus"`

**(a) What.** `tools/splitproof/src/main.rs`, called from the split pass and the monolith
parity pass.

**(b) Why now.** Against the landed, registered fleet.

**(c) How.** `[GR1]` create → join → members list through gateway-svc, DB-asserted.
`[GR2]` the three join policies: `open` admits, `request` records a `requested` row that
`decide` accepts, `invite` refuses a bare join. `[GR3]` authz negatives — a non-member's
`members` read and a non-admin's `decide` both answer `NotFound`, not `Forbidden`.
`[GR4]` **the wire-only `role_of` is reachable over the internal edge and NOT through the
front door** — the second half is the one that matters, and it must assert a 404/405 from
the gateway rather than merely not calling it. `[GR5]` the admin page renders through
admin-svc (remote `admin.adminData`) with a created group visible. `[GR6]` the prune
sweeps a stale `requested` row and leaves a `member` row untouched, `[SP2]`'s
force-`last_fired` shape. `[GR*m]` re-runs the set against `cmd/server`.
**Prove one assertion non-vacuous** by perturbing what it names, observing the failure,
and reverting byte-identically.

**(d) Dispatch.** `[test-author]` at `model:"opus"` — the front-door-negative assertion is
a shape the harness has not carried before.

## Step 10 — module unit tests  `[test-author]`, `model:"sonnet"`

**(a) What.** `modules/groups/src/{tests,service_tests,store_tests,projection_tests}.rs`.

**(b) Why now.** From the landed, compiling diff.

**(c) How.** Each test names the branch that would otherwise be unproven: the cursor
codec's reject arms (pure, no DB) including the calendar cases; `resolve_limit`'s three
arms; the `NotFound`-not-`Forbidden` answer for a foreign group **and** for an absent one,
proven to be the same verdict; `MAX_MEMBERS` enforced in the statement under two
concurrent joins; the `memberships_role_check` constraint rejecting a `requested` row with
a role; `role_of` answering `""` for a non-member and for a non-existent group alike; the
prune deleting `invited`/`requested` past retention and **leaving `member` untouched**,
with the batch loop terminating; and `decide`'s reject arm on a `member` row being a kick
that emits `group.member_left` with `reason = "kicked"`.
**Timing doctrine:** no sleeping on a real clock — explicit persisted state, concurrency
rather than speed, a paused tokio clock where a timer is involved.
**Fixture hygiene:** cleanup is a drop guard, not a trailing call — a panicking test must
not strand rows in the shared database, and a guard that cannot block must degrade with a
warning rather than panic during unwind.
**Confirm zero `SKIP: postgres unreachable` lines** in the final run and say so; a green
suite that skipped every DB test is the repo's recorded false-green shape.

**(d) Dispatch.** `[test-author]` at `model:"sonnet"` — these follow landed patterns.

## A note on lanes, and one explicit carve-out

CLAUDE.md's Implementation Mode says tests are never `[sonnet]` and are their own later
step. Steps 5, 7 and 8 each extend an **existing hand-maintained list assertion** with the
new service's row — `seeded_schedule_names_are_contract`, `fleet_tests.rs`'s canonical
snapshot, `fleet_toml_tests.rs`' service count, `manifest_tests.rs`' env goldens, audit's
anti-drift list and `csharp-client-gen/src/tests.rs`' method set. Those edits are the
assertion half of the registration data the same step adds; splitting them out means
deliberately landing a knowingly-red blocking gate and carrying it across steps, which is
the sequencing defect this plan already corrects twice. So: **extending an existing list
assertion travels with its registration; authoring any NEW assertion remains
`[test-author]`** — Steps 9 and 10 own every new test in this rollout. Recorded as a
deviation rather than taken silently.

## Step 11 — acceptance  `[inline]`

`cargo run -p verifyctl -- --fast`, then `--all --strict`. One rollout at a time: check
`pgrep -x cargo; pgrep -x rustc`, require no active fleet, run exactly one. Redirect and
capture `$?` — a piped `| tail` reports the pipe's status, which produced a false green
twice in this repo. Blessings expected: `--bless-public-api` (`groupsapi.txt`,
`groupsevents.txt`, and `schedulerevents.txt` for the new schedule const),
`--bless-contract-golden`, and `--bless-input-golden` for the admin form's fields, plus
regenerating both codegen artifacts with `cargo run -p opscatalog-gen` and
`cargo run -p csharp-client-gen`. Read each diff before accepting it.

## Step 12 — documentation  `[docs]`, `model:"sonnet"`

`docs/roadmap/feature-tracker.md` (a new row and a dated decisions entry), `README.md`,
`CLAUDE.md` (the module list, the fortress count, and the split-proof port sentence),
`.agents/shared/gamebackend.md` (which mirrors all three of those claims), `AGENTS.md`
(a `docs-current` ROOT_DOCUMENT, `tools/verifyctl/src/stages/docs_current.rs:6`),
`docs/reference/game-backend-feature-gaps.md:87,200-201` (whose "Groups / guilds / clans ❌"
row and "P1#8 — new `groups` fortress" recommendation both become false), and this plan's
errata. Name every file with its line — an unnamed file is never found. The gap list must
be complete or it is silence implying coverage: no push nudge, no promote/demote, no
group rename, `join_policy` immutable after create, and `chat`'s dependency on `role_of`
unexercised until `chat` lands.

---

## Non-goals (deliberate, recorded)

- **Channels and messages** — `chat`'s plan, after this lands.
- **Promote/demote and role changes** — the only role assignment is the creator's `admin`
  at create time. A second admin needs an op with its own authorization story; deferred
  rather than half-shipped.
- **Group rename, avatar, description, `open`/`closed` toggling** — `join_policy` is
  immutable in this rollout; changing it retroactively raises "what happens to pending
  requests", which is a decision, not a field.
- **Group search / public listing** — `list_mine` only. A discovery surface is an
  enumeration oracle over a social graph and deserves its own design.
- **Blocking** — recorded in the friends plan as belonging to `friends` as a third edge
  state. `groups` must not grow a second authority for "may these two interact".
- **A push nudge on membership change** — additive later; nothing consumes it yet.
- **`member_count` on a group summary** — a count per row on every page, for a number the
  client can get from `members`' page. Additive later if a screen needs it.
- **A group-delete op.** The only collection is the last-member rule above: a group whose
  last member leaves is deleted in that transaction. An admin cannot delete a group that
  still has members, and that is deliberate — deleting other people's membership wholesale
  is a moderation action, and moderation is not this rollout.
- **A swept invitation emits nothing.** Step 5's prune deletes stale `invited`/`requested`
  rows without a `group.member_left`, so a player is never told an invitation expired.
  Recorded as a decision rather than discovered later; a notification needs `notifications`
  to consume a topic that does not exist yet.


---

## Review response (revision 1 → 2)

The plan review returned REJECT with twenty findings. The five blocking ones were real
holes, not nits:

- **`invite` was a dead op** (1). An invited player had no way to see the invitation and no
  way to accept it: `list_mine` was "the caller's groups", `join` was refused under the
  invite policy, and `decide` was an admin verb. The invitation would sit until Step 5's
  prune swept it. Closed by making `list_mine` the invitation inbox — it returns all three
  states — and by adding `respond`, the subject's verdict on its own row.
- **`decide` on a request was equally unreachable** (2): with `members` returning only
  `member` rows, an admin could never learn a pending `subject_id`. Closed with `pending`,
  an admin-only read, mirroring `friends`' own `list`/`pending` split.
- **`decide` was three ops in a trenchcoat** (3). It carried an admin's accept, an admin's
  reject, and a kick that acts on a `member` row — while its doc claimed "a verdict on a
  pending row", already false. The subject-side accept would have made it a fourth, under a
  different authority. Split: `respond` (subject) and `decide` (admin), with the full
  authorization matrix now written into the contract instead of deferred.
- **The role CHECK admitted the row it was meant to forbid** (4). It was an implication
  where the invariant is an equivalence, so `state='member' AND role=''` was legal — an
  accept that set the state and forgot the role would commit, and `role_of` would answer
  `""` for a real member. Fixed as `(state='member') = (role <> '')`.
- **`MAX_MEMBERS` cannot be enforced in one statement** (5), and the precedent this plan
  cites says so verbatim: `friends` takes a `pg_advisory_xact_lock` precisely because two
  concurrent joins both read below the cap under READ COMMITTED. A per-group lock is now a
  named non-negotiable in Step 2, and Step 10's concurrency test would otherwise have been
  rewritten into something that proves nothing.

Also closed: the four DTOs are defined field by field with an explicit stance on handle
resolution (one batched `Directory` call per page, not an N+1) (6); the unstated
authorization and error mapping is now the matrix table (7); the last-admin freeze and the
zombie-group problem have explicit rules (8, 9); `codegen-freshness`' two generated artifacts
and `csharp-client-gen`'s hand-written test lists are named in Steps 8 and 11 (10); the
lane question has an argued carve-out rather than a silent violation (11); `rpc_modules()`
moved into Step 1, because its self-check **bails** rather than diffs and would have been
red from Step 1 to Step 8 (12); the admin placeholder is gone (13); `member_left` gains
`actor_id` and `create` is stated to emit the creator's join (14, 15); Step 12 names
`AGENTS.md` and the feature-gap doc (16); finding F is restated with its real
consequence — a managed gateway never resolves friends at all (17); `role_of`'s single
answer for both cases is argued from what `chat` needs (18); `[GR4]`'s plane and the
existing edge-dialling precedent are named (19); and the silently-swept invitation is a
recorded non-goal (20).

One finding was checked and left as-is: the review's note that the topics stay unsubscribed
until Step 8 is correct but is **not** the mail rollout's defect — `defined_topics()` is
hand-listed, so the `group.*` topics are invisible to `--durability-strict` until that
entry lands *together with* audit's sinks. There is no red window there.

## Errata

**1 — Step 4(a) dropped `groupsrpc::remote_factories()`.** Not landed in commit
`0957e39`: the aggregator's job is to turn a peer's capability into ANOTHER process's
registry entry, and no process consumes `Membership` until `chat`, so shipping it now
would pin a public contract-crate symbol nothing calls. The first `Membership` consumer
adds it — recorded here so Steps 8/11 and the `chat` plan do not assume it exists.

**2 — Step 2(b)'s "seven ops" is stale from revision 1.** The player trait that shipped
in Step 1 (`api/groups/api/src/lib.rs`) has nine: `create`, `list_mine`, `members`,
`pending`, `join`, `leave`, `invite`, `respond`, `decide` — the count *Review response*
item 1/2 already corrected in the trait section above was never carried into Step 2's
own text.

**3 — "There is no red window there" (just above) is false.** Commit `b9b942448` (which
registered `group.*` in `topiccheck::defined_topics()` and gave it audit sinks) titles
itself "Closes the contract-surface half of the red window commit 7c7ee12 opened" — a
direct admission that landing the contract crate in Step 1 without a matching topiccheck
entry did leave a red window, not merely an unsubscribed-but-green topic. A related but
distinct red window (five gates keyed on `api/<domain>` rather than `modules/<domain>`,
so a contract crate landing ahead of its module made a *hypothetical* `modules/groups`
turn all five red) is what commits `a5c8398`/`6c09bea` closed by splitting the served
surface from the contract surface and introducing the `CONTRACT_ONLY` exemption list
(`tools/rpc-contract-model`). Both are corrections to this plan's Step 1 sequencing, not
new drift at HEAD.

**4 — The non-goal "A swept invitation emits nothing" is reversed.** Adversarial review of
Step 5 (`fdc59fb`) rejected it: unlike `mail`'s prune, which removes tombstones, this sweep
removes LIVE relations a player can still act on, so the ledger would hold an invite that
never ended while `audit`'s `group.member_left` sink stayed silent. The sweep now emits
`group.member_left` per swept row inside the delivery transaction, with the new
`groupsevents::REASON_EXPIRED` and an EMPTY `actor_id` (no party ended it). Additive: the
`reason` vocabulary is open by contract. The `notifications` argument in the original
bullet is unaffected — no inbox row is produced for it here.
