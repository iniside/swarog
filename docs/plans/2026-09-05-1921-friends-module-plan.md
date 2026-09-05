# Friends module (seq P0#4) — plan

**Written 2026-09-05-1921. Revision 2 (2026-09-05-2010)** after an independent review
returned "not implementable as written" with 18 findings. Revision 1 is not preserved —
the corrections are recorded in "What revision 2 changed" at the foot.

Feature: `docs/roadmap/feature-tracker.md` → Social → "Friends (add/remove/block,
states)", P0#4.

Scope settled with the user:

- **Presence is session-derived, not socket-derived**, and is exposed as
  `online_until` (RFC3339 string, empty = no live session) — never a boolean named
  `online`. See "Why not socket presence".
- **Discovery is by handle `Name#1234`**, minted by `accounts`. `display_name` alone
  is not unique and never will be.
- **No blocking in v1.**

## Context — the overlapping systems, and why a new fortress

**Why not extend `notifications`.** Structural, not stylistic. `notifications.messages`
has exactly one mutable column (`read_at`) and every mutation carries its owner *inside*
the SQL predicate — `WHERE id = $1 AND player_id = $2::uuid`
(`modules/notifications/src/store.rs:103-150`), with the comment stating ownership is
"never checked afterwards". There is **no second-party write path at all**. A friend
request is inherently a two-player object whose *addressee* must mutate it. Add: no pair
uniqueness (the only unique index is `source_event_id`, a dedup guard), an index built
for "player X's rows in time order" rather than "players related to X", and a TTL prune
correct for messages and wrong for a relation.

*Partly could, and we use that part:* the **announcement** half is a legitimate
notifications consumer. `friends` emits its own durable events; `notifications`
subscribes exactly as it already does for `wallet.changed`.

**Why not extend `accounts` with the whole feature.** `accounts` owns identity. A
friendship graph is a different domain noun with its own lifecycle, admin surface and
retention. But `accounts` **is** the right owner of *player lookup and handles*, which
is why Step 1 extends it narrowly rather than duplicating a name table inside `friends`.

**Why not `mail`'s command-topic shape.** `mail` both defines and subscribes to
`mail.send_requested` because the topic is a *command* addressed to one implementation
(`api/mail/events/src/lib.rs:5-9`). A friend request is a *fact about the friends
domain* that more than one module reacts to, so it takes the ordinary shape: publisher
`friends`, consumers `audit` and `notifications`.

## Why not socket presence (recorded so it is not re-proposed)

Four obstacles, all verified against code by the reviewer:

1. Presence lives in `HubState.by_player` (`modules/gateway/src/push_ws.rs:596`) with
   **no read accessor** — `live()` returns a connection count (`:910`). Per-process RAM.
2. `gateway-svc` has **no database**: `pool_budget: PoolBudget { pool_max: 0,
   dedicated: 0 }` (`tools/processctl/src/fleet.rs:844-846`). It cannot `emit_tx`, and
   topiccheck's `PLANELESS_PROCESSES` independently forbids any durable subscription
   there.
3. The offline transition is decided and emitted inside `impl Drop for Slot`
   (`push_ws.rs:926-971`) so an aborted task still reports. `Drop` cannot `await`. A
   graceful stop announces nothing at all (`:856-859`).
4. No fan-in primitive: `Pool::deliver_all` is `pub(crate)`, returns `usize`, discards
   every response body (`core/remote/src/lib.rs:1090,1113-1119`). *This is the weakest
   of the four — it is "write a new primitive", not "impossible".*

Also: a SIGKILLed front emits no offline, and no splitproof assertion covers more than
one gateway (`[PH5]` is single-process, split-only).

**The session-derived flag's real weakness, stated plainly rather than hidden in a doc
comment:** access tokens live 60 minutes, so a player who quits reads a future
`online_until` for up to an hour, and a player idling with an expired access token but a
live refresh reads empty. That is why the field is a **timestamp, not a boolean** — the
client decides what to render, and the contract does not promise something it cannot
deliver.

