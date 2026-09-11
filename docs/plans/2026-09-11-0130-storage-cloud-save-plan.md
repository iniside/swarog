# `storage` — player cloud-save (P1#7)

**Status:** revision 1, awaiting review.

Tracker row: `docs/roadmap/feature-tracker.md:206` — *Generic storage objects /
player cloud-save (KV+OCC) ❌ — P1#7. Self-contained new fortress.*
The gap analysis already decided the shape (`docs/reference/game-backend-feature-gaps.md:194-198`):
*"collection/key, per-user + public, read/write permission levels, OCC via version …
new `storage` fortress, its own schema, a permission + version(OCC) model;
player-facing CRUD ops. Self-contained."*

---

## Context — the overlapping systems, and why a new fortress

Required by the house rule before proposing a module. Each was read, not assumed.

- **`config`** — operator KV with a revision. **Why not extend:** no owner column
  (PK is `(namespace, key)`), no authenticated write path at all (`ConfigSnapshot` is
  wire-only with no `Identity`, and `set` is deliberately off the shared trait), and a
  **global singleton revision** whose only job is invalidating a whole-store cache held
  in every process. One player's save would force a full config re-read and a
  full-snapshot RPC in every replica, on a store sized for tens of operator knobs.
- **`inventory`** — the closest per-player mutable state. **Why not extend:** its value
  is a single CHECK-bounded `bigint` quantity keyed to a foreign-keyed catalog row, so
  it can only ever store counts of pre-registered items — never a caller-chosen key or
  an opaque document. No version, no paging, and its only player write is the dev-gated
  simulated IAP.
- **`accounts`** — **why profile fields do NOT belong here.** `display_name`/
  `discriminator` back a cross-player uniqueness invariant and are served to other
  modules through the wire-only `Directory`. Avatar and language are attributes *of the
  identity*, so they stay an `accounts` edit. Conversely, cloud-save values are opaque
  to the server, unreadable by the directory, and unbounded in count.
- **`notifications`** — the closest *structural* precedent (per-player rows, keyset
  paging, contract-side caps, `NotFound`-not-`Forbidden`). **Why not extend:** its rows
  are server-authored, append-only, addressed by an opaque server-minted id, and
  time-pruned by design. Cloud-save is client-authored, updated in place under CAS,
  addressed by a caller-chosen key, and must not expire.
- **`apikeys`** — **not extended, but it is the OCC design source.** It already
  implements CAS-by-`revision` (`modules/apikeys/src/store.rs:296-327`) with the rule
  this plan copies verbatim: *a miss is `Conflict`, never a not-found.* It is operator
  credentials with no owner column and no player-facing op, so `storage` shares the
  idiom and no table.
- **`wallet`** — cited for its idempotency-key discipline and its byte-cap pairing,
  depended on for neither. Its concurrency question is "did I already apply this
  request?"; `storage`'s is "is the value I am overwriting the one I read?" — a lost
  update an idempotency key cannot detect.

---

## Findings that shape the design

**S1 — `storage` is the *eighth* `player.deleted` consumer, so its purge half is a
copy, not a design.** The account-deletion rollout (`79cf52d`..`e90985a`) landed a
tombstone table, a budgeted sweep and a write guard in six modules, through six rounds
of adversarial review that produced five defects nobody predicted. The copy source is
`inventory` (pure delete-by-owner, no cascade emit) with `characters`' doc comments.

**S2 — but three parts of that pattern do not transfer.**
1. **The ACL half is a `friends`-shaped problem.** Grant rows are keyed by *grantee*,
   so a sweep deleting `WHERE owner_id = $1` leaves grants **to** the deleted player on
   other owners' objects. That needs the both-sides predicate
   (`WHERE $1::uuid IN (owner_id, grantee_id)`), and it puts `storage` in the
   `friends`/`groups` bucket of the residual guard race: an orphan grant is *visible to
   the surviving object owner*, not an unreadable dead row.
2. **Per-player work is not naturally bounded.** The six landed modules have domain
   caps (character limit, catalog-bounded balances, `MAX_MEMBERS`); a caller-authored
   document store has none. Settled by D5 below.
3. **No `wallet`-style keep-under-sentinel.** The ledger survives because it *is* the
   dedup registry. A version column is not an idempotency registry — a reused
   `(owner, collection, key)` after deletion is a new document — so `storage` deletes.

