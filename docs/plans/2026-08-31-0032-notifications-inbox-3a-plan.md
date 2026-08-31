# Notifications — in-app inbox (seq #3a)

**Status:** revision 2 — adversarial review applied (see *Review errata* at the end)
**Tracker row:** #3, split by this plan into **#3a** (in-app inbox — this document),
**#3b** (outbound email channel), **#3c** (push, FCM/APNs).
**Scope decided with the owner 2026-08-31:** in-app inbox only. The outbound mail
channel — the hard prerequisite for seq #4 — is explicitly NOT in this rollout.

---

## Context — why a new module, and why not extend an existing one

The repo's rule is that a plan adding a module must first justify not extending the
overlapping systems. The candidates, and why each is not the home for an inbox:

- **`audit`** — the closest structural twin: it already fans in from 8 durable topics
  into one append-only table via `on_tx_raw`. But audit is a *zero-coupling operator
  ledger*: raw JSON, no player ownership, no read state, no player-facing op, and
  deliberately no typed payload knowledge. An inbox is the opposite on every axis —
  typed per-topic rendering, per-player ownership authz, mutable read state, and a
  player-facing contract. Extending audit would mean giving the ledger a player face
  and a mutable column, which destroys the property that makes it trustworthy.
- **`accounts`** — owns `player_id` and is where a "messages for this player" table
  might superficially belong. Rejected: accounts is the identity authority, and the
  inbox consumes `wallet.changed`, which would make the identity module depend on the
  economy's event contract. Wrong direction; accounts must stay a leaf that others
  consume.
- **`config` / `core/invalidation`** — not a candidate for delivery (freshness, not
  delivery), noted only to record that it was considered and is the wrong plane.
- **`admin`** — hosts the operator UI, but an operator-sent message is *player-owned
  durable state*, not portal state. Admin stays domain-agnostic; the page for sending
  mail is a contributed `adminapi::Item`, exactly like wallet's, not new admin code
  ([[generic-remote-admin-write-seam]]).

So: a new fortress, `notifications`, the **13th domain module**. It is the first module
whose primary job is to *consume* other modules' durable events — which is precisely
why it is worth building: it exercises the consumer half of the bus seam the way
`characters`→`inventory` exercises the sync-capability half.

### Decisions taken with the owner before this plan (all three change the contract)

1. **Pagination: cursor in the request body, POST.** Nothing in the repo paginates
   today, and the `#[http(...)]` grammar has **no query-parameter source** — only
   `path_args` and body fields (`tools/rpc-macro/src/lib.rs:75-90`; `ArgSource` in
   `core/opsapi/src/lib.rs`). An inbox is the first list that grows without bound per
   player, so the hard-cap shape (`leaderboardapi::Leaderboard::top_scores`, capped at
   100 *in the impl*) is not acceptable — `docs/reference/module-reference.md` names
   "hard-ceiling list endpoints with no cursor/pagination" in its *do NOT copy* list.
   Extending the macro grammar with a query source plus a shared `Page<T>` is the
   authority-level fix, but it touches `rpc-macro`, `route_bindings`/`databind.rs`,
   `opscatalog`, and the public-api baseline — its own rollout, not a sub-task of this
   one. **This plan puts the cursor in the request body of a POST op**, which is
   expressible in the grammar as it stands and needs zero macro work.
2. **Fan-in sources: operator mail + two durable topics.**
3. **Lifecycle: player marks read, player deletes, scheduler prunes** the remainder.

### Correction to the fan-in pair proposed when decision 2 was put to the owner

The options offered named `wallet.changed` **and `match.finished`**. `match.finished`
is wrong and this plan does not use it: its payload is
`Finished { match_id: String, winner: String, loser: String }`
(`api/match/events/src/lib.rs`) and those two fields are **opaque contestant strings,
not `player_id`s** — `tools/splitproof` reports a match with `Winner: "alice"`, and
`rating`/`leaderboard` treat them as bare keys. An inbox row must be addressed to a
`player_id`, so `match.finished` cannot address one without match first carrying player
ids, which is a change to a published payload shape (a new contract version) and out of
scope here.

**Substituted second topic: `player.promoted`** — `PlayerPromoted { player_id,
from_provider, to_provider }`, a real `player_id`, and it produces the single most
useful first inbox row (a welcome message when a guest becomes a real account). Both
chosen topics therefore carry a `player_id` in their payload:

| Topic | Payload field used | Notification produced |
|---|---|---|
| `wallet.changed` | `player_id`, `currency`, `delta`, `balance_after`, `reason` | `kind = "wallet.credit"`, only when `delta > 0` |
| `player.promoted` | `player_id`, `to_provider` | `kind = "account.promoted"` (welcome) |

Recorded as a **known gap**: match results produce no inbox row until `match.finished`
carries player ids. Do not paper over it with a name→player lookup.

---

## Contract shape (settled here, not during implementation)

`api/notifications/api/src/lib.rs`, crate `notificationsapi`:

```rust
pub const MAX_TITLE_BYTES: usize = 200;
pub const MAX_BODY_BYTES: usize = 4000;
pub const MAX_KIND_BYTES: usize = 64;
pub const MAX_CURSOR_BYTES: usize = 128;
pub const MAX_PAGE_LIMIT: i64 = 100;
pub const DEFAULT_PAGE_LIMIT: i64 = 25;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notification {
    pub id: String, pub kind: String, pub title: String, pub body: String,
    pub created_at: String,
    /// RFC3339 read timestamp; the EMPTY STRING means unread. Not a `bool`:
    /// `csharp-client-gen`'s type lattice does not model `bool` on a player-facing
    /// surface, and the column is a nullable timestamp anyway.
    pub read_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page { pub items: Vec<Notification>, pub next_cursor: String }

#[rpc(prefix = "notifications")]
#[async_trait]
pub trait Player: Send + Sync {
    #[http(verb = "POST", path = "/notifications/list", auth = "player", success = 200)]
    #[retry_safe]
    async fn list(&self, identity: Identity, cursor: String, limit: i64) -> Result<Page, Error>;

    #[http(verb = "POST", path = "/notifications/{id}/read", auth = "player",
           success = 204, path_args(notification_id = "id"))]
    #[retry_safe]
    async fn mark_read(&self, identity: Identity, notification_id: String) -> Result<(), Error>;

    #[http(verb = "DELETE", path = "/notifications/{id}", auth = "player",
           success = 204, path_args(notification_id = "id"))]
    async fn delete(&self, identity: Identity, notification_id: String) -> Result<(), Error>;
}
```

Deliberate choices, each with its reason:

- **Scalars are `i64`/`String` only — never `u32` or `bool` on this surface.**
  `tools/csharp-client-gen/src/scrape.rs:396-398` lists both in `UNMODELLED_SCALARS`,
  and every `#[http]`-reachable DTO field is mapped through that lattice, so a `u32`
  arg or a `bool` field makes `cargo run -p csharp-client-gen` `bail!` — inside the
  **blocking** `codegen-freshness` stage. `limit: i64` follows `inventory.grant`'s
  `qty: i64`; unread is the empty `read_at`, not a `bool`. A negative `limit` is
  `opsapi::Status::Invalid`.
- **No `Option<T>` in any wire arg.** Empty `cursor` means first page; `limit == 0`
  means `DEFAULT_PAGE_LIMIT`; `limit > MAX_PAGE_LIMIT` clamps. This follows
  `charactersapi::Player::create`'s empty-`class`-defaults-in-the-impl precedent and
  avoids depending on `Option` support in the macro, which nothing in the repo
  currently exercises on a wire arg.
- **`list` and `mark_read` are `#[retry_safe]`, `delete` is not.** `list` is a read.
  `mark_read` writes `read_at = COALESCE(read_at, now())`, so a replay returns the same
  state. `delete` is not idempotent — a replay after a successful delete is a
  `NotFound`, exactly like `charactersapi::Player::delete`, which is also not
  `#[retry_safe]`.
- **No `<name>events` crate.** This module publishes nothing in #3a. It consumes only.
- **No sync capability, so `requires()` is empty.** Retention is read from
  `NOTIFICATIONS_RETENTION_DAYS` (default 30), matching `audit`'s
  `AUDIT_RETENTION_DAYS`, so there is no `dyn Config` dependency and
  `cmd/notifications-svc` needs no `remote::Stub` other than what the metrics module
  brings. Operator mail is sent through the admin submit path (local closure in the
  monolith, `admin.adminSubmit` over the edge in the split), exactly as wallet does —
  it needs no wire trait of its own.

### Schema (`modules/notifications/src/lib.rs`, `SCHEMA_DDL`)

```sql
CREATE SCHEMA IF NOT EXISTS notifications;
CREATE TABLE IF NOT EXISTS notifications.messages (
    id             uuid        PRIMARY KEY,
    player_id      uuid        NOT NULL,
    kind           text        NOT NULL,
    title          text        NOT NULL,
    body           text        NOT NULL,
    created_at     timestamptz NOT NULL DEFAULT now(),
    read_at        timestamptz,
    source_event_id text,
    CONSTRAINT notifications_title_len_check CHECK (octet_length(title) <= 200),
    CONSTRAINT notifications_body_len_check  CHECK (octet_length(body)  <= 4000),
    CONSTRAINT notifications_kind_len_check  CHECK (octet_length(kind)  <= 64)
);
CREATE UNIQUE INDEX IF NOT EXISTS notifications_source_event_idx
    ON notifications.messages (source_event_id) WHERE source_event_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS notifications_inbox_idx
    ON notifications.messages (player_id, created_at DESC, id DESC);
```

The CHECK constraint names end in `_len_check` because both precedent modules'
anti-drift tests filter on that suffix (Step 8). `octet_length`, not `char_length`, so the CHECK
matches Rust's `str::len()` — wallet's convention. The partial unique index on `source_event_id` is a **belt**: durable
delivery is already exactly-once for a `TransactionalPg` consumer, but the index is
what makes an operator re-drive (`eventctl`) safe rather than duplicating a player's
inbox. `player_id` is a plain id column — no cross-module FK — and it is `uuid`, not
`text`: every sibling that keys on a player (`accounts.players.id`,
`wallet.balances`/`ledger`, `characters.characters`, `inventory.holdings`) is `uuid`
with a bound `::uuid` param, which folds uppercase/braced/unhyphenated spellings of one
id into a single value, so an operator-entered id in Step 4's send-mail form cannot
silently create a row its owner can never see, and a genuine typo becomes a loud
22P02 instead of an orphan row.

**Cursor:** keyset on `(created_at DESC, id DESC)`, encoded opaque as base64url of
`"{created_at_rfc3339}|{uuid}"`, capped at `MAX_CURSOR_BYTES`. A malformed or
over-length cursor is `opsapi::Status::Invalid` (400), never a silent reset to page 1 —
a silent reset makes a paging bug invisible. Keyset, not OFFSET, because rows are
deleted underneath a paging client.

---

## Steps