## Constraints

1. **A defined topic with no durable subscriber FAILS the blocking gate.**
   `ALLOW_UNSUBSCRIBED` is `&[]` (`tools/topiccheck/src/main.rs:80`); the
   `--durability-strict` leg runs inside `fortress`. Every event ships with its
   consumer in the same rollout. *Verified: `on_tx_raw` records version 1 at the
   transport seam (`core/bus/src/lib.rs:388-392`), so `audit`'s raw subscriptions DO
   satisfy the check — `friend.removed` covered solely by audit is fine, and this holds
   only because the contracts are v1.*
2. **`(id, topic, version, start)` is immutable** — `spec_hash` mismatch fails startup
   (`core/asyncevents/src/catalog.rs:92-107`). Start positions below are decided once.
3. **A payload field cannot be added later** without a v2 contract. Everything a
   consumer needs must be in the v1 payload — see Step 3's denormalization.
4. **No `bool` on any `#[http]`-reachable DTO.** `UNMODELLED_SCALARS` in
   `tools/csharp-client-gen/src/scrape.rs:399-402` makes `bool` a hard error, and
   `codegen-freshness` is blocking. Scalars are `i64` and `String`.
5. **The admin page must be registered in `cmd/admin-svc`** (`src/lib.rs` `admin_stub`
   + `FRIENDS_EDGE_ADDR` default in `src/main.rs`) or it exists only in the monolith.
   No drift check exists for this.
6. Ports: **`:8095` HTTP, `:9014` edge** (`:9013` is `GATEWAY_EDGE_PORT`).

## Hand-maintained authorities

Corrected in revision 2: `checkmodules` is **loud** (`tools/checkmodules/src/tests.rs:6-33`
reads `cmd/` from disk and asserts the list matches), and `routecheck`'s `GATES` is
inapplicable — it lists *gating env vars*, and `friends` introduces none.

| Authority | Entry | Fails |
|---|---|---|
| root `Cargo.toml` members + `workspace.dependencies` | 5 new crates | loud |
| `cmd/server/src/lib.rs` | `Box::new(friends::Friends::new())` | loud |
| `cmd/gateway-svc/src/lib.rs` | `Stub::describe_peer("friends", …)` | loud (archcheck 17 + checkmodules) |
| `cmd/admin-svc/src/lib.rs` + `main.rs` | `admin_stub("friends", …)` + `FRIENDS_EDGE_ADDR` | **SILENT** → monolith-only page |
| `modules/accounts/src/lib.rs` `EDGE_SLOT`/`DESCRIBE_SLOT` | `directory_rpc::register_server` | **SILENT** → split 404s, monolith fine |
| `tools/splitproof/src/main.rs` | `[FR*]` assertions | **SILENT** → no cross-process proof |
| `api/accounts/rpc/src/lib.rs` | `accounts_directory_meta!(rpc_macro::generate_glue);` | loud |
| `tools/checkmodules/src/lib.rs` | `("friends-svc", friends_svc::modules(&w))` | loud |
| `tools/processctl/src/fleet.rs` | `service("friends-svc", 8095, Some(9014), vec!["accounts-svc"])` **plus** `"friends-svc"` in gateway-svc's and admin-svc's `dependencies` and `("FRIENDS", 9014)` in both peer-env loops | loud |
| `weles/fleet.split.toml` | `[[service]]` + the same gateway/admin peer additions | loud |
| `weles/master/src/manifest_tests.rs` | env golden tuple | loud, only under `cargo test -p weles-master` |
| `tools/topiccheck/src/main.rs` `defined_topics()` | × 3 | loud |
| `tools/topiccheck/src/golden.rs` | `event_samples_by_crate()`, `rpc_modules()` | loud (self-check) |
| `tools/opscatalog-gen/src/main.rs:65-84` `rpc_modules()` | a **second, distinct** list — `friendsapi::player_rpc` **and** `accountsapi::directory_rpc` | loud |
| `tools/csharp-client-gen/src/scrape.rs:36` `PROVIDERS` | `friends` | loud |
| `clients/csharp/Generated` + `opscatalog/src/generated.rs` | **regenerate**, not bless | loud (codegen-freshness) |
| `modules/audit/src/lib.rs` | 3 topics + 3 spec ids, same indices | loud (its own tests) |
| `tools/conformance/src/policy.rs` | `friends()` + `input_policies()` rows | loud |