**S3 — `CONTRACT_ONLY` is empty, so there is no free contract-before-module window.**
`groups` emptied it. Landing `api/storage/*` with `#[http(` while `modules/storage`
does not exist **fails archcheck rule 21**. Two honest shapes: one commit, or an
explicit `CONTRACT_ONLY` entry deleted in the module commit (leaving it is a stale
violation). Rule 21 also only protects `#[http(`-bearing domains — a wire-only contract
crate with a divergent module name is invisible to it and to all five served gates.

**S4 — `modules/storage/Cargo.toml` is the detonator.** The instant that file exists —
even as an empty stub — archcheck rules 12/15/17/21, `checkmodules`, `opscatalog-gen`,
`csharp-client-gen` and `conformance` all switch on in the same commit. There is no
"create the crate now, wire it next".

**S5 — the write path's ambiguity is structural.** A CAS `UPDATE` that matches zero
rows cannot distinguish "not yours", "no such object" and "stale version". Settled by
D4.

---

## Decisions settled here

| # | Decision | Rationale |
|--:|---|---|
| D1 | **Address is `(owner_id, collection, key)`** — the owner is part of the address | Owner's decision. A public reader must know the owner to name the object, so the leak is in the *addressing scheme*, not the response — the same shape as `leaderboard` publishing `Score.player`. Recorded below as an accepted oracle. |
| D2 | **No time-based pruning** | Owner's decision. A cloud save must not evaporate. Objects live until the owner deletes them or the player is deleted. The cost, recorded: nothing else collects them. |
| D3 | **Public read widens the visibility predicate** | Owner's decision, derived from the repo's rule rather than a precedent — no module here has ever had per-object ACL. One statement: a nonexistent object, another owner's private object, one not shared with me, and a malformed id **all collapse to the same `None`**. A non-owner's *write* is also `NotFound`, never `Forbidden`, or the oracle returns through the write path. |
| D4 | **Two statements in one transaction on a write miss** | Owner's decision. CAS `UPDATE` first; on zero rows, an **ownership-scoped** `SELECT` in the same transaction. Row present → `Conflict` (yours, version stale — retry with the new one). Row absent → `NotFound` (missing *or* not yours, indistinguishable). The one shape that gives the owner the 409 an OCC client needs while keeping a stranger at 404; the disambiguating read is itself ownership-scoped, so it is not an oracle. It must be **in** the transaction, or a concurrent delete turns a real conflict into a 404. |
| D5 | **A per-owner object cap, enforced at the write path** | Owner's decision, closing S2.2. **Non-negotiable consequence:** a cap counted by a statement is a count-then-insert, so it inherits the advisory-lock obligation — `modules/groups/src/store.rs:59-79` states the rule and `MAX_MEMBERS` is the precedent. Under READ COMMITTED two concurrent writes both read `cap-1` and both land past it. One `pg_advisory_xact_lock` keyed on the owner, namespaced distinctly, taken before the count. |
| D6 | **`version` is monotone for the address's lifetime** | `version = version + 1`, never assigned, never reset, never decremented. `mail` grew a `generation` column because a requeue reset `attempts` and made a superseded attempt's value reachable — and its own plan records a test that *claimed* to pin the CAS leg and did not. A future operator "restore" writes the old **value** at `version + 1`, never the old number. |
| D7 | **`acl` is a named closed-set string**, validated in the service | `bool` is not in the C# type lattice. The precedent for a boolean that cannot be a boolean is `read_at: String` where empty means unread; the precedent for a closed-set token is `join_policy`. Extensible to a future `public_write` without a wire break. |
| D8 | **`expected_version = 0` means unconditional**, `< 0` is `Invalid` | No `Option` on the wire. `resolve_limit`'s `0 means unspecified, negative is the one rejection` is the precedent, and `0` is unreachable as a real expectation because a live row starts at `1` and only rises (D6). Stated in the **contract crate**, next to the field, not only in the impl. |

### The oracle we accept, stated honestly

D1 means a public object's address contains its owner's id, so anyone who can read a
public object already knew whose it was. That is a property of the addressing scheme,
accepted deliberately. The alternative — an opaque uuid as the public address, with
`(owner, collection, key)` reserved for owner-scoped ops — changes the primary key and
was not chosen.

### What does **not** survive a deletion, and what does

`storage` is the eighth purge consumer, so a deleted player's objects and the grants
they authored are swept. **Grants *to* them on other owners' objects are swept by the
both-sides predicate (S2.1); an orphan produced by the residual guard race is visible
to the surviving object owner.** The event log retains `player.deleted` for 90 days,
and `audit.log` for 30 — neither is redacted, as the deletion plan records.

---

## Contract shape

