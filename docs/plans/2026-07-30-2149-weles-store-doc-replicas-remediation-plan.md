# Weles remediation: store truth, doc truth, replicas composition proof

**Date:** 2026-07-30 21:49 (revised after an independent `core-reviewer` pass at ultrathink)
**Trigger:** an external review of the Weles commit series (2026-07-21 → 07-29) raising three
points — an unproven `replicas` composition path, `PortAssignment.alive` asserting a semantics
the code does not implement, and a `weles-design.md` that is locally self-contradictory after the
SQLite→redb move. Research (three parallel read-only subagents, 2026-07-30) confirmed all three,
enlarged two of them, and found a fourth. A `core-reviewer` pass then found seven blocking defects
in the first draft of this plan; §"Review corrections" records what changed and why.

---

## Context: what research established

### The three seams this touches

None of this work adds a module, a service, an event, or an admin section, so the
research-before-planning rule's "why not extend X" obligation is narrow: the only *new* artifact
is one verifyctl stage, and Step 6 records why it is a new stage rather than an extension of
`weles-managed-gateway`.

### Finding A — the store is write-only in production (larger than the review said)

Confirmed with call-site enumeration, re-verified by the reviewer:

- `PortAssignment` (`weles/master/src/store.rs:83-92`) has exactly one production construction
  site, `mint_fleet_ports` (`weles/src/supervisor.rs:1096-1114`), which hardcodes `alive: true` at
  both `:1100` and `:1113`.
- `Store::port_assignment()` (`store.rs:200-219`) has **zero** production callers — every call
  site is in `store_tests.rs` (`:110,115,129,185,193`).
- `Store::deploy_record()` (`store.rs:152-170`) *also* has zero production callers — only
  `store_tests.rs:57,63,86` and `prep_tests.rs:265,289`. Its sole production writer is
  `prep.rs:608`.
- The live source of truth for minted ports is the in-memory `minted: Vec<ServiceDef>`
  (`weles/src/supervisor.rs:816-817`), from which `PeerAddrs::from_fleet` (`:849`) derives every
  address.

**The asymmetry that decides the fix:** `alive`'s reader is a *committed* M2 obligation —
`docs/reference/weles-design.md:328` names "port assignments of dead instances" as what the store
exists for, and the doc's resolve-semantics table commits M2 to answering `200 {"addrs":[]}`
(zero live instances) instead of 404. `deploy_history`'s reader is named in **no** document:
`docs/plans/2026-07-21-1540-weles-m1-gaps-replicas-roundrobin-routing-plan.md:314-315` specifies
its columns and assigns it no consumer. One is a placeholder with a date; the other is provenance
nobody planned to consume.

### Finding B — five comments assert behaviour the code does not have

1. `weles/master/src/store.rs:77-81` — "WRITERLESS by design in A3 … A3 deliberately does NOT
   fabricate a synthetic caller". A4 (`4e1eee7`) added the writer at `supervisor.rs:829`.
2. `store.rs:174-175` — "A4 writes this; A3 only defines it … There is no production caller yet
   by design — do not invent one." Same falsehood, imperative mood.