---

## Step 1 — `accounts`: handles + the `Directory` capability `[opus]`

**(a) What.**

`modules/accounts/src/lib.rs` `SCHEMA_DDL`: add `discriminator text NOT NULL` to
`accounts.players` plus
`CREATE UNIQUE INDEX accounts_handle_idx ON accounts.players (lower(display_name), discriminator)`.
Wipe is the migration strategy — DROP SCHEMA and boot fresh, no backfill.

`api/accounts/api/src/lib.rs`:

```rust
pub const MAX_DISPLAY_NAME_BYTES: usize = 128;   // promoted from the private module const
pub const MAX_HANDLE_BYTES: usize = 133;         // name + '#' + 4 digits
pub const MAX_LOOKUP_IDS: usize = 256;

pub struct PlayerSummary {
    pub player_id: String,
    pub display_name: String,
    pub handle: String,        // "Name#1234"
    pub online_until: String,  // RFC3339; empty = no live session
}

#[rpc(prefix = "accounts")]
#[async_trait]
pub trait Directory: Send + Sync {
    /// Unknown ids are OMITTED from the result — a short vector, never an error.
    #[retry_safe]
    async fn players_by_id(&self, ids: Vec<String>) -> Result<Vec<PlayerSummary>, Error>;
    #[retry_safe]
    async fn find_by_handle(&self, handle: String) -> Result<Option<PlayerSummary>, Error>;
}
```

`api/accounts/rpc/src/lib.rs`: `accountsapi::accounts_directory_meta!(rpc_macro::generate_glue);`
and `directory_rpc::provide_remote` in `remote_factories()`.

`modules/accounts/src/lib.rs`: `provide` under `registry::key("accounts","directory")`;
**add `accountsrpc::directory_rpc::register_server(server, svc.clone())` to the
`EDGE_SLOT` closure and concat `directory_rpc::describe()` into `DESCRIBE_SLOT`.**

**(b) Why now.** Friends cannot validate a target, render a list, or answer "which
friends are online" without it. It is the only step touching an existing module.

**(c) How — the non-mechanical parts.**

- **The handle is the whole reason this step exists.** `display_name` has no uniqueness
  today and `register` even defaults it to the caller's email when blank
  (`modules/accounts/src/lib.rs:600-604`). Without a discriminator, Mallory registers as
  `"Alice"` and silently receives invitations meant for the real Alice — an invite hijack
  with no authentication step anywhere.
- Discriminator minting: 4 digits, retried on unique-index violation up to a bounded
  number of attempts (the index is the authority; do not pre-check for freeness, that is
  a TOCTOU). Exhaustion for one name is an error, not a silent fallback. Guests get one
  too — every player is addressable.
- `players_by_id` is **batched** (a friend list needs N names in one call; per-id would
  be N edge round trips in split) and capped at `MAX_LOOKUP_IDS`.
- `online_until` is computed in the **same batch statement**, reusing the existing
  sub-select `EXISTS (SELECT 1 FROM accounts.sessions s WHERE s.player_id = p.id AND
  s.expires_at > now())` (`modules/accounts/src/store.rs:626-631`) — rewritten to
  `max(s.expires_at)` and `WHERE p.id = ANY($1::uuid[])`. One statement, no N+1.
- Both methods are **wire-only** (no `#[http]`): a player must not enumerate the
  directory through the front door.
- **Note the exposure being accepted:** `Directory` answers for *any* player, not only
  the caller's friends. That is deliberate (an invite needs to resolve a stranger) and
  is why the throttle in Step 4 exists.

**(d) Dispatch.** `[opus]` — new public contract on an existing module, a schema change,
and the minting loop.