```rust
// api/storage/api/src/lib.rs — consts
pub const MAX_COLLECTION_BYTES: usize = 64;
pub const MAX_KEY_BYTES: usize = 128;
pub const MAX_VALUE_BYTES: usize = 65_536;      // mail's body cap is the precedent
pub const MAX_OBJECTS_PER_OWNER: i64 = 10_000;  // D5
pub const MAX_CURSOR_BYTES: usize = 256;
pub const MAX_PAGE_LIMIT: i64 = 100;
pub const DEFAULT_PAGE_LIMIT: i64 = 25;
pub const ACL_PRIVATE: &str = "private";
pub const ACL_PUBLIC_READ: &str = "public_read";
```

Ops on one `#[rpc(prefix = "storage")] pub trait Player`, every method leading with
`Identity`:

| op | verb / path | retry_safe | notes |
|---|---|:--:|---|
| `put` | POST `/storage/{collection}/{key}` | no | `value: String` (serialized JSON), `expected_version: i64`, `acl: String` |
| `get` | POST `/storage/{collection}/{key}/get` | **yes** | POST because the grammar has no query source and a body is needed for nothing else — keep it POST for symmetry with `list` |
| `delete` | DELETE `/storage/{collection}/{key}` | no | a replay is a 404, the repo's uniform shape |
| `list` | POST `/storage/list` | **yes** | `collection`, `cursor`, `limit` in the **body** |

`DeleteTicket`-style DTOs are `String`/`i64` only — no `bool`, no `Option`, no `u32`
(`tools/csharp-client-gen/src/scrape.rs:429-432`). An arbitrary JSON value is
expressible **only** as a `String`, bound as text and cast `::jsonb` in SQL; it is
validated in the service before the statement, because an unmapped `22P02` from the
cast is a 500 where the contract promises 400.

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
    CONSTRAINT storage_value_len_check      CHECK (octet_length(value::text) <= 65536),
    CONSTRAINT storage_acl_check            CHECK (acl IN ('private','public_read'))
);
CREATE INDEX IF NOT EXISTS storage_list_idx
    ON storage.objects (owner_id, collection, created_at DESC, key DESC);
```

`collection` and `key` sit in the **primary key**, a btree, so an uncapped value raises
`54000` — an *unmappable 500*, not the 400 the contract promises
(`modules/notifications/src/service.rs:87-93`, `:242`). The Rust caps and the named
`octet_length` CHECKs are paired in a `ColumnCap`/`COLUMN_CAPS` table
(`modules/apikeys/src/store.rs:85-131`) so the two cannot word one limit two ways, and
both directions are pinned by tests (`modules/apikeys/src/store_tests.rs:582-615`).

---

## Step 1 — contracts + module skeleton, ONE commit  `[opus]`

**(a)** `api/storage/{api,events,rpc}`, `modules/storage/{Cargo.toml,src/{lib,store,service}.rs}`,
`cmd/storage-svc`, and every registration S4 lists.

**(b) Why one commit.** S3: `CONTRACT_ONLY` is empty, so a contract crate with
`#[http(` and no module **fails archcheck rule 21**. S4: the module's `Cargo.toml` fires
five more gates the same instant. Splitting them means either a knowingly-red tree or a
`CONTRACT_ONLY` entry that must be deleted again — the sequencing defect the last two
plans each hit once.

**(c)** Copy `api/groups/*` and `modules/groups` for structure. `define(` must be one
line; no `Option` fields on event payloads. Registrations, all in this commit:
`cmd/server`, `cmd/gateway-svc` (`Stub::describe_peer("storage", …)`),
`tools/checkmodules` Split profile, workspace `Cargo.toml` (five entries).

**Red after this step — observe, do not predict.** Expect `codegen-freshness`,
`conformance`, `contract-golden`, `public-api`; Step 2 closes them. **Run the gates and
report verbatim.**

**(d)** `[opus]` — core-implementer, *think hard*.

## Step 2 — the gates  `[opus]`

**(a)** `opscatalog-gen`'s `rpc_modules()`, `csharp-client-gen`'s `PROVIDERS` +
`phase_a()`, the regenerated artifacts, the four `testdata` goldens, the hand sets in
`tools/csharp-client-gen/src/tests.rs`, `DEV_CLIENT_POLICY`, `conformance`'s
`policy.rs` entry + `input_policies()` rows, and three blessings.

**(c) Order, derived by execution in two prior rollouts:**
1. regenerate `opscatalog/src/generated.rs` — **in the same commit** extend
   `DEV_CLIENT_POLICY`, whose reverse test iterates the *generated* catalog and is
   green until this moment; missing it is a runtime 403 for `dev-key-client` on every
   storage op in both topologies, with every gate green.