Every step names its files. Tests are their own steps, after the implementation they
cover has landed and compiled ([[split-impl-and-tests]]).

**Note on build state, Steps 1–6:** three blocking stages key off `api/<name>/api`
**on disk**, not off the module or the svc — `fortress`/`archcheck` rule 17
(`http_op_domains`, `tools/archcheck/src/main.rs:768-816`), `contract-golden`'s
`self_check_rpc_list` (`tools/topiccheck/src/golden.rs:513-534`), and
`codegen-freshness`'s `check_completeness` (`tools/csharp-client-gen/src/scrape.rs:183-201`)
plus its own `self_check_rpc_list` in `tools/opscatalog-gen/src/main.rs`. All three go
red the moment Step 1 lands `notificationsapi`'s `#[http(` methods, not when Step 5
adds the svc or Step 6 wires the hand-maintained lists. This is a **sanctioned broken
intermediate build**: the tree is red on these three blocking stages from Step 1 until
Step 6 completes, and running `verifyctl` in between is expected to fail and is not a
signal to fix anything early.

### Step 1 — contract crates `notificationsapi` + `notificationsrpc`  `[opus]`

**(a) What:** new `api/notifications/api/{Cargo.toml,src/lib.rs}` and
`api/notifications/rpc/{Cargo.toml,src/lib.rs}`; workspace `members` + dep aliases in
the root `Cargo.toml`.
**(b) Why now:** every later step compiles against these types; the `#[rpc]` macro also
generates the `notifications_player_meta!` macro the rpc crate invokes, so nothing can
be written before the trait exists.
**(c) How:** the api crate is the block under "Contract shape" above, verbatim
**including the `#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]` on
both DTOs** — the generated client does `serde_json::from_value::<Page>` and the server
adapter `to_value(&Page)`, so a missing derive is a compile error on a
public-api-baselined surface. Traits, DTOs, and the `MAX_*` consts, transport-free (deps: `opsapi`, `rpc-macro`,
`async-trait`, `serde`, `serde_json`). The rpc crate is the one-liner
`notificationsapi::notifications_player_meta!(rpc_macro::generate_glue);` plus
`pub use adminrpc::{register_admin, register_admin_submit};`. Do **not** add
`remote_factories()` — nothing consumes this module's capability remotely (wallet's rpc
crate omits it for the same reason); the gateway reaches it through `PEER_SLOT`, not a
capability stub.
**(d) Lane:** `[opus]` — this is the public contract surface, pinned by the public-api
baseline; getting it wrong is a contract migration later.

### Step 2 — module skeleton: store, service, the three player ops  `[opus]`

**(a) What:** `modules/notifications/{Cargo.toml,src/lib.rs,src/store.rs,src/service.rs}`.
**(b) Why now:** the ops must exist and compile before the event handlers (Step 3) can
share the insert path, and before the admin page (Step 4) can call the service.
**(c) How:** `lib.rs` holds `NotificationsModule`, `SCHEMA_DDL` (above), and the
`lifecycle::Module` impl — `name() = "notifications"`, `requires() = vec![]`,
`register` provides `dyn Player` under `registry::key("notifications", "player")`,
`migrate` runs `sqlx::raw_sql(SCHEMA_DDL)`, `init` contributes the op set
(`opsapi::{SLOT, BINDING_SLOT, LOCAL_SLOT}` from
`notificationsapi::player_rpc::operations(svc.clone())`), `edge::EDGE_SLOT` with an
`EdgeReg` registering **only** `player_rpc::register_server`, and
`opsapi::DESCRIBE_SLOT`. The `register_admin`/`register_admin_submit` registrations
belong to Step 4, NOT here: `adminrpc::register_admin` takes `Arc<dyn AdminData>` and
`register_admin_submit` takes `Arc<dyn AdminSubmit>` (`api/admin/rpc/src/lib.rs:34,47`),
and neither impl exists until Step 4 — registering them now forces a placeholder impl
that Step 4 would have to delete, which is an old rail living across two steps. No `start`/`stop` — no background
work. `store.rs` takes `&mut PgConnection` for writes and the pool for reads (wallet's
split), and owns the keyset query:
`SELECT … WHERE player_id = $1 AND (created_at, id) < ($2, $3) ORDER BY created_at DESC, id DESC LIMIT $4+1`
— fetch `limit + 1` rows to decide `next_cursor` without a second COUNT.
`service.rs` implements `notificationsapi::Player`: **every op filters by
`identity.player_id()`**, and a row belonging to another player is `Status::NotFound`
(404), never `Forbidden` — a 403 tells the caller the id exists, which is an
enumeration oracle over another player's inbox. `mark_read` uses
`SET read_at = COALESCE(read_at, now())`. The cursor codec (encode/decode, byte cap,
`Status::Invalid` on malformed) lives here as a pure function, so Step 8 can test it
with no I/O.
**(d) Lane:** `[opus]` — the authz filter and the keyset predicate are the two places
this module can be silently wrong.

### Step 3 — durable fan-in: two subscriptions + prune  `[opus]`

**(a) What:** `modules/notifications/src/projection.rs`; subscription wiring added to
`init` in `modules/notifications/src/lib.rs`; one const added to
`api/scheduler/events/src/lib.rs` (`schedule_names::NOTIFICATIONS_PRUNE`), one
bootstrap row added to `modules/scheduler/src/lib.rs`'s `SCHEMA_DDL`, and
`NOTIFICATIONS_PRUNE` added to the hardcoded array in
`modules/scheduler/src/tests.rs`'s `seeded_schedule_names_are_contract`.
**(b) Why now:** it needs the insert path from Step 2; it must precede the admin page
so the page has rows to render.
**(c) How:** three independent subscriptions, each with its own checkpoint, registered
in `init` (the `audit` shape — nine independent subs, never one multiplexed handler):