## Step 2 — tests for Step 1 `[test-author]`, `model:"sonnet"`

Covers landed Step 1. Must exercise: two players with the same `display_name` get
distinct handles and both are resolvable; `find_by_handle` miss returns `None`;
`players_by_id` with mixed known/unknown ids exercises the **omission** branch; the
`MAX_LOOKUP_IDS` cap; the `MAX_DISPLAY_NAME_BYTES` cap; `online_until` is populated for
a live session and **empty** for one whose `expires_at` is in the past (insert the
expired row explicitly — never sleep against a real clock).

## Step 3 — `friends` contracts `[opus]`

**(a) What.** `api/friends/api/` (`friendsapi`), `api/friends/events/` (`friendsevents`),
`api/friends/rpc/` (`friendsrpc`).

Events — three topics, each with a consumer landing in Step 6. **Names are
denormalized into the payload** (constraint 3):

```rust
pub struct Requested { pub edge_id: String, pub requester_id: String, pub requester_handle: String,
                       pub addressee_id: String, pub addressee_handle: String }
pub struct Accepted  { /* same five fields */ }
pub struct Removed   { pub edge_id: String, pub actor_id: String, pub actor_handle: String,
                       pub other_id: String, pub other_handle: String, pub reason: String }

pub static REQUESTED: LazyLock<EventType<Requested>> =
    LazyLock::new(|| define("friend.requested", 1, HistoryPolicy::MinRetention { days: 30 }));
// ACCEPTED, REMOVED likewise
```

`friendsapi` declares its **own** DTO — it does not put an `accountsapi` type on the
friends wire:

```rust
pub const MAX_PAGE_LIMIT: i64 = 100;
pub const DEFAULT_PAGE_LIMIT: i64 = 50;
pub const MAX_CURSOR_BYTES: usize = 256;
pub const MAX_PENDING_OUTSTANDING: i64 = 100;

pub struct Friend { pub player_id: String, pub display_name: String, pub handle: String,
                    pub online_until: String, pub edge_id: String, pub state: String }
```

Ops (`trait Player`), full grammar:

```rust
#[http(verb = "POST", path = "/friends/requests", auth = "player", success = 201)]
async fn request(&self, identity: Identity, target_handle: String) -> Result<Friend, Error>;

#[http(verb = "POST", path = "/friends/requests/{id}/accept", auth = "player",
       success = 204, path_args(edge_id = "id"))]
async fn accept(&self, identity: Identity, edge_id: String) -> Result<(), Error>;

#[http(verb = "POST", path = "/friends/requests/{id}/decline", auth = "player",
       success = 204, path_args(edge_id = "id"))]
async fn decline(&self, identity: Identity, edge_id: String) -> Result<(), Error>;

#[http(verb = "DELETE", path = "/friends/{id}", auth = "player",
       success = 204, path_args(edge_id = "id"))]
async fn remove(&self, identity: Identity, edge_id: String) -> Result<(), Error>;

#[http(verb = "POST", path = "/friends/list", auth = "player", success = 200)]
#[retry_safe]
async fn list(&self, identity: Identity, cursor: String, limit: i64) -> Result<Page, Error>;

#[http(verb = "POST", path = "/friends/requests/list", auth = "player", success = 200)]
#[retry_safe]
async fn pending(&self, identity: Identity, cursor: String, limit: i64) -> Result<Page, Error>;
```

**(b) Why now.** The impl, the svc root and a dozen hand-lists name these types.

**(c) How.**

- **Every path id is the `edge_id`**, never the other player's id — one addressing
  scheme across all four mutating ops.
- `edge_id` in every payload mirrors `walletevents::Changed.ledger_id`: the row is the
  authority, the event is the delivery path, and it is the consumer's idempotency key.
  This matters because **ordering is per-subscription** — a consumer of both `requested`
  and `removed` can see removal first. Carry the id; never assume order.