2. regenerate `clients/csharp/Generated/`, then re-bless the four `testdata` goldens.
3. **Only then** update `golden_covers_the_known_surface`'s counts (37/19/8 today) and
   **both** `BTreeSet` literals — it reads the committed testdata, not the live scrape,
   so it reddens only after step 2.
4. `--bless-input-golden`, `--bless-contract-golden`, `--bless-public-api`.

**Conformance stances must be executed, not prose.** `NotApplicable { why }` is checked
only for non-blankness — it is the single largest hole for a new module. If `storage`
implements `AdminSubmit`, `admin_submit_findings` forces `ADMIN_SUBMIT_MODULES` **and**
a non-empty `CapCase`. **Prove one CapCase bites**: break the guard, read the named
failure verbatim, restore, confirm with a checksum.

**After every `--bless-*`, diff the directory** — a bless accepts whatever the tree
produces.

**(d)** `[opus]`.

## Step 3 — the CAS write path  `[opus]`

**(a)** `put`, the owner cap, the advisory lock, and the D4 disambiguation.

**(c)** Non-negotiables:
- **One statement for the CAS**, `UPDATE … WHERE owner_id = $1 AND collection = $2 AND
  key = $3 AND version = $5 RETURNING version`. No advisory lock is needed *for the CAS
  itself*: when it blocks on a concurrent writer's row lock it re-applies its own
  qualifier to the newly committed row, so the loser matches zero rows. That
  re-application is the entire guarantee.
- **But D5's cap is count-then-insert**, so the owner lock **is** required and is taken
  before the count. `modules/groups/src/store.rs:59-79` is the precedent and the
  reasoning.
- **Never** `SELECT version` → compare in Rust → `UPDATE`: that is the count-then-insert
  bug in disguise and produces a silent overwrite.
- **Never** `SELECT … FOR UPDATE` then an unconditional `UPDATE`: it serializes
  correctly and then the loser *succeeds*. The 409 comes from the predicate.
- The write predicate uses **`owner_id = $1` alone** — the read's public-read
  disjunction must be a *separate string* with a doc comment saying why, or
  `public_read` becomes public *write* with no error anywhere.
- Validate the JSON, the `acl` token, `expected_version < 0`, and every byte cap **in
  the service, before the statement**.

**(d)** `[opus]` — *think hard*.

## Step 4 — reads, ACL and paging  `[opus]`

**(a)** `get`, `list`, the visibility predicate, the cursor codec.

**(c)** The read predicate is one statement widened by a disjunction, D3's shape, so
four different "no" answers collapse to one `None` mapped to a single `NOT_FOUND` const
in the service. The keyset codec is copied per-module, not shared: base64url-no-pad,
calendar-validated, cap checked **before** the base64 decode, malformed ⇒ `Invalid`
never a silent page-1 reset. **`key` is text, not a uuid**, so the `|` separator must be
excluded from the legal key alphabet or the second half must be encoded separately —
`split_once('|')` takes the first `|`, and a key containing one round-trips truncated,
which is a silent wrong page, not an error. Both existing codecs have a uuid there and
cannot hit this.

**(d)** `[opus]`.

## Step 5 — the purge half  `[opus]`

**(a)** `storage.purged_players`, the tombstone subscription, the sweep, the write
guard, the admin KPI.

**(c)** Copy `modules/inventory/src/projection.rs` (delete-by-owner, no cascade emit)
with `modules/characters/src/projection.rs`'s doc comments. Reuse the shared
`schedulerevents::schedule_names::PLAYER_PURGE` — **no new schedule row, no
`seeded_schedule_names_are_contract` edit**. Two globally-unique subscription ids,
both `Genesis`. `ORDER BY storage.purged_players.player_id` — **qualify the table**;
unqualified it binds to the `::text` output column and the floor degrades to a seq scan
with an on-disk sort inside the delivery transaction (~1850×, measured). Budget at the
player boundary, after the work *and* the stamp. Add `"storage"` to the two hard-coded
schema arrays in `tools/splitproof/src/main.rs`.

**The ACL table needs the both-sides predicate** (S2.1) — `inventory`'s template is
insufficient there; `friends`' `$1::uuid IN (low_id, high_id)` is the shape.

**(d)** `[opus]`.

## Step 6 — the admin page  `[opus]`

