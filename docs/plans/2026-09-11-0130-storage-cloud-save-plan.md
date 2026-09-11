# `storage` — player cloud-save (P1#7)

**Status:** revision 2, after adversarial review (17 findings: 7 blocking-factual,
4 structural, 6 citation). Review response at the end.

Tracker row: `docs/roadmap/feature-tracker.md:206` — *Generic storage objects /
player cloud-save (KV+OCC) ❌ — P1#7. Self-contained new fortress.*
Shape already decided by the gap analysis (`docs/reference/game-backend-feature-gaps.md:194-198`):
*"collection/key, per-user + public, read/write permission levels, OCC via version …
new `storage` fortress, its own schema … Self-contained."*

---

## Context — the overlapping systems, and why a new fortress

- **`config`** — operator KV with a revision. **Why not extend:** no owner column
  (PK `(namespace, key)`), no authenticated write path (`ConfigSnapshot` is wire-only,
  no `Identity`; `set` is off the shared trait), and a **global singleton revision**
  whose job is invalidating a whole-store cache in every process. One player's save
  would force a full config re-read and a full-snapshot RPC in every replica.
- **`inventory`** — the closest per-player mutable state. **Why not extend:** its value
  is a CHECK-bounded `bigint` keyed to a foreign-keyed catalog row — counts of
  pre-registered items only, never a caller-chosen key or an opaque document. No
  version, no paging.
- **`accounts`** — **why profile fields do not move here.** `display_name`/`discriminator`
  back a cross-player uniqueness invariant and are served to other modules through the
  wire-only `Directory`; avatar and language are attributes *of the identity* and stay
  an `accounts` edit. Cloud-save values are opaque to the server and unreadable by the
  directory.
- **`notifications`** — the closest *structural* precedent (per-player rows, keyset
  paging, contract-side caps, `NotFound`-not-`Forbidden`). **Why not extend:** rows are
  server-authored, append-only, opaquely addressed and time-pruned. Cloud-save is
  client-authored, updated in place under CAS, caller-addressed, and must not expire.
- **`apikeys`** — **not extended; it is the OCC design source.** CAS-by-`revision`
  (`modules/apikeys/src/store.rs:294-327`) with the rule this plan copies verbatim:
  *a miss is `Conflict`, never a not-found.*
- **`wallet`** — cited for byte-cap pairing and idempotency discipline, depended on for
  neither. Its question is "did I already apply this request?"; ours is "is the value I
  am overwriting the one I read?" — a lost update a key cannot detect.

---

## Findings

**S1 — the purge half is a copy, not a design.** The account-deletion rollout
(`79cf52d`..`e90985a`) landed a tombstone table, a budgeted sweep and a write guard in
**six** modules through six review rounds. `storage` is the **seventh purge module**
and the **eighth `player.deleted` consumer** (six module subscriptions + audit's raw
sink). Its splitproof block is `[DL11]`.

**S2 — two parts of that pattern do not transfer.**
1. **Per-player work is not naturally bounded.** The six landed modules have domain caps
   (character limit, catalog-bounded balances, `MAX_MEMBERS`); a caller-authored
   document store has none. Settled by D5.
2. **No `wallet`-style keep-under-sentinel.** The ledger survives because it *is* the
   dedup registry; a version column is not one — a reused `(owner, collection, key)`
   after deletion is a new document — so `storage` deletes.

*(Revision 2 deletes rev 1's third non-transfer. It described a both-sides predicate for
a grantee-keyed ACL grant table that this plan does not create and its own non-goals
forbid. With `acl` a token on the object row, `WHERE owner_id = $1` is sufficient and
the `inventory` copy is exact.)*

**S3 — `CONTRACT_ONLY` is empty**, so a contract crate carrying `#[http(` with no
module **fails archcheck rule 21** (`tools/rpc-contract-model/src/lib.rs:119`, `:208-235`).
Rule 21 also only protects `#[http(`-bearing domains: a wire-only crate with a divergent
module directory name is invisible to it and to all five served gates.

**S4 — `modules/storage/Cargo.toml` is the detonator.** `module_dirs` keys on that file
alone (`lib.rs:78-99`), driving archcheck 12/15/17/21, `checkmodules`,
`opscatalog-gen` (`main.rs:180-192`), `csharp-client-gen` (`scrape.rs:188-192`) and
`conformance` (`main.rs:101`, `input_inventory.rs:89-95`). There is no "create the crate
now, wire it next".