- **Handles are in the payload because `notifications`' handler cannot resolve them.**
  It runs inside the delivery transaction with no registry access to
  `accounts.directory`, and notifications-svc has no accounts stub. Without this, the
  inbox row reads "3f2a…-… sent you a friend request" and the feature's user-visible
  half is unusable on arrival. Constraint 3 makes this un-fixable later.
- `Removed.reason` folds decline / unfriend / (later) block into one topic with one
  consumer — three topics would each need their own consumer.
- No `bool` anywhere (constraint 4). `state` is a `String`.
- Cursor is an opaque base64 blob **in the request body**: the `#[http]` grammar has no
  query-parameter source, so every non-path scalar rides the body.
- **No `Forbidden` anywhere.** Another player's edge is `NotFound`. Ownership goes
  *inside* the SQL predicate, never a comparison after a fetch.

**(d) Dispatch.** `[opus]` — public contract pinned by two baselines and two generators.

## Step 4 — `friends` module impl, write and read paths `[opus]`

Merged from revision 1's Steps 4 and 5: `impl friendsapi::Player` cannot compile with
`list`/`pending` deferred, and the `require::<dyn Directory>` wiring is three lines.

**(a) What.** `modules/friends/` — `lib.rs` (Module + DDL), `store.rs`, `service.rs`.

```sql
CREATE SCHEMA IF NOT EXISTS friends;
CREATE TABLE IF NOT EXISTS friends.edges (
    id           uuid PRIMARY KEY,
    low_id       uuid NOT NULL,
    high_id      uuid NOT NULL,
    requester_id uuid NOT NULL,
    state        text NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    accepted_at  timestamptz,
    CONSTRAINT friends_pair_ordered CHECK (low_id < high_id),
    CONSTRAINT friends_state_check  CHECK (state IN ('pending','accepted'))
);
CREATE UNIQUE INDEX IF NOT EXISTS friends_pair_idx ON friends.edges (low_id, high_id);
CREATE INDEX IF NOT EXISTS friends_low_idx  ON friends.edges (low_id,  state, created_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS friends_high_idx ON friends.edges (high_id, state, created_at DESC, id DESC);
```

**(b) Why now.** Contracts exist; Step 7's svc root needs a constructible module.

**(c) How — the parts that are not mechanical.**

- **The canonical ordered pair is the whole design.** `(low_id, high_id)` with
  `CHECK (low_id < high_id)` plus a unique index gives symmetry and pair-uniqueness in
  the database rather than in application code. `requester_id` is separate because the
  *transitions* are not symmetric.
- **`request` is two statements, never exception-driven control flow.** A duplicate key
  has three distinct causes and a blind "turn it into an accept" lets a player accept
  their own request by POSTing twice:
  ```sql
  INSERT INTO friends.edges (id, low_id, high_id, requester_id, state)
  VALUES ($1,$2,$3,$4,'pending')
  ON CONFLICT (low_id, high_id) DO NOTHING RETURNING id;          -- emits friend.requested
  -- zero rows ⇒ the reverse-pending auto-accept:
  UPDATE friends.edges SET state='accepted', accepted_at=now()
   WHERE low_id=$2 AND high_id=$3 AND state='pending' AND requester_id <> $4::uuid
  RETURNING id;                                                    -- emits friend.accepted
  -- zero rows again ⇒ idempotent no-op: duplicate own request, or already accepted.
  --                   EMITS NOTHING.
  ```
- **Ownership predicates — the addressee is not reliably `high_id`.** It depends on uuid
  ordering, so a `high_id = $2` test 404s legitimate accepts for ~half of all pairs and,
  in the other half, lets the *requester* accept their own request:
  ```sql
  -- accept / decline: only the non-requester party, only while pending
  … WHERE id=$1 AND state='pending' AND requester_id <> $2::uuid AND $2::uuid IN (low_id, high_id)
  -- remove: either party, any state, no requester_id clause
  DELETE FROM friends.edges WHERE id=$1 AND $2::uuid IN (low_id, high_id)
  ```