`ADMIN_SECTION` beside its siblings; every self-link from `adminapi::slug(LABEL)`,
never the item id; `admin_data` **must never `Err`** — render an error card, because the
portal forwards every page's params to every provider and an `Err` degrades an
*unrelated* page in the split; rejections map `Stale → Conflict`, `Rejected → invalid`,
`Internal → internal`, **never `NotFound`**, which the edge makes indistinguishable from
`UnknownMethod`. Copy `friends`' read half. Include the *Purges Pending* KPI.

## Step 7 — fleet and topology  `[sonnet]`

`storage-svc` takes **HTTP 8097 / edge 9016** (verified: 8096 and 9015 are the current
maxima, 9013 is gateway's own edge). `tools/processctl/src/fleet.rs` row **and** the
`FleetSpec::new` vec (order is boot order, a dependency must be strictly earlier);
`cmd/gateway-svc/src/addrs.rs`'s `ADDR_SPECS` **plus** its `FLEET` and `FakeAgent`
tables — and **rename `managed_resolves_the_same_twelve_pairs_as_env`**, whose count
lives only in the test name; `cmd/admin-svc` lib+main; `weles/fleet.split.toml` placed
before its consumers; `weles/master`'s `17 → 18` and the env-manifest golden.
Session budget: +6 → 103 against 131. Fits.

## Step 8 — split-proof  `[proof-auditor]` then `[opus]`

**Audit the assertion spec first, then write it.** That sequence caught four vacuous
shapes in the last rollout — a zero-count that passes when the fixture never seeded, a
cursor compare on a column that does not exist in the assumed shape, a re-mint check
identical with and without the mechanism, and a `!= 200` that waves through a 503.

Required per consumer, **joined with "and"**: an exact non-zero pre-count; a survivor
seeded identically and untouched; **both** subscription cursors at-or-past, in the
**positive** form (`count(*) = 1`) — the negative form passes vacuously for a deleted
consumer, and that one line is the rollout's only defence against a missing consumer.

Plus: a CAS conflict returning **409 to the owner** and **404 to a stranger** (D4); a
public object readable by a non-owner while a private one is 404; the owner cap
refusing at `MAX_OBJECTS_PER_OWNER + 1`; and the `[DL*]`-shaped purge proof.

**Prove one assertion non-vacuous**, perturb-observe-revert with a checksum.

## Step 9 — module unit tests  `[test-author]`, `model:"sonnet"`

Each names its previously-wrong branch. The fourteen wrong-result-not-error sites the
research enumerated are the checklist: ownership compared after the fetch; the read
predicate reused on the write path; a CAS on a resettable column; a restore writing the
old version number; delete-then-recreate resurrecting `version = 1` against an in-flight
expectation; `expected_version = 0` read as a literal; a negative expectation folded
into the conflict path; an empty `acl` silently meaning private; malformed JSON reaching
the cast; uncapped `collection`/`key` raising `54000`; an uncapped value; `22P02` on
`owner_id` surfacing as 500; the disambiguating SELECT outside the transaction;
`version` as `i32`.

Confirm **zero `SKIP: postgres unreachable` lines** and say so.

## Step 10 — acceptance  `[inline]`

`--fast`, then `--all --strict`. Redirect and **read the stage table** — a trailing
`echo $?` reports the echo's status, which produced a false green on 2026-09-10.

## Step 11 — documentation  `[docs]`, `model:"sonnet"`

`docs/roadmap/feature-tracker.md:206`, `docs/reference/game-backend-feature-gaps.md`
(archive — append errata, do not rewrite), `CLAUDE.md` (module list, fortress count
16 → 17, the split-proof port sentence and assertion families),
`.agents/shared/gamebackend.md`, `AGENTS.md`, and this plan's errata.

**The known-gap list must be complete or it is silence implying coverage:** the D1
addressing oracle; nothing collects objects but the owner and the purge (D2); the
residual guard race putting an orphan grant in the surviving owner's view; no
value-indexed query (a JSONB index is a later feature); no object sharing beyond
`public_read`; no per-collection TTL; and the two pre-existing conformance defects the
deletion rollout recorded, which this module inherits.

---

## Non-goals (deliberate, recorded)

- **Value-indexed queries.** Nakama's storage indexes are a separate feature; a
  `jsonb` GIN index over caller-authored documents is an unbounded write amplifier.
- **Object sharing beyond `public_read`.** A grantee table is a second authorization
  path and a second oracle surface in a module that does not exist yet. D7's closed-set
  string is extensible to it without a wire break.
- **Per-collection TTL.** Contradicts D2 for the general case; revisit when a concrete
  cache-shaped collection exists.
- **Cross-owner atomic writes.** One object, one statement, one version.
