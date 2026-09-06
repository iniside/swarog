# `groups` — membership, roles and invites (the 16th fortress)

Revision 1 · 2026-09-06 · first half of the chat/groups pair the user asked for in the
Nakama shape. `chat` gets its own plan after this lands, because its channel table's
shape depends on what membership actually ships.

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

**F. `friends` broke a precedent silently and this plan should not inherit it.**
Every other `__describe`-routed module — `wallet`, `notifications` — has a
`<NAME>_EDGE_ADDR` row in `cmd/gateway-svc/src/addrs.rs`'s `ADDR_SPECS`. `friends` has
none; its boot address is a hardcoded literal in `lib.rs` that happens to match
processctl's port choice. Nothing fails, and an operator's env override is silently
ignored in standalone mode. Step 7 adds `groups` **and** closes friends' gap, because
leaving a known twin is what the sweep rule forbids.

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

```rust
#[rpc(prefix = "groups")]
#[async_trait]
pub trait Player: Send + Sync {
    #[http(verb = "POST", path = "/groups", auth = "player", success = 201)]
    async fn create(&self, identity: Identity, name: String, join_policy: String)
        -> Result<GroupSummary, Error>;

    #[http(verb = "POST", path = "/groups/list", auth = "player", success = 200)]
    #[retry_safe]
    async fn list_mine(&self, identity: Identity, cursor: String, limit: i64)
        -> Result<GroupPage, Error>;

    #[http(verb = "POST", path = "/groups/{id}/members/list", auth = "player",
           success = 200, path_args(group_id = "id"))]
    #[retry_safe]
    async fn members(&self, identity: Identity, group_id: String, cursor: String, limit: i64)
        -> Result<MemberPage, Error>;

    #[http(verb = "POST", path = "/groups/{id}/join", auth = "player", success = 200,
           path_args(group_id = "id"))]
    async fn join(&self, identity: Identity, group_id: String) -> Result<MemberSummary, Error>;

    #[http(verb = "POST", path = "/groups/{id}/leave", auth = "player", success = 204,
           path_args(group_id = "id"))]
    async fn leave(&self, identity: Identity, group_id: String) -> Result<(), Error>;

    #[http(verb = "POST", path = "/groups/{id}/invites", auth = "player", success = 201,
           path_args(group_id = "id"))]
    async fn invite(&self, identity: Identity, group_id: String, target_handle: String)
        -> Result<(), Error>;

    #[http(verb = "POST", path = "/groups/{id}/decide", auth = "player", success = 204,
           path_args(group_id = "id"))]
    async fn decide(&self, identity: Identity, group_id: String, subject_id: String,
                    decision: String) -> Result<(), Error>;
}
```

`decide` is the one op that carries an admin's verdict on a pending row —
`decision ∈ {"accept","reject"}` — and it is also how a member is removed (`reject` on a
`member` row is a kick). One op rather than three keeps the authorization check in one
place; the plan deliberately does not ship promote/demote, so the only role transition is
the creator's `admin` at create time.

**None of the mutating ops is `#[retry_safe]`.** A replay of `join`/`leave`/`invite`/
`decide` after an ambiguous failure is indistinguishable from a second, intentional call
on a row the caller may no longer have — the reason `friends::accept` and
`notifications::delete` decline the attribute too. `create` is not retry-safe either: it
mints a fresh group id, so a replay would create a second group. Only the two reads carry
it.

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

Provided under `registry::key("groups", "membership")`. One index probe. It answers
`""` for "not a member", never an error, so a caller cannot distinguish "no such group"
from "not your group" — the same non-oracle discipline the `NotFound` rule enforces on
the player face.

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
        CHECK ((state = 'member' AND role IN ('admin','member')) OR role = '')
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

The `memberships_role_check` constraint is the schema-level statement of the invariant the
contract's consts describe — a `requested` row can never carry a role, so `role_of`
reading a non-empty role is proof of membership without a second predicate.

`memberships_pending_idx` is partial and exists for Step 5's sweep alone; without it the
prune seq-scans a table whose live rows dominate.

### Durable events

`api/groups/events/src/lib.rs`, all `MinRetention { days: 30 }` matching `friends`:

| topic | payload |
|---|---|
| `group.created` | `{group_id, name, creator_id, join_policy}` |
| `group.member_joined` | `{group_id, player_id, role}` |
| `group.member_left` | `{group_id, player_id, reason}` — `reason ∈ {"left","kicked","declined"}` |

`reason` is an open vocabulary on purpose: `friendsevents` records the rule that an
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

**(d) Dispatch.** `[sonnet]` — three crates from three named templates.

## Step 2 — the module: schema, store, service, player ops  `[opus]`

**(a) What.** `modules/groups/{Cargo.toml,src/{lib,store,service,admin_placeholder}.rs}`.

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
- `MAX_MEMBERS` is enforced in the same statement that inserts, not by a count-then-insert.
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
`tools/opscatalog-gen`; `tools/csharp-client-gen`; `modules/apikeys`'s `DEV_CLIENT_POLICY`.

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

## Step 11 — acceptance  `[inline]`

`cargo run -p verifyctl -- --fast`, then `--all --strict`. One rollout at a time: check
`pgrep -x cargo; pgrep -x rustc`, require no active fleet, run exactly one. Redirect and
capture `$?` — a piped `| tail` reports the pipe's status, which produced a false green
twice in this repo. Blessings expected: `--bless-public-api` (`groupsapi.txt`,
`groupsevents.txt`, and `schedulerevents.txt` for the new schedule const),
`--bless-contract-golden`, and `--bless-input-golden` for the admin form's fields. Read
each diff before accepting it.

## Step 12 — documentation  `[docs]`, `model:"sonnet"`

`docs/roadmap/feature-tracker.md` (a new row and a dated decisions entry), `README.md`,
`CLAUDE.md` (the module list, the fortress count, and the split-proof port sentence),
`.agents/shared/gamebackend.md` (which mirrors all three of those claims), and this plan's
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