- **Outstanding-request cap.** `request` refuses above `MAX_PENDING_OUTSTANDING` rows
  where `requester_id = me AND state='pending'`, counted in the same transaction. This
  bounds table growth and blunts the probe described under "the oracle we accept".
- Self-friending is refused in the service, before the store.
- No cross-module FK (archcheck rule 7): `low_id`/`high_id` are plain `uuid` columns.
- Emission is the **last statement before `tx.commit()`**, via
  `emit_tx(AnyTx::new(&mut *tx), …)` on the store's own tx. A different tx compiles and
  silently breaks atomicity — nothing mechanical catches it.
- **The list query is a UNION ALL of two bounded branches**, not the single-column
  keyset `notifications` uses. `low_id = $1 OR high_id = $1` is a BitmapOr + sort that
  `LIMIT n+1` does not bound, so the keyset guarantee is *not* inherited from that
  precedent:
  ```sql
  (SELECT … WHERE low_id =$1 AND state=$5 AND (created_at,id) < ($2,$3)
     ORDER BY created_at DESC, id DESC LIMIT $4)
  UNION ALL
  (SELECT … WHERE high_id=$1 AND state=$5 AND (created_at,id) < ($2,$3)
     ORDER BY created_at DESC, id DESC LIMIT $4)
  ORDER BY created_at DESC, id DESC LIMIT $4
  ```
  The two indexes exist precisely to serve the two branches.
- Hydration is **one** batched `players_by_id` for the whole page. A player id the
  directory omits keeps its row with an empty `display_name` — a missing name must never
  drop a friendship from the list.
- `requires()` returns `vec!["accounts".into()]`.

**(d) Dispatch.** `[opus]` — pair canonicalization, the three-branch request, the
consent predicates and the emit-in-tx discipline are all correctness-critical.

## Step 5 — tests for Step 4 `[test-author]`, `model:"sonnet"`

The previously-wrong branches this must execute, named:

- **Both uuid orderings for accept.** Pick fixture uuids that force `requester = low_id`
  in one case and `requester = high_id` in the other. Without both, the F4 bug is never
  exercised.
- Accept by the **requester** is `NotFound`; accept by the addressee succeeds.
- **Duplicate own request is a no-op, not an accept**, and emits nothing.
- Reverse-pending request → `accepted`, and `friend.accepted` emitted **exactly once**.
- Already-accepted request → no-op, no second emission.
- Canonicalization: both request directions produce one row with `low_id < high_id`.
- Self-friend refused before the store.
- `decline`/`remove` by a third party → `NotFound`.
- The outstanding cap refuses the (N+1)th pending request.
- Paging across a boundary with equal `created_at`; both UNION branches return rows.
- A directory id omitted from the batch keeps its row.
- Emit shares the store tx: roll the tx back, assert no `asyncevents.events` row.

## Step 6 — consumers: `audit` sinks + `notifications` inbox rows `[sonnet]`

**(a) What.** `modules/audit/src/lib.rs`: append the three topics to `DURABLE_TOPICS`
and `"audit.friend-requested.v1"`, `"audit.friend-accepted.v1"`,
`"audit.friend-removed.v1"` to `DURABLE_SPEC_IDS` **at the same indices**; add
`friendsevents` to `[dev-dependencies]`; extend the two list assertions in
`modules/audit/src/tests.rs`. `modules/notifications/src/projection.rs`: two
subscriptions, `notifications.friend-requested.v1` and
`notifications.friend-accepted.v1`, both `StartPosition::AfterRegistration`.

**(b) Why now.** Constraint 1 — the events fail the blocking gate until each has a
subscriber.

**(c) How.** `audit` needs **zero handler code** — it subscribes by topic string via
`on_tx_raw` and never imports a payload type; the spec-id shape is asserted by its own
test. `notifications` uses `AfterRegistration` for the reason it already documents:
`Genesis` would replay every retained friendship into every existing inbox. Immutable
once shipped. The inbox row renders `requester_handle` from the payload — no lookup.

**(d) Dispatch.** `[sonnet]` — fully specified list edits.