**S5 — the write path's ambiguity is structural.** A CAS `UPDATE` matching zero rows
cannot distinguish "not yours", "no such object" and "stale version". Settled by D4.

**S6 — `collection` and `key` are the first caller-authored path wildcards in the tree.**
Every existing wildcard is a server-minted uuid. The gateway router does raw, undecoded
segment matching (`modules/gateway/src/lib.rs:2089-2107`) with an exact segment count,
so a key containing `/` is **unaddressable** — a 404 the client cannot act on — and a key
containing `%` stores whatever the client encoded, so two clients with different encoding
habits address different rows for the same logical key. The player-QUIC plane has no such
restriction, so without a rule the two topologies accept **different key sets**. Settled
by D9.

---

## Decisions

| # | Decision | Rationale |
|--:|---|---|
| D1 | **Address is `(owner_id, collection, key)`** | Owner's decision. A public reader must know the owner to name the object, so the leak is in the addressing scheme, not the response — the shape `leaderboard` already has with `Score.player`. Accepted oracle, stated below. |
| D2 | **No time-based pruning** | Owner's decision. A save must not evaporate. Consequence, recorded: nothing collects an object but its owner and the purge — which is why D5's sweep must not leave a residual row. |
| D3 | **Public read widens the visibility predicate** | Owner's decision, derived from the repo's rule — no module here has had per-object ACL. One statement; a nonexistent object, another owner's private object and a malformed id all collapse to the same `None`. A non-owner's **write** is also `NotFound`, or the oracle returns through the write path. |
| D4 | **One statement, one snapshot** — a CTE, not two statements | Owner chose "disambiguate rather than collapse to 404". Rev 1 specified two statements in a transaction and justified it with a claim that is **false**: under READ COMMITTED each statement takes its own snapshot and a zero-row `UPDATE` takes no lock, so the transaction buys nothing and the difference is untestable. The CTE form genuinely closes the window and is assertable: `WITH upd AS (UPDATE … WHERE owner_id=$1 AND collection=$2 AND key=$3 AND version=$5 RETURNING version) SELECT (SELECT version FROM upd), EXISTS(SELECT 1 FROM storage.objects WHERE owner_id=$1 AND collection=$2 AND key=$3)`. Updated → `Ok(version)`. Not updated but exists → `Conflict`. Neither → `NotFound`. |
| D5 | **A per-owner object cap at the write path, and the sweep takes the same lock** | Owner's decision. A cap counted by a statement is count-then-insert, so it inherits the advisory-lock obligation (`modules/groups/src/store.rs:59-79`; under READ COMMITTED two concurrent creates both read `cap-1`). **Rev 2 adds the second half:** `inventory`'s sweep disclaims a lock because its rows are disjoint from any guarded key (`modules/inventory/src/projection.rs:261-262`) — untrue here, where the sweep deletes exactly the rows the owner lock guards. `groups` is the right precedent: it takes the lock in its sweep and pays with a per-fire cap. `STORAGE_OWNERS_PER_FIRE = 64` (the subxid-cache ceiling, the binding constraint — not the lock table's 9600 slots). Without this, D2 means a row surviving the race is collected by **nothing, ever**. No deadlock either way: the sweep waits on row locks a `put` holds; a `put` never waits on anything the sweep holds. |
| D6 | **`version` is monotone within an incarnation; `delete` resets the address** | `version = version + 1`, never assigned or lowered; a future operator "restore" writes the old **value** at `version + 1`. The ABA case rev 1 named and left open is now settled: after delete-and-recreate an in-flight `put` carrying `expected_version = 1` **matches** and overwrites a different document. Expectations ≥ 2 miss and become 409. Accepted as a documented contract rather than a high-water table, because D8 makes the safe path the default (`0` = create-or-409, so a correct client never sends `1` blind). **On the gap list.** |
| D7 | **`acl` is a named closed-set token**, validated in the service | `bool` is not in the C# lattice. Precedent for a boolean that cannot be one: `read_at: String`, empty means unread. Precedent for a closed-set token: `join_policy`. Extensible to `public_write` without a wire break. An **empty** `acl` on a write means "leave it unchanged" — stated in the contract crate and pinned, or a value-only write silently un-shares an object. |
| D8 | **`expected_version = 0` means "create, or 409 if it exists"**; `< 0` is `Invalid` | Rev 1 said "unconditional", which is a **blind overwrite that defeats OCC** and is exactly what a naive client sends. Create-or-409 has no escape hatch and makes `0` the safe default. `resolve_limit`'s `0 = unspecified, negative is the one rejection` is the sentinel precedent. Stated in the **contract crate** beside the field. |
| D9 | **A legal-segment alphabet, one authority** | S6. `storageapi::is_legal_segment` over `[A-Za-z0-9._-]`, stated beside `MAX_KEY_BYTES`, validated in the service before any statement, and cited by **both** the path binding and the cursor codec — which also settles rev 1's undecided `|` question, since `|` is outside the alphabet. |
| D10 | **`storage` publishes no durable event** | The value is opaque; nothing downstream can project it. So **no `api/storage/events` crate** — five `api/<name>` dirs already have none. Rev 1 created one and told the implementer how to write `define(`, which would have fired topiccheck's unsubscribed-topic drift for a topic nobody wants. |

### The oracle we accept

D1 means a public object's address contains its owner's id, so anyone who can read one
already knew whose it was. Accepted deliberately. The alternative — an opaque uuid as
the public address — changes the primary key and was not chosen.

---

## Contract shape

```rust
// api/storage/api/src/lib.rs
pub const MAX_COLLECTION_BYTES: usize = 64;
pub const MAX_KEY_BYTES: usize = 128;
pub const MAX_VALUE_BYTES: usize = 65_536;
pub const MAX_OBJECTS_PER_OWNER: i64 = 10_000;
pub const MAX_CURSOR_BYTES: usize = 256;
pub const MAX_PAGE_LIMIT: i64 = 100;
pub const DEFAULT_PAGE_LIMIT: i64 = 25;
pub const ACL_PRIVATE: &str = "private";
pub const ACL_PUBLIC_READ: &str = "public_read";
```

One `#[rpc(prefix = "storage")] pub trait Player`, every method leading with `Identity`:

| op | verb / path | retry_safe | notes |
|---|---|:--:|---|
| `put` | POST `/storage/{collection}/{key}` | no | `value: String`, `expected_version: i64`, `acl: String` |
| `get` | POST `/storage/{collection}/{key}/get` | **yes** | |
| `delete` | DELETE `/storage/{collection}/{key}` | no | replay is 404, the tree's uniform shape |
| `list` | POST `/storage/list` | **yes** | `collection`, `cursor`, `limit` in the **body** — the grammar has no query source |

**`put` is deliberately not `#[retry_safe]`**, so an ambiguous network result on a CAS
write has no replay story: the client re-reads and re-CASes. `match.report` and
`wallet.credit` earn the attribute with a required client key; adding one here would be
a second idempotency mechanism beside `version`. On the gap list.

DTOs are `String`/`i64` only. `bool`/`u32` are rejected by `UNMODELLED_SCALARS`
(`tools/csharp-client-gen/src/scrape.rs:429-432`); `Option<T>` is **not** in that list —
it falls through to `TypeRef::Struct("Option")` (`scrape.rs:461`) and dies later as a
missing-DTO error, so the ban is real but that line is not its authority. An arbitrary
JSON value is expressible only as a `String`, bound as text and cast `::jsonb`, validated
in the service first — an unmapped `22P02` from the cast is a 500 where the contract
promises 400.

### Schema

```sql
CREATE TABLE IF NOT EXISTS storage.objects (
    owner_id   uuid        NOT NULL,
    collection text        NOT NULL,
    key        text        NOT NULL,
    value      jsonb       NOT NULL,
    version    bigint      NOT NULL DEFAULT 1,
    acl        text        NOT NULL DEFAULT 'private',
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (owner_id, collection, key),
    CONSTRAINT storage_collection_len_check CHECK (octet_length(collection) <= 64),
    CONSTRAINT storage_key_len_check        CHECK (octet_length(key)        <= 128),
    CONSTRAINT storage_value_len_check      CHECK (octet_length(value::text) <= 131072),
    CONSTRAINT storage_acl_check            CHECK (acl IN ('private','public_read'))
);
CREATE INDEX IF NOT EXISTS storage_list_idx
    ON storage.objects (owner_id, collection, created_at DESC, key DESC);
```

`collection` and `key` sit in the **primary key**, a btree, so an uncapped value raises
`54000` — an *unmappable 500*, not the promised 400
(`modules/notifications/src/service.rs:87-93`, `:240-246`). Their Rust caps and named
CHECKs pair through `ColumnCap`/`COLUMN_CAPS` (`modules/apikeys/src/store.rs:85-131`),
pinned both directions (`store_tests.rs:582-615`).

**`value` is deliberately outside that pairing.** The CHECK measures normalized jsonb
text — whitespace dropped, keys reordered, escapes and numbers rewritten — while the Rust
cap measures the caller's input string. They are different byte strings, so the DB
constraint is a **backstop at double the Rust limit**, not a twin, and its number is
deliberately distinct so the two can never be confused. `value` must be excluded from the
`COLUMN_CAPS` pairing test, whose `octet_length(`/`<= N` assertions would otherwise pass
vacuously while the mapped message lied about what the caller sent.

---

## Step 1 — contracts, module, svc, registrations — ONE commit  `[opus]`

**(a)** `api/storage/{api,rpc}` (no events crate, D10), `modules/storage/{Cargo.toml,src/{lib,store,service}.rs}`
including `SCHEMA_DDL` and `Module::migrate` for `storage.objects`, `cmd/storage-svc`,
and every registration S4 fires: `cmd/server`, `cmd/gateway-svc`
(`Stub::describe_peer("storage", …)`), `tools/checkmodules` Split profile, workspace
`Cargo.toml` (**four** entries — two api crates, the module, the svc).

**(b) Why now / the dependency.** S3: `CONTRACT_ONLY` is empty, so a contract crate with
`#[http(` and no module fails a blocking gate, and the only alternative is an exemption
entry that must then be deleted — a stale exemption outlives its reason
(`tools/rpc-contract-model/src/lib.rs:236-249` fails it both ways), which is the thing
the repo actually forbids. A **transient** red across Steps 1-2 is the sanctioned broken
intermediate; a stale allow-list entry is not.

**(c)** Copy `api/groups/{api,rpc}` and `modules/groups` for structure. **Every op body is
`Err(Error::internal("not implemented"))`** — Steps 3 and 4 replace them, and saying so
here is what lets a reviewer at Step 3 tell stub from intent.

**Red after this step — observe and report verbatim, do not predict.** Expect
`codegen-freshness`, `conformance`, `contract-golden` (all **blocking**) and `public-api`
(**advisory**, `--all`/`--strict` only). Step 2 closes them.

**(d)** `[opus]` — core-implementer, *think hard*.

## Step 2 — the gates  `[opus]`

**(a)** `opscatalog-gen`'s `rpc_modules()`; `csharp-client-gen`'s `PROVIDERS` + `phase_a()`;
the regenerated artifacts; the four `testdata` goldens; the hand sets in
`tools/csharp-client-gen/src/tests.rs`; `DEV_CLIENT_POLICY`; `conformance`'s `policy.rs`
entry and `input_policies()` rows; three blessings.

**(b) Why now.** These fail *because* Step 1 landed, and Steps 3-7 add no new op, so
deferring them leaves the tree red across five steps for no gain.

**(c) Order, derived by execution in two prior rollouts:**
1. regenerate `opscatalog/src/generated.rs` — **same commit**, extend `DEV_CLIENT_POLICY`.
   Its reverse test iterates the *generated* catalog, so it is green until this moment
   and then **fails red**; it exists precisely so this is a test failure and not a silent
   403 for `dev-key-client` (`modules/apikeys/src/tests.rs:99-132`).
2. regenerate `clients/csharp/Generated/`, then re-bless the four `testdata` goldens.
3. **Only then** update `golden_covers_the_known_surface`'s counts (37/19/8 today) and
   **both** `BTreeSet` literals — it reads the committed testdata, not the live scrape.
4. `--bless-input-golden`, `--bless-contract-golden`, `--bless-public-api`.

Conformance stances must be **executed**: `NotApplicable { why }` is checked only for
non-blankness and is the largest hole for a new module. If `storage` implements
`AdminSubmit`, `admin_submit_findings` forces `ADMIN_SUBMIT_MODULES` **and** a non-empty
`CapCase`. **Prove one CapCase bites** — break the guard, read the failure verbatim,
restore, confirm with a checksum. **After every bless, diff the directory.**

**(d)** `[opus]`.

## Step 3 — the CAS write path  `[opus]`

**(a)** `put` (create and update), the owner cap, the advisory lock, the D4 CTE. Replaces
Step 1's stub body.

**(b) Why now.** Everything else reads what this writes; the cap and lock decided here
constrain Step 5's sweep.

**(c)** Non-negotiables:
- **The D4 CTE is one statement and one snapshot.** Updated → `Ok(version)`; not updated
  but exists → `Conflict`; neither → `NotFound`.
- **Create path**, stated because rev 1 omitted it: `expected_version = 0` →
  `INSERT … ON CONFLICT DO NOTHING RETURNING version`; zero rows → `Conflict` (D8's
  create-or-409). So `23505` cannot surface.
- **The owner lock is taken on the create path only**, before the count — the update
  path adds no row and needs none. D5's cap is count-then-insert
  (`modules/groups/src/store.rs:59-79`).
- The CAS itself needs **no** lock: blocking on a concurrent writer's row lock, it
  re-applies its own qualifier to the newly committed row, so the loser matches zero
  rows. That re-application is the whole guarantee.
- **Never** `SELECT version` → compare in Rust → `UPDATE` (the count-then-insert bug in
  disguise, and a silent overwrite). **Never** `SELECT … FOR UPDATE` then an
  unconditional `UPDATE` (serializes correctly, then the loser *succeeds*).
- The write predicate uses **`owner_id = $1` alone**. The read's public-read disjunction
  must be a **separate string** with a doc comment saying why, or `public_read` becomes
  public *write* with no error anywhere.
- Validate JSON, the `acl` token, `is_legal_segment` (D9), `expected_version < 0` and
  every byte cap **in the service, before the statement**.

**(d)** `[opus]` — *think hard*.

## Step 4 — reads, ACL and paging  `[opus]`

**(a)** `get`, `list`, the visibility predicate, the cursor codec. Replaces Step 1's stubs.

**(b) Why now.** After the write path, so the tests in Step 9 can seed through the real
authority rather than raw SQL.

**(c)** The read predicate is one statement widened by a disjunction (D3), so four
different "no" answers collapse to one `None` mapped to a single `NOT_FOUND` const. The
keyset codec is copied per-module: base64url-no-pad, calendar-validated, cap checked
**before** the base64 decode, malformed ⇒ `Invalid` never a page-1 reset. `key` is text,
not a uuid — D9's alphabet excludes `|`, which is what makes `split_once('|')` safe here;
both existing codecs have a uuid in that half and cannot hit it.

**(d)** `[opus]`.

## Step 5 — the purge half  `[opus]`

**(a)** `storage.purged_players`, the tombstone subscription, the sweep, the write guard,
the *Purges Pending* KPI.

**(b) Why now.** After the rows exist to purge and after D5's lock exists to share.

**(c)** Copy `modules/inventory/src/projection.rs` for the delete-by-owner shape and
`modules/characters/src/projection.rs` for the doc comments — **but take the owner
advisory lock (D5), which `inventory` does not**, and add `STORAGE_OWNERS_PER_FIRE = 64`
with the subxid-cache sentence. Reuse `schedulerevents::schedule_names::PLAYER_PURGE` —
**no new schedule row, no `seeded_schedule_names_are_contract` edit**. Two globally unique
subscription ids, both `Genesis`. `ORDER BY storage.purged_players.player_id` — **qualify
the table**; unqualified it binds to the `::text` output column and the floor degrades to
a seq scan with an on-disk sort inside the delivery transaction (~1850×, measured).
Budget at the player boundary, after the work *and* the stamp. Add `"storage"` to the two
hard-coded schema arrays in `tools/splitproof/src/main.rs` (`:3046`, `:3062`).

**(d)** `[opus]`.

## Step 6 — the admin page  `[opus]`

**(a)** `modules/storage/src/admin.rs`, the `adminapi::SLOT` item, `register_admin` in the
existing `EDGE_SLOT` closure.

**(b) Why now.** After Step 5, because the page carries the *Purges Pending* KPI; before
Step 7, which adds the `admin-svc` stub that makes it visible in the split.

**(c)** Every self-link from `adminapi::slug(LABEL)`, never the item id. `admin_data`
**must never `Err`** — render an error card; the portal forwards every page's params to
every provider, so an `Err` degrades an *unrelated* page in the split. Rejections map
`Stale → Conflict`, `Rejected → invalid`, `Internal → internal`, **never `NotFound`**,
which the edge makes indistinguishable from `UnknownMethod`. Copy `friends`' read half.

**(d)** `[opus]`.

## Step 7 — fleet and topology  `[sonnet]` + `[test-author]`

**(a)** `[sonnet]`: `tools/processctl/src/fleet.rs` (row **and** the `FleetSpec::new` vec —
order is boot order, a dependency must be strictly earlier);
`cmd/gateway-svc/src/addrs.rs`'s `ADDR_SPECS`; `cmd/admin-svc` lib+main;
`weles/fleet.split.toml` placed before its consumers.
**`[test-author]`**: `cmd/gateway-svc/src/addrs_tests.rs`'s `FLEET` table and
`FakeAgent::healthy()`, **and the rename of `managed_resolves_the_same_twelve_pairs_as_env`**
(`:288`) whose count lives only in the test name; `weles/master/src/fleet_toml_tests.rs:868`
(`17 → 18`); `weles/master/src/manifest_tests.rs`'s env-manifest golden.

**(b) Why now.** After the module exists, before Step 8 — splitproof's `preflight_fleet`
fails on disk drift before anything spawns.

**(c)** **HTTP 8097 / edge 9016** (8096 and 9015 are the current maxima; 9013 is
gateway's own edge). Session budget +6 → 103 against 131. Fits.

**(d)** Split by lane because CLAUDE.md makes test files `[test-author]`'s only — the
four files above are tests, the rest is not.

## Step 8a — split-proof assertions  `[test-author]`, `model:"opus"`

**(a)** `[DL11]` plus `[ST*]` in `tools/splitproof/src/main.rs`, both topologies.

**(b) Why now.** Against landed Steps 3-7; nothing forces these to exist, so they are the
only proof.

**(c)** Per consumer, **joined with "and"**: an exact non-zero pre-count; a survivor
seeded identically and untouched; **both** subscription cursors at-or-past in the
**positive** form (`count(*) = 1`) — the negative form passes vacuously for a deleted
consumer. Plus: a CAS conflict returning **409 to the owner** and **404 to a stranger**
(D4); a public object readable by a non-owner while a private one is 404; the owner cap
refusing at `MAX_OBJECTS_PER_OWNER + 1`; a key containing `/` rejected by D9 at the
service rather than 404-ing at the router. **Prove one assertion non-vacuous** —
perturb, observe, revert with a checksum.

**(d)** `[test-author]` at `model:"opus"`.

## Step 8b — audit the proof  `[proof-auditor]`

**(a)** Audit the landed `[DL11]`/`[ST*]` assertions.

**(b) Why now / why this order.** Rev 1 had proof-auditor audit a *spec* — outside its
contract, which is auditing landed artifacts. Auditing after the fact is what caught four
vacuous shapes last rollout, including an assertion that computed two cursors, printed
them, and never put them in the predicate.

**(c)** Does each assertion fail if the property fails? Does any pass when a fixture never
seeded? Is the cursor comparison the positive form?

**(d)** `[proof-auditor]`.

## Step 9 — module unit tests  `[test-author]`, `model:"sonnet"`

**(a)** `modules/storage/src/*_tests.rs`.

**(b) Why now.** From the landed, compiling implementation.

**(c)** Each names its previously-wrong branch. The checklist: ownership compared after
the fetch; the read predicate reused on the write path; a CAS on a resettable column; a
restore writing the old version number; **`expected_version = 1` after delete-and-recreate
overwriting a different document** (D6's accepted contract — pin the behaviour so it
cannot change silently); `expected_version = 0` against an existing row returning 409, not
an overwrite; a negative expectation as `Invalid`; an empty `acl` leaving the ACL
unchanged; malformed JSON reaching the cast; `collection`/`key` over cap raising `54000`;
a `value` whose normalized jsonb exceeds the Rust cap; `22P02` on `owner_id` as `None`
not 500; `version` as `i32`; and D9's alphabet rejecting `/`, `%`, `|`.
Confirm **zero `SKIP: postgres unreachable` lines** and say so.

**(d)** `[test-author]` at `model:"sonnet"`.

## Step 10 — acceptance  `[inline]`

`--fast`, then `--all --strict`. Redirect and **read the stage table** — a trailing
`echo $?` reports the echo's status, which produced a false green on 2026-09-10.

## Step 11 — documentation  `[docs]`, `model:"sonnet"`

`docs/roadmap/feature-tracker.md:206`; `docs/reference/game-backend-feature-gaps.md`
(archive — append errata, do not rewrite); `CLAUDE.md` (module list, fortress count
16 → 17, split-proof ports and assertion families); `.agents/shared/gamebackend.md`;
`AGENTS.md`; **`.agents/README.md:26`**, which says "13 fortresses + gateway" and is
already stale; and this plan's errata.

**The known-gap list must be complete:** the D1 addressing oracle; nothing collects an
object but its owner and the purge (D2); **D6's `expected_version = 1` ABA after
delete-and-recreate**; **keys unrepresentable through the HTTP front (`/`), and the
player-QUIC plane accepting a wider set unless D9 is enforced in the service** — the
never-monolith-only rule pointed the other way; **the `value` cap and its CHECK measure
different byte strings**, so the CHECK is a backstop and `value` is outside the
`COLUMN_CAPS` pairing test; **`put` is not `#[retry_safe]`**, so an ambiguous network
result has no replay story; no value-indexed query; no sharing beyond `public_read`; no
per-collection TTL; and the two pre-existing conformance defects the deletion rollout
recorded, which this module inherits.

---

## Non-goals (deliberate, recorded)

- **Value-indexed queries.** A `jsonb` GIN index over caller-authored documents is an
  unbounded write amplifier; Nakama's storage indexes are a separate feature.
- **A grantee-keyed ACL table.** A second authorization path and a second oracle surface
  in a module that does not exist yet. D7's token is extensible to it without a wire
  break. *(Rev 1 both forbade this table and wrote a sweep predicate against it.)*
- **Per-collection TTL.** Contradicts D2 in the general case.
- **Cross-owner atomic writes.** One object, one statement, one version.

---

## Step 1 errata (landed `974692f`)

1. **D3 had no reachable consumer.** The contract table gave `get` only
   `{collection}/{key}`, but D1 puts the owner in the address — so a non-owner could not
   name another owner's object and `ACL_PUBLIC_READ` was unexercisable, including by
   Step 8a's required assertion. `get` now takes `owner_id: String` (empty = the
   caller's own). `list` deliberately does **not**: a public object is readable by
   whoever can name it, never discoverable. That second half is the implementer's
   decision, consistent with D1's accepted oracle.
2. **`ColumnCap`/`COLUMN_CAPS` moves to Step 3, deliberately.** Its `constraint` field
   exists only so a `23514` can be mapped back to the Rust verdict; Step 1 has no
   statement that can raise one, so the table would need `#[allow(dead_code)]` under
   `-D warnings` — the same exemption-outliving-its-reason shape Step 1(b) rejects.
   Step 1 lands the named CHECKs and one `validate_segment` authority instead.
   **Step 3 must introduce `ColumnCap`/`COLUMN_CAPS`/`by_constraint` with the write
   path's error mapping, or Step 9's pairing test has nothing to iterate and the
   pairing never exists.**
3. `ObjectSummary` carries no `value` — 100 rows x 64 KiB is a response nobody asked
   for. `put` returns a one-field `ObjectVersion` rather than a bare `i64`, because no
   `#[http]` op in the tree returns a scalar today.
4. The plan's Schema block omits `CREATE SCHEMA IF NOT EXISTS storage;`.
5. **Carried obligations for later steps, found while docs-correcting Step 1's
   comments (`974692f`):**
   - **Step 4 / Step 9:** `get(owner_id = "<the caller's own uuid>")` must return the
     caller's own private object, not 404 — a naive `if owner_id.is_empty() { caller }
     else { require public_read }` 404s a client that echoes its own `owner_id` back.
   - **Step 4 / Step 9:** the paging cursor must NOT encode `owner_id`. A cursor forged
     or replayed from another session must not move the `WHERE owner_id = $caller`
     predicate — the only path by which `list`, which has no owner argument, could leak
     another owner's rows. Prove it with a cursor minted for owner A replayed by owner B.
   - **Step 3:** `storage_value_len_check` (131072) can fire on an input UNDER the 65536
     Rust cap, because jsonb normalization can expand a value (a large exponent literal,
     for instance). Today an unmapped `23514` is store trouble — a 500 where the contract
     promises 400. Step 3 must decide that mapping explicitly even though `value` stays
     outside the `COLUMN_CAPS` pairing.
   - **Step 3:** when `ColumnCap` lands, its `23514` message must be byte-identical to
     `validate_segment`'s current wording, or the Rust and DB verdicts word one limit two
     ways.
   - **Step 7:** `ADDR_SPECS`' storage entry must be `9016`, matching the hardcoded
     default at `cmd/gateway-svc/src/lib.rs:159`. Until then `STORAGE_EDGE_ADDR` is
     ignored and a managed boot silently dials the standalone default instead of failing
     loudly — the exact disjointness `cmd/gateway-svc/src/addrs.rs:11-18` exists to
     refuse. Same defect class as the `FRIENDS_EDGE_ADDR` finding in the groups rollout.

## Review response (rev 1 → rev 2)

All seven blocking-factual findings addressed: the phantom ACL grant table and every
sentence derived from it are deleted (A1); D4 becomes a single-statement CTE after the
transaction rationale proved false under READ COMMITTED, which also removes an unwritable
test item (A2); D6 settles the ABA rather than naming it (A3); D8 changes
`expected_version = 0` from a blind overwrite to create-or-409 and Step 3 now states the
`INSERT` and where the lock is taken (A4); D5 gains the sweep-side lock and a per-fire cap
after `inventory`'s disclaimer proved inapplicable (A5); D9 adds a legal-segment alphabet,
which also settles the cursor separator (A6); and the `value` CHECK is restated as a
backstop measuring a different quantity, explicitly outside the pairing test (A7).

Structural: every step gains (a)/(b)/(c)/(d) and Steps 6-8 are restructured (B1); lanes
are fixed — Step 7's four test files move to `[test-author]`, splitproof assertions are
written by `[test-author]` and audited afterwards by `[proof-auditor]` (B2); Step 1's
sequencing rationale is restated as "a stale exemption outlives its reason, a transient
red does not", with `public-api` marked advisory (B3); Step 1 now owns `storage.objects`'
DDL and `migrate` and names the stub bodies Steps 3-4 replace (B4).

Citations: the `DEV_CLIENT_POLICY` claim is corrected — the test **fails red**, it does
not stay green (C1); "eighth purge consumer" becomes "seventh purge module, eighth
`player.deleted` consumer, `[DL11]`" (C2); the events crate is dropped and D10 records why
(C3); `FLEET`/`FakeAgent` are attributed to `addrs_tests.rs` (C4); the `Option` ban keeps
its real authority (C5); `.agents/README.md` joins Step 11 (C6).

## Step 1/2 errata (proof audit, `05e38fe`/`014fcf2`)

1. **Split-proof is red and no step's observed-red list caught it.** `cmd/storage-svc/`
   exists on disk while `tools/processctl/src/fleet.rs` has no `storage` service, so
   `validate_disk` raises `FleetError::DiskDrift` at
   `tools/splitproof/src/main.rs:913`. Step 7 owns the fix. It was missed because
   `validate_disk` has exactly two references in the tree — its definition and that one
   call site — so no unit test reaches it, and `tools/processctl/src/fleet_tests.rs`
   diffs the fleet against a hand-written snapshot that also omits `storage-svc`, so it
   passes. "Observe what is red" cannot surface this defect class before a fleet boot.
2. **The `storage list cursor` probe was coupled to its error message's wording**,
   fixed in `014fcf2` by a shared `cursor_too_long_message()` authority
   (`modules/storage/src/service.rs`). The lesson: a probe matching a message substring
   turns a comment-grade rewording into a red gate whose failure text lies (e.g. "was
   ACCEPTED" when the cap rejected it). `groups`, `notifications` and `friends` compare
   a shared const by inequality; `storage` was the only module with the defect.