| Subscription id | Topic | Start |
|---|---|---|
| `notifications.wallet-changed.v1` | `walletevents::CHANGED` | `AfterRegistration` |
| `notifications.player-promoted.v1` | `accountsevents::PLAYER_PROMOTED` | `AfterRegistration` |
| `notifications.prune-on-scheduler.v1` | `schedulerevents::FIRED.topic()` (raw) | `Genesis` |

The first two are typed `on_tx`; the prune one is `on_tx_raw`, deserializing only
`{name: String}` and no-opping unless `name == NOTIFICATIONS_PRUNE`, copied from
`modules/audit`'s `PruneHandler`. `AfterRegistration`, not `Genesis`, for the two
content topics: `Genesis` would replay the entire retained history into every existing
player's inbox on first boot. Each handler inserts through Step 2's store on the
handed `Delivery::tx.downcast::<PgConnection>()`, so the row and the checkpoint commit
together. **The insert is
`INSERT … ON CONFLICT (source_event_id) WHERE source_event_id IS NOT NULL DO NOTHING`** —
both halves matter. Without `ON CONFLICT`, an operator re-drive raises 23505, the
handler returns `Err`, and the plane backs off and **pauses the subscription**, taking
the inbox offline for every player rather than skipping one duplicated row — the exact
opposite of the belt this index is for. Without the repeated `WHERE` clause, Postgres
refuses to infer a *partial* unique index at runtime ("no unique or exclusion constraint
matching the ON CONFLICT specification"). **A data-quality rejection returns `Ok(())`, not `Err`** (wallet's precedent):
a `wallet.changed` with `delta <= 0` is skipped, and skipping must not back off and
pause the subscription. Step 2's shared insert path (`Service::deliver_on`) returns
`Err` for BOTH an input rejection and an infra failure — it does not hand the
handler two distinguishable outcomes — so a durable handler here must inspect the
`Err` and map an input rejection (`opsapi::Status::Invalid`) to `Ok(())` itself
rather than propagate it; propagating it would back off and pause the subscription,
which is exactly the failure this rule forbids. The scheduler edit is the one deliberate touch to an existing
module and follows the established `audit-prune` seam exactly — the name is a shared
const in `schedulerevents`, the seed is an idempotent bootstrap row in scheduler's DDL,
and the linking test lives in **scheduler's own** `src/tests.rs`
(`seeded_schedule_names_are_contract`, whose array is hardcoded), never in
notifications' — `SCHEMA_DDL` is a private static in `modules/scheduler/src/lib.rs`, and
notifications importing scheduler would be a module→module edge `archcheck` rejects.
**(d) Lane:** `[opus]`, via `core-implementer` — this is bus-seam work and the
`AfterRegistration`-vs-`Genesis` choice is exactly the kind of authority decision that
rule set exists for.

### Step 4 — admin page: inbox table + operator send-mail form  `[opus]`

**(a) What:** `modules/notifications/src/admin.rs`; the `adminapi::SLOT` contribution
and the `register_admin`/`register_admin_submit` lines inside the `EdgeReg` in `init`
(deferred here from Step 2); `tools/conformance/src/checks.rs:111`
(`ADMIN_SUBMIT_MODULES`).
**(b) Why now:** it is how an operator sends a 1:1 message — the second half of the
feature — and it needs the service from Step 2.
**(c) How:** follow `modules/wallet/src/admin.rs` structurally: a shared
`build_content(svc, params) -> Content` used by BOTH the local `RenderFn` and the
`adminapi::AdminData` impl, so the page cannot diverge between topologies; an
`apply_submit` behind `adminapi::AdminSubmit` with a single `_action` discriminant
(`send-mail`) and typed `Field`s (player id, title, body); an idempotency key minted
into a hidden field at render time (`mint_idempotency_key`, wallet's `OsRng` helper) so
a double-submit does not send twice. Section: **`"Player Support"`, a new sidebar
section** — the existing sections are Platform / Identity / Game Content / Economy &
Store, and mail belongs to none of them (`Identity` is the identity authority, not a
messaging surface). Layout follows the Players page shape in
`UILayout/GameOps Admin.dc.html` — a filterable table plus a drill-down modal — using
the shipped `adminapi::Table`/`ContextHeader`, not a new widget. **There is no inbox
mockup**; that is a declared deviation, not an invention: no artboard exists to copy, so
the page reuses the mockup's Players shape and nothing beyond it. Also contribute an
`ExtensionEntry` "View Inbox" onto `accountsapi::admin::PLAYERS_ROW_MENU`, mirroring
wallet's "View Wallet", linked as `notifications?player={id}`.
`ADMIN_SUBMIT_MODULES = &["apikeys", "wallet"]` is **diffed against `modules/*/src`**
by `checks::admin_submit_findings` before any assertion runs, so the blocking
conformance stage goes red the moment `impl AdminSubmit for Service` lands — add
`"notifications"` in the same step that adds the impl, and widen the
`admin.adminSubmit params.<value>` basis prose in `policy.rs`, which currently claims
something about every implementor while naming only wallet and apikeys.
**(d) Lane:** `[opus]` — UI is never a mechanical lane ([[follow-uilayout-mockup-faithfully]]).

### Step 5 — `cmd/notifications-svc` + registration in every composition root  `[sonnet]`

**(a) What:** `cmd/notifications-svc/{Cargo.toml,src/lib.rs,src/main.rs}`;
`cmd/server/src/lib.rs` (register the module); `cmd/gateway-svc/src/lib.rs` (a
`remote::Stub::describe_peer("notifications", …)` for `PEER_SLOT`); `cmd/admin-svc/src/lib.rs`
(`admin_stub("notifications", wiring, "127.0.0.1:9011")`);
`tools/checkmodules/{Cargo.toml,src/lib.rs}` (Split-profile entry); root `Cargo.toml`.
**(b) Why now:** the fortress rule and `archcheck` rule 12 make the svc mandatory the
moment `modules/notifications` exists (Step 2). `archcheck` rule 17 has been failing
the build since Step 1 — it scans `api/<name>/api/src` directly and does not wait for
the svc — so this step is what STOPS it failing, by finally adding the gateway stub,
not what starts it.
**(c) How:** `lib.rs` is `modules(wiring: &ProcessWiring) -> Vec<Box<dyn Module>>`
returning `metrics::Metrics::new()` + `NotificationsModule::new()` and **no** other
stub (nothing is consumed). The gateway entry is
`remote::Stub::describe_peer("notifications", edge_peer(wiring, edge_list_resolver,
"notifications", "127.0.0.1:9011"))` — **`describe_peer`, never `Stub::new`**:
`Stub::new` with an empty factory list `anyhow::bail!`s in `register`
(`core/remote/src/lib.rs:1451`) and gateway-svc fails to boot. Wallet, the identical
pure-HTTP case, uses `describe_peer` for exactly this reason
(`cmd/gateway-svc/src/lib.rs:108-117`). `archcheck` rule 17 accepts either form, so
nothing but a live boot catches the wrong one. `main.rs` mirrors `cmd/wallet-svc/src/main.rs`: build
`ProcessWiring`, an `Arc<Mutex<edge::Server::new()>>`, then
`app::run(Config::from_env(), mods, Some(edge_server), None)` — the trailing `None` is
the player-QUIC front, which only gateway-svc and the monolith pass.
**(d) Lane:** `[sonnet]` — fully specified, N-similar edits against existing templates.

### Step 6 — the hand-maintained lists a 13th service breaks  `[sonnet]`

**(a) What:** `tools/processctl/src/fleet.rs` (the service entry, plus
`notifications` added to the `gateway` and `admin` dependency lists and their `peer()`
tables); `tools/processctl/src/fleet_tests.rs`; `modules/apikeys/src/lib.rs`
(`DEV_CLIENT_POLICY`); `tools/opscatalog-gen/src/main.rs` (`rpc_modules()` + Cargo dep)
and the regenerated `opscatalog/src/generated.rs`; `tools/topiccheck/src/golden.rs`
(`rpc_modules()`); `tools/csharp-client-gen/src/scrape.rs` (`PROVIDERS` at `:36`,
`phase_a()` at `:42-51`, + Cargo dep) and the committed `clients/csharp/Generated`
tree; `docs/reference/contract-golden/` via `--bless-contract-golden`.
**(b) Why now:** `contract-golden` and `codegen-freshness` have been failing since
Step 1 (both key off `api/<name>/api` on disk, not off the svc); the `processctl`
fleet entry and its fleet-drift preflight are the ones that specifically require
Step 5's `cmd/notifications-svc` to exist on disk first. All of them are independent
of the module's logic.
**(c) How:** ports **8093 / 9011** (next free after wallet's 8092/9010);
`let notifications = service("notifications-svc", 8093, Some(9011), vec![]);` — no
dependencies, since `requires()` is empty; `overrideable_env = &["NOTIFICATIONS_RETENTION_DAYS"]`.
`pool_budget` is not passed because the `service(...)` closure defaults it
(`fleet.rs:551-554`). The entry must be pushed into the final `FleetSpec::new(vec![…])`
**before** `gateway` and `admin`, which now list it as a dependency — `FleetSpec::new`
rejects `DependencyNotEarlier` (`fleet.rs:704-706`). Record the session arithmetic in
the itemized budget table at `fleet.rs:70-110` rather than nudging a bare number: the
proof fleet reserves 73 of `PG_SESSION_BUDGET` = 81 today, and notifications' +6 takes
it to **79/81** — two sessions of headroom, which is worth stating because
`SPLITPROOF_REPLICA_SESSIONS` and `HARNESS_RESERVE` are hand-estimated.
`fleet_tests.rs::proof_fleet_is_the_canonical_thirteen_service_snapshot` is the **one
list in this repo with no drift self-check** — a hardcoded 13-tuple `assert_eq!` that
fails with a raw mismatch and no per-service hint. Extend it to 14 and rename it
(`…_fourteen_service_snapshot`), **and in the same edit replace the bare `assert_eq!`
with a per-service named diff** (the shape `FleetSpec::validate_names` already uses):
re-inscribing a drift-blind list for the 14th service is precisely the failure this
repo has already paid for ([[didnt-forget-scripts-must-self-check]]), and "it is
already a known gap" is a cost argument, not an answer. Deriving the whole snapshot
from `FleetSpec` would make the test tautological, so the list stays hand-written —
only its failure diagnostic changes. Add
`notifications.list,notifications.markRead,notifications.delete` to `DEV_CLIENT_POLICY`
so the dev client key can call them.

**Three generators carry a hand-written module list with a self-check that kills the run
before it generates anything** — each needs its list entry *and* a Cargo dep on
`notificationsapi` before the generator is even worth invoking:
`tools/opscatalog-gen/src/main.rs`'s `rpc_modules()` (self-check at `:212-231`),
`tools/topiccheck/src/golden.rs`'s `rpc_modules()` (`self_check_rpc_list`, `:513-534`,
behind the blocking `contract-golden` stage), and `tools/csharp-client-gen`'s
`PROVIDERS`/`phase_a()` (completeness gate at `scrape.rs:194-199`: *"provider(s) expose
#[http] methods but are not in the hardcoded PROVIDERS list"*, behind the blocking
`codegen-freshness` stage, which also byte-diffs the committed `clients/csharp/Generated`
tree — so the regenerated C# must be committed too).

Then the three blesses, all of them: `--bless-public-api` (the baseline itself
self-discovers `api/*/{api,events}` from disk, so only the generated `.txt` is
committed), `--bless-contract-golden` (notifications' `route_bindings`/`wire_ops`/
`body_shapes`/`describe` values), and — in Step 7 — `--bless-input-golden`.
**(d) Lane:** `[sonnet]` — mechanical, but every item is named here so nothing is left
to discovery.

### Step 7 — conformance: caps that actually execute  `[opus]`

**(a) What:** `modules/notifications/src/conformance.rs`;
`tools/conformance/src/policy.rs` (an `Entry` in `entries()` + rows in
`input_policies()`); `cargo run -p verifyctl -- --bless-input-golden`.
**(b) Why now:** `checks::drift_findings` three-way diffs `modules/*` on disk against
`entries()` and each `Module::name()`, so conformance goes red the moment Step 2
lands — and the input inventory auto-discovers this module's String wire fields.
**(c) How:** the 2026-08-30 rollout established that `InputPolicy::Validated { basis }`
is **prose nothing executes** — a proof audit deleted both input caps from accounts'
`link` handler and conformance still printed OK. So each cap gets a real probe:
`conformance.rs` exposes `conformance_title_rejected(len) -> bool`,
`conformance_body_rejected`, `conformance_kind_rejected`, `conformance_cursor_rejected`,
each **calling the production validator** (mirroring
`modules/apikeys/src/conformance.rs`), and the `Entry` carries
`Convention::InputByteCaps` with a `CapCase` per probe wired to the matching
`notificationsapi::MAX_*_BYTES`. The three input keys the inventory will discover are
`notifications.list/cursor`, `notifications.markRead/notification_id`, and
`notifications.delete/notification_id`; `cursor` is `Validated { cap: MAX_CURSOR_BYTES,
basis }` naming the codec function, and the two `notification_id`s are `Opaque` with the
`characters.delete` uuid rationale. The other conventions get `NotApplicable` with a
concrete reason each: `EnvValidation` (retention is a parsed integer with a default,
name the fallback), `InfraOutage503` (no external verifier), `ArgonParity` (no password
hashing).
**(d) Lane:** `[opus]` — this is the gate that has lied twice; it is not mechanical.

### Step 8 — unit tests  `[test-author]`, `model: "sonnet"`

**(a) What:** `modules/notifications/src/tests.rs` (separate file, never inline).
**(b) Why now:** Steps 2–4 and 7 have landed and compile; this is the first step that
can start from a real diff.
**(c) How:** cover the branches that were previously wrong or are newly at risk —
each test must execute the branch, not sit near it:
- **cursor codec** — round-trip; malformed base64 → `Status::Invalid`; over-`MAX_CURSOR_BYTES`
  → `Status::Invalid`; and explicitly **not** a silent reset to page 1.
- **keyset paging** — insert N rows with colliding `created_at`, page through with
  `limit` smaller than N, assert every row is seen exactly once and `next_cursor` is
  empty only on the last page (the tie-break on `id` is the part that breaks first).
- **ownership authz negative** — player B's `mark_read`/`delete` on player A's row
  returns `NotFound`, and A's row is unchanged afterwards. This is the enumeration
  oracle; prove the 404, not merely "an error".
- **`mark_read` idempotence** — twice in a row keeps the first `read_at`.
- **fan-in skip** — a `wallet.changed` with `delta <= 0` returns `Ok(())` and inserts
  nothing (the guard clause must be reachable from a real delivery, not just called
  directly — the 2026-08-30 guest-skip finding was exactly an unreachable-looking guard
  that no test executed).
- **source-event dedup** — the same `event_id` delivered twice yields one row **and
  the second delivery returns `Ok(())` with the subscription still unpaused**. The
  row-count assertion alone would pass against the broken `Err`-on-23505 shape that
  takes every player's inbox offline; the liveness half is the point.
- **cap ↔ DDL anti-drift, both directions** — every `_len_check` CHECK in `SCHEMA_DDL` maps
  to a `notificationsapi::MAX_*_BYTES` const and vice versa, copying
  `modules/wallet/src/tests.rs`'s `every_len_check_in_the_ddl_is_mapped_by_a_catalog_cap`
  and its apikeys twin. Without it: bump `MAX_BODY_BYTES` to 8000 in the api crate, the
  Rust validator accepts 6000 bytes, the CHECK rejects it as an unmapped 23514, and the
  operator gets a 500 where a 400 belongs.
- **prune** — a row older than retention goes, a newer one stays; a `scheduler.fired`
  with a foreign `name` prunes nothing.
Run tests per the `safe-verification` skill — one rollout at a time on the shared Postgres.
**(d) Lane:** `[test-author]` at `model: "sonnet"` — the harness is conventional; the
implementation has already landed.

### Step 9 — split-proof assertions  `[test-author]`, `model: "opus"`

**(a) What:** `tools/splitproof/src/main.rs` — named assertions `[NT1]`–`[NT5]` plus
monolith parity `[NT1m]`/`[NT4m]`.
**(b) Why now:** it is the gate that makes the feature *done* — monolith-only is not
done ([[never-monolith-only-features]]) — and it needs the fleet entry from Step 6.
**(c) How:** follow wallet's `[WL1]`–`[WL7]` shape exactly:
- `[NT1]` — `POST {gateway}/notifications/list` with `X-Api-Key: dev-key-client` +
  Bearer returns 200 and an empty page for a fresh player.
- `[NT2]` — operator sends mail through `{gateway}/admin/notifications` (gateway
  passthrough → admin-svc session + CSRF → `admin.adminSubmit` over QUIC →
  notifications-svc), then the row is asserted **directly in `notifications.messages`
  via the sqlx pool**, proving the remote submit path.
- `[NT3]` — the player lists it, marks it read, deletes it; the DELETE returns 204 and
  a second DELETE returns 404.
- `[NT4]` — **the cross-process fan-in**: grant currency through the admin wallet page,
  then poll `notifications.messages` for the `wallet.credit` row. This is the assertion
  that actually proves a durable event crossing from wallet-svc to notifications-svc;
  without it the module is unproven in the topology that is at risk.
- `[NT5]` — paging: seed >`DEFAULT_PAGE_LIMIT` rows, walk the cursor to exhaustion
  through the gateway, assert no duplicate and no missing id.
- `[NT6]` — **the second fan-in topic**, which otherwise ships unproven: create a
  guest, link a real identity so `player.promoted` fires, then poll
  `notifications.messages` for the `account.promoted` row. Without it, half the fan-in
  is monolith-only by omission.
- `[NT1m]`/`[NT4m]` — the same two against `cmd/server` on the monolith base URL.
**(d) Lane:** `[test-author]` at `model: "opus"` — a new harness assertion crossing two
processes plus the admin passthrough is not the conventional case, so this step
escalates off the lane's default.

### Step 10 — documentation  `[docs]`, `model: "sonnet"`

**(a) What:** `docs/roadmap/feature-tracker.md` (split row #3 into #3a/#3b/#3c, flip
#3a, bump the `Last update` stamp, add a change-log entry); `CLAUDE.md` (domain-module
list 12 → 13 fortresses, the new module's paragraph, the split-proof port list);
`.agents/shared/gamebackend.md` (the same list — it mirrors CLAUDE.md, plus its
layout comment at `:605`); `README.md:87` ("12 fortresses plus the gateway") **and
`README.md:198`** ("fleet of twelve"); `CLAUDE.md:571` (the layout comment "12
fortresses + gateway"); and two **code comments that will assert behaviour the code no
longer has** — `tools/processctl/src/fleet.rs:147` ("12 DB-backed processes plus
splitproof's `[REPLICAS]` 13th") together with the budget prose at `:79-82`, and
`tools/splitproof/src/main.rs:2608` ("http 8080-8092, edge 9000-9010"). Those last two
are a correctness defect, not a docs nit, and `[docs]` is the only lane that may touch
comment prose. Plus this plan (errata section if anything deviated).
**(b) Why now:** last, against landed code — its job is as much deleting false prose as
adding true prose.
**(c) How:** the counts are the trap. Five separate hand-written service counts went red
on the 13th process during the wallet rollout, and every number going into prose needs a
second authority, never a grep count ([[grep-counts-are-lower-bounds]]) — derive the
fortress count from `modules/*` on disk and the port list from
`tools/processctl/src/fleet.rs`, and state in the tracker that `match.finished` produces
no inbox row and why.
**(d) Lane:** `[docs]` — the only lane that writes prose.

---

## Known gaps this rollout deliberately leaves open

1. **No pagination in the shared grammar.** The cursor lives in one module's request
   body; the next paginating module will copy it rather than share it. The authority-level
   fix (query args + `Page<T>` in `opsapi`) is a separate rollout.
2. **`match.finished` produces no notification** — its payload carries contestant
   strings, not player ids.
3. **No outbound email or push** — #3b and #3c.
4. **No per-player notification preferences / mute** — every fan-in topic notifies
   unconditionally.
5. **`fleet_tests.rs`'s snapshot stays hand-written.** Step 6 gives it a per-service
   named diff so the 15th module gets a hint instead of a raw mismatch, but the list
   itself is still maintained by hand — deriving it wholesale from `FleetSpec` would
   make the test tautological.

## What this rollout removes

**Nothing.** It is purely additive: new contract crates, a new module, a new svc. The
only touches to existing behaviour are one idempotent seed row in scheduler's DDL, one
shared schedule-name const, three generator module lists, one conformance const, and
the composition-root registrations. No old rail is left alive behind a new one, because
there is no old rail — this is the first inbox. Stated explicitly because "what dies?"
is the always-on review question and silence reads as an oversight.

---

## Review errata — adversarial pass, 2026-08-31

One independent `core-reviewer` pass (Opus, think hard) returned **REJECT** on revision
1 with 13 findings. Revision 2 applies all of them. The ones that were build- or
boot-breaking, kept here because the reasoning matters more than the edit:

1. **`u32`/`bool` are in `csharp-client-gen`'s `UNMODELLED_SCALARS`** — revision 1's
   `limit: u32` and `read: bool` would have failed the blocking `codegen-freshness`
   stage. Now `limit: i64` and `read_at: String`.
2. **`Stub::new` with no factories `bail!`s in `register`** — revision 1 would have
   stopped gateway-svc from booting, and `archcheck` accepts both spellings, so only a
   live boot would have caught it. Now `describe_peer`.
3. **The dedup index needed explicit conflict handling** — a bare insert turns an
   operator re-drive into a paused subscription (every player's inbox offline), and
   `ON CONFLICT` cannot infer a *partial* unique index without repeating the `WHERE`.
4. **Four hand-maintained lists were missing** from the "everything a 13th module
   breaks" step, three of them behind self-checks that kill the generator before it
   generates: opscatalog-gen, topiccheck golden, csharp-client-gen. Plus
   `--bless-contract-golden`, unnamed in revision 1, and `ADMIN_SUBMIT_MODULES`.
5. **The schedule-linking test was placed where it cannot exist** — scheduler's
   `SCHEMA_DDL` is private and the import would be a module→module edge. Moved into
   scheduler's own tests.
6. **`player.promoted` shipped with no split assertion**, making half the fan-in
   monolith-only by omission. Added as `[NT6]`.

The reviewer also **disproved one of the plan's own worries**: adding a
`notifications-prune` seed row to scheduler's DDL does take effect on an
already-migrated database, because the seed is a separate
`INSERT … ON CONFLICT (name) DO NOTHING` run on every boot, not part of
`CREATE TABLE IF NOT EXISTS`. No data migration is implied.

Verified clean and left unchanged: the macro grammar (DELETE + `path_args` + 204, a
non-`String` scalar body arg, a struct return, `#[retry_safe]` on a POST), the
`Option`-free claim, route-overlap against `routecheck`, the keyset predicate and its
index, `AfterRegistration` semantics and both topics' retention, `admincheck` needing
no edit, and the public-api baseline's disk discovery.

An adversarial review of Step 1's landed diff found two more factual errors in this
plan itself (not in the diff, which followed the plan correctly):

7. **`opsapi::Status::InvalidArgument` does not exist.** The real variant is
   `opsapi::Status::Invalid` (`core/opsapi/src/lib.rs:154`, mapped to HTTP 400 at
   `:178`). Every occurrence in this plan (the `MAX_CURSOR_BYTES`/negative-`limit`
   prose and the Step 8 test description) is corrected to `opsapi::Status::Invalid`.
8. **Steps 5(b) and 6(b) misdated when the tree goes red, by four steps.** Both said
   the blocking gates fail "as soon as Step 5 lands." In fact three blocking stages —
   `fortress`/`archcheck` rule 17 (`http_op_domains`,
   `tools/archcheck/src/main.rs:768-816`), `contract-golden`'s `self_check_rpc_list`
   (`tools/topiccheck/src/golden.rs:513-534`), and `codegen-freshness`'s
   `check_completeness` (`tools/csharp-client-gen/src/scrape.rs:183-201`) plus its own
   `self_check_rpc_list` in `tools/opscatalog-gen/src/main.rs` — key off
   `api/<name>/api` **on disk**, so they go red the moment Step 1 lands
   `notificationsapi`'s `#[http(` methods, three steps before the svc (Step 5) even
   exists. Corrected the wording in both steps and added a note at the top of
   `## Steps` naming the tree red from Step 1 through Step 6 as a sanctioned broken
   intermediate build, so `verifyctl` before Step 6 is expected to fail and is not a
   signal to fix anything early.

A second adversarial review of Step 2's landed implementation found two more factual
errors in this plan itself:

9. **The plan's own constraint names contradicted the suffix it named as load-bearing.**
   The schema block named the CHECK constraints `notifications_title_len` /
   `_body_len` / `_kind_len`, but the very next paragraph said "the `_len` suffix
   matters — Step 8's anti-drift test filters on it." Both precedent anti-drift tests
   actually filter on `_len_check`
   (`modules/wallet/src/tests.rs:2572`, `modules/apikeys/src/store_tests.rs:634`), and
   both precedent DDLs name their constraints `*_len_check`
   (`modules/wallet/src/lib.rs:58,61,64`; `modules/apikeys/src/lib.rs:72,75,84`).
   Renamed the three constraints to `notifications_title_len_check` /
   `_body_len_check` / `_kind_len_check` and corrected the prose and the Step 8
   bullet to match.
10. **`player_id` was declared `text`, not `uuid`.** Every sibling module that keys on
    a player uses `uuid` with a bound `::uuid` param — `accounts.players.id`
    (`modules/accounts/src/lib.rs:107`), `wallet.balances`/`wallet.ledger`
    (`modules/wallet/src/lib.rs:73,87`), `characters.characters`
    (`modules/characters/src/lib.rs:56`), `inventory.holdings`
    (`modules/inventory/src/lib.rs:76`). Changed the column to `player_id uuid NOT
    NULL` and added the reason to the schema prose: `::uuid` folds distinct
    spellings of one id into a single value, so an operator-entered id in Step 4's
    send-mail form can't silently create a row its owner can never see, and a typo
    becomes a loud `22P02` instead of an orphan row. This does not contradict the
    existing "plain id column — no cross-module FK" point: characters, inventory and
    wallet are all `uuid` with no FK.

Also recorded, not an error: Step 3(c) now states that Step 2's shared insert path
(`Service::deliver_on`) returns `Err` for both an input rejection and an infra
failure — it does not hand the handler two distinguishable outcomes — so a durable
handler must itself map an input rejection to `Ok(())` rather than propagate it,
which would otherwise back off and pause the subscription in violation of the rule
Step 3 already states.