## Step 7 — `cmd/friends-svc` + every authority `[sonnet]`

**(a) What.** New `cmd/friends-svc/{Cargo.toml,src/lib.rs,src/main.rs}` plus every row
of the authority table.

**(b) Why now.** Nothing can be proven in split until the process exists and every
authority knows it.

**(c) How.** The precedent is **`cmd/inventory-svc`**, not `cmd/mail-svc` — mail
deliberately dials no peer and takes `_wiring` unused, while friends needs
`Stub::new("accounts", wiring.peer_or("accounts", "127.0.0.1:9003"),
accountsrpc::remote_factories())`, exactly inventory's shape for `characters`. Note the
two silent rows: `cmd/admin-svc` (or the page is monolith-only) and — carried from
Step 1 — accounts' `EDGE_SLOT` registration, without which the split 404s while the
monolith works.

**(d) Dispatch.** `[sonnet]`.

## Step 8 — admin page, read-only `[opus]`

**(a) What.** `modules/friends/src/admin.rs`: a "Friends" item under Player Support —
KPIs (edges, pending, accepted), a table of recent edges, per-player drill-down via
`?player=`. `impl adminapi::AdminData` + `friendsrpc::register_admin`. **No
`AdminSubmit` in v1.**

**(b) Why now.** After the read paths exist; before the proof step, which asserts the
page through the front door.

**(c) How.** Read-only keeps `ADMIN_SUBMIT_MODULES` untouched. Follow `audit`'s
read-only shape, not `apikeys`' editable one. The route is `slug(label)` — derived from
the **label**, never the item id. `admin_data` must tolerate foreign/malformed params
and render an error card, **never `Err`**: the portal forwards every page's params to
every provider, so an `Err` surfaces as a broken card on an unrelated page, split-only.

**(d) Dispatch.** `[opus]` — UI is never a mechanical lane.

## Step 9 — tests for Step 8 `[test-author]`, `model:"sonnet"`

`admin_data` with foreign, missing and malformed params returns `Ok` with an error card
— the requirement Step 8 states and nothing currently pins. Plus the KPI counts against
a seeded fixture.

## Step 10 — contract hand-lists, regeneration, blesses `[sonnet]`

`topiccheck::defined_topics()` × 3; `golden.rs::event_samples_by_crate()` and
`rpc_modules()`; **`opscatalog-gen`'s separate `rpc_modules()`** (a second list of the
same name) with `friendsapi::player_rpc` **and** `accountsapi::directory_rpc`;
`csharp-client-gen`'s `PROVIDERS`. `golden_samples()` in the events crate with a
`None`-populated sample for any `Option` field. Then **regenerate**
`clients/csharp/Generated` and `opscatalog/src/generated.rs` (a regeneration, not a
bless — `codegen-freshness` byte-diffs the committed tree), then `--bless-public-api`
and `--bless-contract-golden`, **reading each diff** and restoring any baseline file
this change did not cause.

## Step 11 — conformance `[sonnet]`

`tools/conformance/src/policy.rs`: a `friends()` entry with an explicit stance for all
four conventions, plus `input_policies()` rows for every new wire field — Step 1's
`ids`/`handle`, Step 3's `target_handle`/`cursor`/`limit`/`edge_id`, each pointing at
the named const. Non-applicability needs a concrete architectural reason, never
`na("n/a")`. Then `--bless-input-golden`.

## Step 12 — splitproof assertions `[test-author]`, `model:"opus"`

`[FR1]`–`[FR7]`, through gateway-svc, plus a monolith-parity pass. Minimum: request →
`pending` row (DB-asserted); accept by the addressee → `accepted`; accept by the
requester → 404; the durable chain `friend.accepted` → `audit.log` row **and** →
notifications inbox row **carrying the handle, not a uuid** — the cross-process path a
monolith run cannot substitute for; `list` returns the friend with a non-empty
`handle` and an `online_until`, proving Step 1's capability resolves Remote **over the
edge registration** (this is the assertion that would have caught the silent F2);
another player's edge id is 404 not 403; both-sides-request auto-accepts.
`model:"opus"` — the harness shape is novel (two registered players, two bearers).