3. `store.rs:63` — `DeployRecord` documented as provenance for `weles deploy`/**`rollback`**;
   `prep::rollback` (`weles/src/prep.rs:639-700`) calls `flip_current` at `:686` and never records.
4. `store.rs:84-85` — documents `instance_id` as `<provider>#<n>`; the writer emits
   `format!("{}:http", def.name)` / `":edge"` (`supervisor.rs:1096,1109`), i.e. `<name>:<kind>` —
   and under `replicas` that is `leaderboard-svc#1:http`. **(Found by the reviewer, not the
   research pass.)**
5. `weles/src/prep.rs:589-594` — "The store owns the SQLite/WAL concurrency contract". False since
   the redb move (`56e0e8e`). It disappears with the function in Step 1, but it belongs in this
   list so the sweep is honest. **(Reviewer.)**

`store.rs:5-20`'s justification is a sixth, subtler case: it argues a multi-writer arbitration
problem, while the two production writers are separate *processes*, which redb rejects rather
than arbitrates (`store.rs:37-50`).

### Finding C — siblings of the same class

- `FleetState.run_id` (`weles/master/src/state.rs:102`) — written at `supervisor.rs:563,770`,
  serialized by `state::checkpoint` (`state.rs:130-143`); **no production loader reads it back**.
  The reviewer settled this by full-crate grep: `weles status`/`down` reach the socket via
  `FleetState.control_endpoint` (`state.rs:107-112`), never by reconstructing it from `run_id`;
  the only live reader is the *in-memory* `Reporter.run_id` (`supervisor.rs:863` →
  `control_endpoint_path`). **Verdict: delete the persisted field.**
- `GenerationManifest.gen` (`weles/src/prep.rs:320`, written `:544-548`) — all three production
  deserializers (`prep.rs:748`, `:861`, `:589`) read `.artifacts`/`.fleet`, never `.gen`; the
  value duplicates the `gen-N` directory name every caller already holds.
- `RolloutLock::path()` (`weles/src/lock.rs:91-93`) — public accessor, zero production callers.

### Finding D — the "strict A,B,A,B cursor" claim is unproven

`4ba7445`'s message, `tools/verifyctl/src/stages/weles_managed_gateway.rs:112-116`, `:594` and
`weles_managed_gateway_tests.rs:241` all cite a strict A,B,A,B cursor as unit-proven in
`core/remote`. `pool_distributes_round_robin_across_two_instances`
(`core/remote/src/tests.rs:1139-1157`) asserts only `hits("A") > 0`, `hits("B") > 0`, and
`hits("A") + hits("B") == 6`. No alternation, no per-call ordering. The cursor *is*
deterministic, so the claim is true — it is simply not the thing the test executes.

### Finding E — the replicas gap, localized

Every hop is individually covered; **no artifact in the tree ever puts `replicas = 2` into a file
weles boots.** Two seams have never touched:

- `expand_service` → `mint_fleet_ports`: `mint_fleet_ports` is only ever fed hand-built defs
  (`weles/src/supervisor_tests.rs:1095-1107`), always one instance per provider.
- `mint_fleet_ports` → live `/resolve`: every `agentapi_tests` map comes from `fleet.split.toml`,
  which contains zero `mint` and zero `replicas`. No live weles boot in the tree has ever minted a
  port or served a two-element resolve.

`weles_managed_gateway_tests.rs:369-387` looks like the missing proof but is not: the encoder and
client are real, the two-address set is a literal in the test (`:377-381`). The splitproof
`[REPLICAS]` scenario (`tools/splitproof/src/main.rs:2576-2675`) hand-clones a `ServiceSpec` and
overwrites ports (`:2594-2599`) — that is processctl, not weles.

**Blocker for the obvious approach:** `replicas` cannot be added to `fleet.split.toml`.
`weles_managed_gateway.rs:780` calls `PeerAddrs::from_fleet` on the *un-minted* fleet →
`Port::resolved()` → panic (`weles/master/src/manifest.rs:195-203`); `:1074` renders `Port::Mint`
via `Display` as the literal string `mint` (`manifest.rs:206-215`), so `wait_fleet_serving` would
poll `http://127.0.0.1:mint/readyz` and burn the full 300 s `BOOT_DEADLINE` — failing slowly and
silently, the worst shape.

**Lock constraint:** `weles::lock::acquire_or_borrow` (`weles/src/lock.rs:412-420`) fails closed
and never degrades to `acquire`; the consumption marker is one-shot **per role**
(`lock.rs:43-46`), cleaned up only on lease drop or delivery failure — reviewer-verified, so a
second live weles boot in one verifyctl run genuinely requires a second role. verifyctl acquires
one lease with `["splitproof", "weles"]` (`tools/verifyctl/src/runner.rs:62`), and
`weles_managed_gateway.rs:302` already consumes `"weles"` (`lock.rs:220` `BORROWER_ROLE`).
This also rules out a `weles/tests/*.rs` live-boot integration test: it would run under the
blocking `test` stage inside verifyctl's own lease, find no credential, call `acquire`, and fail
closed against that lease.

**No per-instance observation exists for an edge-routed op.** `core/metrics` exposes only
`http_requests_total` (`core/metrics/src/lib.rs:80`), which does not move for an op the gateway
dispatches over QUIC (recorded at `weles_managed_gateway.rs:112-116`), and `core/edge` exposes no
`_total` counter at all. This is what forces Step 6's assertion shape — see §"Review corrections",
item 1.

### Finding F — the design doc is a full milestone behind

Beyond SQLite (heading `:295`, body `:322-336`, `:375`, `:829`, and `weles/README.md:70`), the
doc presents as *unbuilt* several shipped mechanisms: port minting (`:114-117`, shipped
`4e1eee7`), `weles rollback` (`:831-833`, shipped `229033a`), the `weles-master` crate boundary
(`:368-373`, shipped `27a6acc`), gateway route discovery via `describe()` (`:267-293`, shipped
`ac567a3`/`4c02c7d` — including a "research this before writing it" question that `databind`
answered), round-robin LB and replicas (`:843-844`, `:767-784`, shipped `a8df081` + C1/C2).

Two entries are worse than stale:

- `:447-450` states `mint_ca` short-circuits when the CA files exist, and concludes that "prep is
  eating the recovery budget" is therefore **not** a valid M2 argument. `prep.rs:1072-1077` says
  the opposite in as many words: "NO idempotency / file-existence short-circuit … `edgeca`
  regenerates the CA each `up`". The conclusion the doc forecloses is live again.
- `:545` cites a blocking fleet-parity stage as present-tense evidence inside a live argument for
  a current decision; no `weles_fleet_parity.rs` exists in `tools/verifyctl/src/stages/`.

Structurally: nine errata blocks, all "errata on top of a stale body", including one bullet
carrying three stacked notes (`:736-758`, `:760-770`, `:771-778`) whose middle layer
(`:761-770`, the single-host replicas recipe) is itself superseded by the `replicas = N` sugar.

### Scope decisions taken by the user (2026-07-30)

1. **Store:** keep `alive` (it has an M2 commitment) with an honest comment; delete
   `deploy_history` entirely.
2. **Replicas test:** the gateway variant — prove a real client, not a fake, consumes the resolved
   set. (See §"Review corrections" item 1 for how the *assertion* had to change once it turned out
   nothing can observe which instance served.)
3. **Doc:** full restructure, flattening the errata stack.
4. **Cursor:** tighten the test to assert real alternation, so the four prose sites become true.

---

## Review corrections (what the `core-reviewer` pass changed)

1. **RP3's live-kill failover assertion is deleted.** It was a race dressed as a proof, in three
   independent ways: weles respawns the killed instance on the *same minted port* after a 1 s
   backoff (`supervisor.rs:1377-1386`, `:142`), so a later 200 proves nothing; `select_excluding`
   advances one step per call (`core/remote/src/lib.rs:939`), so whether the post-kill request even
   *reaches* the corpse depends on cursor parity; and the 5 s ready-probe (`:504`, `is_selectable`
   `:681-684`) may exclude the dead instance before the request, exercising a different property.
   Worse, its first half had no observation at all — both replicas read the same Postgres and
   return identical bodies, and (confirmed independently) **no per-instance counter exists for an
   edge-routed op**. A single-instance pool produces identical 200s. Cross-instance failover stays
   where it is genuinely provable: a `core/remote` unit test with a preset-dead instance (Step 4).
2. **Step 5 gains the missing role transport.** `BorrowCredential` carries
   `{version, lock_path, metadata}` and **no role** (`tools/processctl/src/lock.rs:321-327`;
   weles's mirror says so at `lock.rs:313-318`) — the *child* claims the role, hardcoded at
   `lock.rs:421`. A constant alone would make the new stage either replay `"weles"` or steal it
   from the established one. Step 5 now specifies the CLI flag that transmits it.
3. **Step 6 gains an explicit readiness rule** — the first draft told the implementer to model on
   `wait_fleet_serving`, which is exactly the code that renders `Port::Mint` as `"mint"`.
4. **Step 2's target moves behind a seam created in Step 1.** The branch it was told to test is
   inline in `run_up`, which takes only a root and spawns a 13-process fleet — untestable from a
   `[test-author]` lane without a production refactor that lane may not make.
5. **Step 5's rationale was factually wrong.** `match` and `admin` are also Told by nobody;
   leaderboard is the right pick for a different reason.
6. **Step 3's serde reasoning was backwards.** `GenerationManifest` has no `deny_unknown_fields`
   (`prep.rs:318`), so field deletion is forward-tolerant and backward-fatal, not the reverse.
7. **Step 8 splits into four.** One agent rewriting 845 lines of prose in one commit produces a
   review that degenerates into wording nits — the failure mode the Comments rule names.

---

## Sequence

### Step 1 — Delete `deploy_history`; make the store's comments true; extract the persist seam `[core-implementer, model: opus]`

**(a) What.** `weles/master/src/store.rs`: remove the `DEPLOY_HISTORY` table def (`:60`),
`DeployRecord` (`:63-73`), `record_deploy` and `deploy_record` (`:152-170`). Rewrite the
`PortAssignment` doc block (`:77-92`, including the false `instance_id` format at `:84-85`),
`record_port_assignment`'s doc (`:174-175`), and the module justification (`:5-20`).
`weles/src/prep.rs`: remove `record_deploy_history` (`:589-613`) and its call site (`:562-564`).
`weles/src/supervisor.rs`: extract the mint-persist block (`:825-841`) into a free function.

**(b) Why now / order.** Every later step's truth claim depends on this: Step 8b documents the
store, Step 2 tests the extracted seam, and Step 3 is the same defect class. Doing the doc first
would document a shape about to change.

**(c) How — the non-mechanical parts.**
- **The extracted seam (this is what makes Step 2 possible).** Signature:
  `fn persist_assignments(state_db: &Path, assignments: &[PortAssignment])` — opens the store,
  loops `record_port_assignment`, and keeps the existing log-and-continue on both the open error
  and the per-row error. It returns `()`: the whole point is that a store failure never affects
  boot. `run_up` calls it in place of the inline block, preserving the `if !assignments.is_empty()`
  guard at the call site. Do not thread a `Result` out and then ignore it — that would invite a
  future caller to start depending on it.
- `record_port_assignment`'s comment must name the *actual* current state and the *actual*
  committed reader: rows are written at mint and read by nothing today; M2's liveness answer
  (`200 {"addrs":[]}`) is the committed consumer, and `alive` is the field it will flip. One
  sentence, present tense. The "A3 defines / A4 writes" milestone narrative is exactly the
  changelog-in-code the Comments rule bans — do not replace it with a newer version of itself.
- `alive`'s own doc (`:90-91`) currently claims liveness. It must say what the code does: always
  written `true` at mint; nothing flips it; M2 owns the transition. If that sentence reads like it
  documents a defect, that is correct — it *is* a placeholder, and the honest comment is what
  stops the next reader citing the field as liveness.
- `instance_id`'s doc (`:84-85`) must state the real format `<service-name>:<kind>` and note that
  under `replicas` the name already carries `#N` (`leaderboard-svc#1:http`).
- `store.rs:5-20` must stop asserting arbitration the code does not perform. The true property:
  two *processes* may race for the file, redb rejects the second (`store.rs:37-50`), and both call
  sites log-and-continue. Keep that; delete the rest.
- With one table left, re-derive whether `open` should still create tables eagerly (`:111-127`)
  and whether its write-probe rationale (`:106-109`) still reads correctly. Do not leave a
  two-table justification over a one-table store.

**(d) Verification.** `cargo check -p weles -p weles-master`. Do **not** run tests — Step 2 owns
them, and this machine allows one rollout at a time.

**(e) Commit.** `refactor(weles): delete the reader-less deploy_history, extract the persist seam, make the store's comments true`

---

### Step 2 — Tests for Step 1, including the never-covered mint-persist failure branch `[test-author, model: sonnet]`

**(a) What.** `weles/master/src/store_tests.rs`: delete `deploy_record_round_trips` (`:45-64`) and
`deploy_record_upserts_on_same_generation` (`:66-89`); retarget
`two_writers_disjoint_rows_both_commit` (`:145-200`) and the now-false docstring of
`port_assignment_round_trips_even_without_a_production_writer` (`:91-133`) — there **is** a
production writer. New tests for `persist_assignments` beside `weles/src/supervisor_tests.rs`.

**(b) Why now / order.** After Step 1 has landed and compiles; the seam it tests does not exist
until then. Bundling would put one agent through the whole implementation twice via the
compile/test-fix loop.

**(c) How.** The previously-wrong branch is **a store failure during the mint pass** — research
confirmed no test proves a boot survives an unopenable or unwritable store while minting. With
Step 1's seam this is a unit test: reuse the sabotage technique from `prep_tests.rs:340-364`
(replace the `state.db` path with a *directory* so `Store::open` fails), call
`persist_assignments` with a non-empty slice, and assert it returns normally. Add the
happy-path twin (a real temp path, then read the rows back via `Store::port_assignment` — the one
legitimate use of that otherwise-reader-less method) so the test proves the sabotage case is not
vacuously green.
**Also record, do not silently drop:** `prep_tests.rs:341`'s
`deploy_history_write_failure_neither_fails_the_deploy_nor_skips_retention` pins *two* properties.
Deleting `record_deploy_history` removes the only failure point between the `current` flip and
`prune_stale_generations`, so the "nor skips retention" half becomes vacuous rather than moved.
Note that in the commit message; do not fabricate a replacement for a property that no longer has
a failure mode.
At-risk topology: weles-side, single-host; no split assertion is needed.

**(d) Verification.** `cargo test -p weles-master` and the targeted weles test file, run after
confirming no `cargo`/`rustc` is live (`pgrep -x cargo; pgrep -x rustc`) and
`target/debug/devctl status` reports no fleet.

**(e) Commit.** `test(weles): pin the mint-persist store-failure branch; drop the deploy_history tests`

---

### Step 3 — Sibling sweep: the three other persisted-but-unread surfaces `[core-implementer, model: opus]`

**(a) What.** Delete `FleetState.run_id` (`weles/master/src/state.rs:102`),
`GenerationManifest.gen` (`weles/src/prep.rs:320`), and `RolloutLock::path()`
(`weles/src/lock.rs:91-93`) — all three verdicts are **delete**, settled by the reviewer's
call-site sweep (Finding C). No per-item deferral remains.

**(b) Why now / order.** Same defect class as Step 1, and the Fix-the-Authority rule requires
sweeping for siblings *while the class is loaded in context*. Before Step 8b, which documents
`state.json` and `manifest.json`.

**(c) How.** The one non-mechanical part is the serialization direction, and the first draft had
it backwards. Neither `GenerationManifest` (`prep.rs:318`) nor `FleetState` (`state.rs:99-123`)
uses `deny_unknown_fields`, so deleting a field is **forward-tolerant** (a new binary reading an
old file ignores the extra key) and **backward-fatal** (an old binary reading a new file gets
`missing field gen`). The backward direction is not hypothetical here: `lock.rs:257-260` records
that `weles deploy` stages binaries which may lag the tree. State in the commit message that per
the repo's wipe-not-migrate rule this window is accepted, and name the one asymmetric consequence:
`predecessor_generation` (`prep.rs:748`) swallows a parse error with `continue`, so a stale binary
would *silently* pick an older rollback target, whereas `verify_generation` (`:861`) fails loudly.
Silent-wrong is the one worth a sentence.
Do not add a wrapper to keep any of the three alive; if one would need it, that is the signal to
delete it.

**(d) Verification.** `cargo check -p weles -p weles-master`.

**(e) Commit.** `refactor(weles): delete the persisted-but-unread siblings (run_id, manifest gen, lock path)`

---

### Step 4 — Make `core/remote` prove the two properties its prose claims `[test-author, model: sonnet]`

**(a) What.** `core/remote/src/tests.rs:1139-1157`
(`pool_distributes_round_robin_across_two_instances`) and a new failover test beside it. Then the
four prose sites: `weles_managed_gateway.rs:112-116`, `:594`, `weles_managed_gateway_tests.rs:241`.

**(b) Why now / order.** Independent of Steps 1-3, but it must land **before** Step 6: Step 6's
stage rests on the cursor being deterministic, and — after RP3's deletion — this is now the *only*
place cross-instance failover is proven at all. Step 6 must inherit a true premise.

**(c) How.**
- **Distribution.** Do not extend `Recorder` (`tests.rs:1036-1069`) — it holds
  `HashMap<addr, AtomicUsize>`, counts with no order. `RecordingCaller` already "echoes the address
  back as the response body" (`:1071-1076`), so each `pool.call(...)`'s `Ok(v)` names its serving
  instance. Collect the six values and assert (i) exactly 3 each and (ii) strict alternation
  relative to whichever instance is served first — do not assert a fixed starting instance unless
  the code guarantees one. Be explicit about what each buys: the 3/3 assertion kills a
  "collapse to first selectable" regression; the order assertion kills a batching regression.
- **Failover** (the property RP3 was going to claim). A pool over two instances where instance #1
  is preset dead, driven through `Pool::call`, must be served by #2 within one request — with the
  call sequence asserted per instance so the test proves the *failover branch*
  (`core/remote/src/lib.rs:1111-1127`) executed, not merely that a response arrived. Pin the
  retry-licensing precondition explicitly, since that is what a future contract change would
  break silently.
- Then verify each of the four prose wordings against what the tests now assert and correct any
  that still overclaim.

**(d) Verification.** `cargo test -p remote`.

**(e) Commit.** `test(remote): assert the round-robin cursor alternates and that failover executes its branch`

---

### Step 5 — Replicas fleet, the borrower-role transport, and the stage registration `[core-implementer, model: opus]`

**(a) What.** New `weles/fleet.replicas.toml`. `weles/src/lock.rs`: a second role constant beside
`BORROWER_ROLE` (`:220`). `weles/src/cli.rs`: a `--borrower-role <role>` flag on `weles up`.
`weles/src/supervisor.rs:726` + `main.rs`: thread the role to `acquire_or_borrow`.
`tools/verifyctl/src/runner.rs:62`: extend the lease roles. `tools/verifyctl/src/model.rs:22-24`
and `:50`, plus `tools/verifyctl/src/stages/mod.rs:18,145-151,155,357`: register a new blocking
stage `weles-replicas` with a stub `run` that returns immediately.

**(b) Why now / order.** Step 6 cannot boot anything until the lease can grant it a distinct role.
Landing the plumbing separately keeps a `BorrowerReplay` failure out of the step that writes the
probe logic.

**(c) How.**
- **The role must be transmitted, not merely declared.** `BorrowCredential` carries
  `{version, lock_path, metadata}` and **no role** (`tools/processctl/src/lock.rs:321-327`;
  weles's mirror documents this at `lock.rs:313-318`) — the child claims its own role, hardcoded
  at `lock.rs:421` (`acquire_or_borrow` passes `BORROWER_ROLE` into
  `borrow_inherited_if_present`, whose definition at `:459` already takes an `expected_role`).
  So: add `--borrower-role <role>` to `weles up`'s parser (`cli.rs:66-94`, which today accepts only
  `--dry-run`, `--root`, and `BORROWED_LEASE_ARG` and bails on anything else), validate it with the
  existing `validate_identifier`, thread it `main.rs → run_up → acquire_or_borrow(root, run_id,
  role)`, and default to `BORROWER_ROLE` so every existing caller is unchanged. Update `USAGE` and
  `cli_tests.rs`. Without this, `spawn_borrower(spec, "weles-replicas")` passes the parent-side
  check and the child still claims `"weles"` — replaying the marker
  `weles-managed-gateway` consumed, or stealing it if ordered first.
  Also decide whether `weles_wire_contract.rs` — which pins weles's and processctl's
  `BORROWED_LEASE_ARG` against each other — needs a twin assertion for the role name; if the role
  never crosses that wire, say so in the commit message rather than leaving it unstated.
- **The fleet file** is `fleet.split.toml` **copied verbatim**, with exactly one edit:
  `leaderboard-svc` (`fleet.split.toml:85-91`) gets `http_port = "mint"`, `edge_port = "mint"`,
  `replicas = 2`, `replica_safe = true`. Keep all 13 services and every other literal port.
  Copying rather than minimizing avoids reasoning about which peers `gateway_addrs` bails on
  (`cmd/gateway-svc/src/addrs.rs:385-393`).
- **Why leaderboard.** Three providers in the fleet are Told by nobody — `leaderboard`, `match`,
  and `admin` — so all three would pass `validate_no_told_peer_to_mintable_provider`
  (`weles/master/src/fleet_toml.rs:440-466`). (`gateway` is excluded for a different reason: its
  `player_port` makes `expand_service` bail, `fleet_toml.rs:320-327`.) leaderboard is chosen
  because it is the only one exposing a `#[retry_safe]` `#[http]` op the gateway dispatches over
  the pool (`api/leaderboard/api/src/lib.rs:38-39`) — the shape Step 6 needs.
- **Justify `replica_safe = true` in the file itself.** `expand_service`'s error text
  (`fleet_toml.rs:289-300`) says the flag asserts the operator verified shared-store state. Two
  leaderboard instances both host the durable subscription `leaderboard.match-finished.v1`,
  which nominally collides with "exactly one host per subscription per deployment profile". It is
  safe because the pull worker selects a due subscription `FOR UPDATE SKIP LOCKED`
  (`core/asyncevents/src/worker.rs:207`) and commits effect + checkpoint in one transaction.
  Record that as the basis, in a comment in `fleet.replicas.toml`, so the assertion is not bare.
- **Do not touch `fleet.split.toml`** — `weles_managed_gateway.rs:780` and `:1074` read a fleet
  pre-mint; the split fixture must stay all-literal.
- **Stage registration is three coupled edits, not one.** `stages/mod.rs`'s
  `stage_manifest_is_frozen` asserts the exact blocking name list, the `&names(Level::All)[15..]`
  slice index, and `manifest(Fast, true).len() == 20`. All three change in this commit or the step
  lands red.
- **Position the stage last, after `weles-managed-gateway`,** and say why in the same comment
  style as `stages/mod.rs:145-151`: the newest fleet-booting stage goes last so a wedge in it
  cannot cost an older proof its run. Note that both stages restage `deploy/current` and boot on
  the same literal ports 8080-8092 against the same Postgres, so the two boots must never
  interleave — they are sequential stages within one lease, which is what guarantees it.

**(d) Verification.** `cargo check -p weles -p verifyctl` and `cargo test -p verifyctl stages::`
for the frozen-manifest assertions. Do not run a verify manifest.

**(e) Commit.** `feat(weles,verifyctl): replicas fleet fixture, borrower-role transport, weles-replicas stage stub`

---

### Step 6 — The `weles-replicas` stage: real mint → real resolve → real client `[core-implementer, model: opus]`

**(a) What.** `tools/verifyctl/src/stages/weles_replicas.rs`, replacing Step 5's stub.

**(b) Why now / order.** Requires Step 5's role transport and fleet file, and Step 4's now-proven
cursor and failover.

**(c) How.**
- **Why a new stage, not an extension of `weles-managed-gateway`:** that stage's role is already
  consumed at `weles_managed_gateway.rs:302`, so a second boot inside it replays the marker; and
  its module doc is a carefully-scoped claim about *used-vs-fetched* addresses, which a replicas
  assertion would muddy.
- Follow the sibling's shape: an `Observed` struct captured from the live run and a **pure
  `findings(&Observed) -> Vec<Finding>`** (`weles_managed_gateway.rs:463-512`), so Step 7 can
  unit-test verdicts from staged inputs without a boot.
- Reuse `weles::fleet_toml::load`, `weles::prep::Layout::discover`, `ctx.command("deploy", …)`,
  `ctx.borrow_rollout(spec, <the new role>)`, `runner::os_environment`, and `remote::resolve_peer`
  (verifyctl links `remote` as a real dependency, `tools/verifyctl/Cargo.toml:19`).
- **Readiness rule — the trap this stage exists to disprove.** `fleet_toml::load` runs
  `expand_service` (`fleet_toml.rs:238`), so the loaded fleet has **14** defs, two named
  `leaderboard-svc#1/#2`, both still `Port::Mint` — the mint pass runs inside the child's
  `run_up`, not here. Therefore: wait on `svc.http_port.literal()` for the 12 literal services and
  **skip every `is_mint()` port**; obtain leaderboard's readiness only through RP1→RP2. And never
  call `PeerAddrs::from_fleet` on the loaded fleet in this stage — it panics on `Port::Mint`
  (`manifest.rs:195-203`). Copying `wait_fleet_serving` (`weles_managed_gateway.rs:1066-1085`)
  verbatim reproduces exactly the `http://127.0.0.1:mint/readyz` failure this stage is meant to
  rule out.
- **Four assertions, all pure-state, none clock-dependent:**
  - **RP1 (mint → resolve).** Real `remote::resolve_peer` against the real agent on `AGENT_PORT`
    (8300, `manifest.rs:82`) for `(leaderboard, http)` and `(leaderboard, edge)` returns **exactly
    two** addresses each; all four ports distinct; **none equals 8090 or 9008**. That last clause
    is what makes it falsifiable by construction — it proves the ports were *minted*, not read
    from the file. A `from_fleet` first-match regression yields the same address twice; an
    `expand_service`→mint break yields one.
  - **RP2 (both instances live).** `GET /readyz` returns 200 on each address **learned only from
    resolve** — the stage must have no other way to know the ports. That is the point.
  - **RP3 (both edges are real, independently).** Dial **each** resolved edge address directly
    from the stage with a single-instance `remote` client and require each to answer a real
    leaderboard op over QUIC. This replaces the deleted round-robin-through-gateway assertion:
    it proves the two-element set is two genuinely distinct serving instances rather than one
    address duplicated, which is the composition property at issue — and unlike traffic through
    the gateway, it is directly observable. Needs `EDGE_CA_CERT`/`EDGE_CA_KEY` from
    `run/weles/edge-ca.*`, as `weles_managed_gateway.rs:283-284` already relies on.
  - **RP4 (a real client consumes the set).** The fleet's managed `gateway-svc` boots against the
    replicas fleet and `GET /leaderboard` returns 200 — proving `gateway_addrs` → `nonempty_list`
    → `with_peer_set` → `Pool` construction over a **resolved two-address set** works end to end.
    Assert only the 200: which instance served it is not observable, and the plan must not
    pretend otherwise.
- **Do not** reuse `fake_http::FakeHttp`. A fake agent is precisely what makes the existing proof
  not a weles proof.
- **Do not** kill an instance and infer failover. weles respawns it on the same minted port after
  a 1 s backoff (`supervisor.rs:1377-1386`, `:142`); the assertion would be a race. Failover is
  Step 4's unit test.

**(d) Verification.** Run **only** this stage, after the `pgrep`/`devctl status` check.

**(e) Commit.** `test(verifyctl,weles): weles-replicas stage — replicas=2 through mint, resolve, and a real client`

---

### Step 7 — Unit tests over the stage's verdict function `[test-author, model: opus]`

**(a) What.** `tools/verifyctl/src/stages/weles_replicas_tests.rs`.

**(b) Why now / order.** After Step 6 lands and compiles. `model: opus` rather than the lane
default because the harness is novel — there is no existing replicas stage to copy.

**(c) How.** Mirror `weles_managed_gateway_tests.rs`: stage `Observed` values and assert `findings`
produces the right verdict for each regression it must catch — one address instead of two
(expansion or mint broke); two *identical* addresses (a `from_fleet` first-match regression); an
address equal to the authored 8090/9008 (mint silently skipped); `/readyz` failing on the second
instance only (spawn env not distinguishing the pair — `compose_env_with_fleet`,
`manifest.rs:565-568`); one edge answering and the other not (RP3); `GET /leaderboard` non-200
(RP4). A green `findings` over a fully-healthy `Observed` is the seventh case, and the one that
proves the others are not vacuous.

**(d) Verification.** `cargo test -p verifyctl weles_replicas` — no fleet boot.

**(e) Commit.** `test(verifyctl): stage-input unit tests for the weles-replicas verdicts`

---

### Step 8a — Fold the nine errata blocks into the prose they correct `[core-implementer, model: opus]`

**(a) What.** `docs/reference/weles-design.md`: `:17-56`, `:58-138`, `:147`, `:297-320`,
`:348-351`, `:582`, `:590-594`, `:699-704`, and the three-deep stack at `:736-778`.

**(b) Why now / order.** **After Steps 1-7** — they change what is true about the store, the
persisted shapes, and the replicas proof. Structure first, claims second: folding and rewording
in one pass is what makes a prose diff unreviewable.

**(c) How.** Structural only — no new claims, no corrections of fact (those are 8b/8c). Each
errata block's content becomes the body it corrects, leaving at most one dated historical line.
The three-deep stack is the worst case and its middle layer (`:761-770`, the single-host replicas
recipe) is superseded by the `replicas = N` sugar: collapse all three into one current statement
plus a single "superseded designs" line. Historical narrative that is *labelled* as history stays.

**(d) Verification.** `cargo run -p verifyctl -- --stage docs-current` (the stage walks
`collect_reference_markdown(&root.join("docs/reference"), …)`,
`tools/verifyctl/src/stages/docs_current.rs:44`, so it does cover this file).

**(e) Commit.** `docs(weles): fold the errata stack into the prose it corrects`

---

### Step 8b — Present-tense the shipped mechanisms `[core-implementer, model: opus]`

**(a) What.** `weles-design.md`: the store section including the heading `:295`, `:322-336`,
`:375`, `:829`; port minting `:114-117`; rollback `:831-833`; the `weles-master` crate boundary
`:368-373` (the doc never names the crate); gateway route discovery `:267-293`; the M1 lists
`:786-844` incl. `:843-844`'s three wrong "Not in M1" entries; `:789,793`'s "twelve services on
static ports"; `:157-160`'s "the managed-mode proof is the ONLY thing gating interop"; `:635-637`'s
claim that a `supervisor.rs` comment is false (fixed in `45f07fb`); the status header `:8-15`.
Module paths that moved to `weles/master/` (`:20-21`, `:236-239`).

**(b) Why now / order.** After 8a, so each rewrite lands on a single body rather than a body plus
its correction.

**(c) How.** Present tense, one claim per sentence. Two additions belong here: the store's actual
shape after Steps 1 and 3, and the replicas proof Steps 5-7 create — replacing the known-gap note
added in `632c423`. Delete, rather than rewrite, the answered research question and the interim
stub-diff tripwire at `:288-293`; the tripwire never existed and the question was answered by
`databind`.

**(d) Verification.** As 8a.

**(e) Commit.** `docs(weles): rewrite the shipped mechanisms in present tense`

---

### Step 8c — The two wrong conclusions and the two unmeasured claims `[core-implementer, model: opus]`

**(a) What.** `weles-design.md:447-450`, `:545`, `:432-438`, `:444-446`.

**(b) Why now / order.** Its own step because it is the only doc change where the *surrounding
argument* changes, so it deserves its own review pass rather than being buried in a large diff.

**(c) How.**
- `:447-450` claims `mint_ca` short-circuits on existing CA files and concludes the "prep eats the
  recovery budget" argument is therefore invalid for M2. `prep.rs:1072-1077` states the opposite —
  no short-circuit, the CA is regenerated every `up`. Swapping the noun is not enough: the
  conclusion the doc forecloses becomes live again, and the doc must say so.
- `:545` cites a blocking fleet-parity stage as present-tense evidence inside a live argument for
  the QUIC decision. No such stage exists. The conclusion may survive; the evidence must go.
- `:432-438` ("a cold start … is seconds") and `:444-446` ("Σ not max") are unmeasured performance
  claims — nothing in the tree measures either, and `:447-450`'s reversal makes the first likelier
  false. **Delete rather than correct.**

**(d) Verification.** As 8a.

**(e) Commit.** `docs(weles): correct two reversed conclusions and delete two unmeasured claims`

---

### Step 8d — The three READMEs and the parity note `[core-implementer, model: sonnet]`

**(a) What.** `weles/README.md:38-43` (missing `weles rollback`), `:46` vs root `README.md:193`
(thirteen vs twelve — the two disagree), `weles/README.md:68-72` ("M1 … is not started" — false on
all four counts), `README.md:182-186` (command list missing `rollback`),
`docs/reference/weles-fleet-parity.md:10` (`weles/src/fleet_toml.rs` → `weles/master/src/`).

**(b) Why now / order.** Last, and mechanical once 8a-8c settle the wording. `[sonnet]` because
every edit here is a stated correction with a known target — no judgment left.

**(c) How.** Make the two service counts agree by deriving both from `fleet.split.toml`'s actual
`[[service]]` count rather than restating a number in two places if the surrounding prose allows
it; otherwise state 13 in both and say where the number comes from.

**(d) Verification.** As 8a.

**(e) Commit.** `docs(weles,readme): correct the status claims, service count, and moved paths`

---

## Known gaps this plan deliberately does not close

- **M2 liveness.** `alive` stays a placeholder with an honest comment. Flipping it requires the
  instance-liveness mechanism the design doc defers to M2; inventing a writer now would be the
  synthetic caller the (deleted) comment warned against.
- **Which instance served a gateway request is not observable.** No per-instance counter exists
  for an edge-routed op (`core/metrics/src/lib.rs:80`; `core/edge` exposes none), so RP4 proves
  the pool *works* over a resolved two-address set, not that traffic *spread* across it in the
  live fleet. Spread is proven at unit level (Step 4). Closing this for real means adding a
  per-instance edge counter — a `core/metrics` change, out of scope here.
- **Cross-process store rejection is still only modelled in-process.** `store_tests.rs:211-235`
  proves the reject class with two handles in one process; nothing exercises `weles deploy` racing
  `weles up`'s mint pass. Step 2 covers the *consequence* (boot survives a store failure), not the
  race.
- **`prep_tests.rs:341`'s retention half becomes vacuous.** Step 2 records this rather than
  fabricating a replacement for a property that no longer has a failure mode.
- **Multi-host replicas.** Everything here is single-host loopback; the design doc's multi-machine
  sections stay flagged as designed-not-built.

---

## Dispatch summary

| Step | Lane | Model | Kind |
|---|---|---|---|
| 1 | `core-implementer` | opus | impl (store + persist seam) |
| 2 | `test-author` | sonnet | tests for Step 1 |
| 3 | `core-implementer` | opus | impl (sibling sweep) |
| 4 | `test-author` | sonnet | tests (cursor + failover) |
| 5 | `core-implementer` | opus | impl (fleet, role transport, registration) |
| 6 | `core-implementer` | opus | impl (stage) |
| 7 | `test-author` | opus | tests for Step 6 |
| 8a-8c | `core-implementer` | opus | docs |
| 8d | `core-implementer` | sonnet | docs (mechanical) |

Each implementation step gets one independent `core-reviewer` pass before the next is dispatched.
Steps 5-7 additionally warrant a `proof-auditor` pass, because they add a verify stage — the gate
is itself the risk surface there.