## Step 13 — verify `[inline]`

`cargo run -p verifyctl -- --fast`, then `--all --strict`. Output to a file with
`; echo "EXIT=$?"` — never through a pipe, which reports the pipe's status.

## Step 14 — docs `[docs]`, `model:"sonnet"`

Named files: `docs/roadmap/feature-tracker.md` (Social row → ✅ with the presence
caveat; the "Presence / online status" row notes session-derived shipped and socket did
not; "User metadata / profile" notes handles landed), `README.md`, `CLAUDE.md` (15
fortresses, module list, `:8095`/`:9014`), `.agents/shared/gamebackend.md`, and errata
in this plan.

---

## The oracle we accept, stated honestly

`find_by_handle` being wire-only does **not** close the existence oracle:
`POST /friends/requests` returns 201 vs 404 through the front door. Exact-handle
matching removes the *bulk* oracle (one prefix → many names per call), not the oracle
itself. What bounds it is the gateway rate limit (20 rps) plus Step 4's outstanding-request
cap. The discriminator also means guessing a handle requires the name *and* four digits.
This is a reduction in throughput, not a closure, and the plan does not claim otherwise.

## Known gaps this plan deliberately does not close

- **Socket presence** — four obstacles above; a separate plan if ever wanted.
- **Blocking** — no `friend.blocked` topic, because a topic without a consumer fails
  the gate. Blocking later is a new state on the existing edge plus
  `Removed { reason: "blocked" }`, not a new table.
- **Handle changes** — `display_name` is still immutable in `accounts`; a rename path is
  its own feature and would need the discriminator re-minted.
- **`notifications` has no command topic**, so every new module wanting an inbox row
  edits `notifications`. `mail` closed this for itself; `notifications` has not.

## What revision 2 changed

Revision 1 was reviewed and returned **not implementable**. The corrections, so the
reasoning is not lost:

1. `online: bool` would have failed the blocking `codegen-freshness` stage — `bool` is
   an unmodelled scalar in the C# generator. Now `online_until: String`, which also
   stops the contract promising socket presence it cannot deliver.
2. The accept predicate `WHERE id=$1 AND high_id=$2` was wrong twice: it 404s
   legitimate accepts for ~half of pairs (uuid ordering) and lets the **requester accept
   their own request** in the other half.
3. The auto-accept branch "turn the duplicate key into an accept" could not distinguish
   reverse-pending from a duplicate *own* request — the same consent bypass via a double
   POST. Now three explicit branches, one of which emits nothing.
4. `display_name` has no uniqueness in `accounts`, so discovery by name was an
   invite-hijack. Handles with a minted discriminator, decided by the user.
5. Event payloads carried only uuids, so the notifications inbox row would have been
   unusable — and constraint 3 makes that un-fixable after v1. Handles denormalized.
6. Missing authorities: accounts' `EDGE_SLOT` registration (**silent, split-only
   404s**), the rpc meta-macro line, `opscatalog-gen`'s separate `rpc_modules()`,
   `csharp-client-gen`'s `PROVIDERS`, the two generated trees, workspace `Cargo.toml`,
   and the gateway/admin dependency + peer-env additions in both fleet authorities.
7. `checkmodules` was labelled silent; it is **loud**. `routecheck`'s `GATES` row was
   noise — cut.
8. Steps 4 and 5 were split such that Step 4 could not compile. Merged.
9. Steps 4, 5 and 8 had no test steps, breaking the repo's rule. Added as Steps 5 and 9.
10. The `notifications` keyset precedent does not carry to a two-column `OR` predicate.
    UNION ALL spelled out.
11. `PlayerSummary` was going to appear on the friends wire; `friendsapi::Friend` is now
    its own DTO.
12. `MAX_DISPLAY_NAME_BYTES` is private to `modules/accounts`; promoted to the contract
    so both sites read one authority.
