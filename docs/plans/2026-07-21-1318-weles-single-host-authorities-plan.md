# Weles: fix the root authority + audit single-host assumptions (post-review revision)

**Date:** 2026-07-21 13:18 · **Revised** after grumpy-reviewer pass (opus, think hard).
**Scope decided with user:** fix single-host *authorities* now while the crate is small, so
later M's depend on a clean base; fold in the remaining review findings (Told/Asks validator,
`topology`→`fleet_label`); produce a complete audit of single-host assumptions as an explicit
known-gaps section rather than implementing multi-machine.

**Key revision from review (Finding 2) + user decision (placement, not raw host):** the
*address* is NOT an authority weles can fix now. The design (`weles-design.md:161-166,232-233`)
classifies addresses as **AUTOMATIC — runtime state the agent owns**; at machine two `resolve`
returns real `host:port` from the agent's observed IP, never an operator-authored literal. A raw
`host` field in `fleet.toml` would MOVE the `127.0.0.1` literal into TOML and create a *second,
conflicting* address authority — the "Fix the Authority" trap. So `service_addr`'s loopback
literal is **correct for single-machine**; the real host authority is the future agent resolve.

The user chose the reviewer's alternative: model the manifest datum as **placement** — the one
address-adjacent thing the design DOES sanction as a manifest annotation (`weles-design.md:245`:
"Placement is a manifest annotation, not scheduling"). Placement names which node/agent runs a
service; the address is still agent-observed. On a single machine (master≡agent, one node)
placement is degenerate → loopback, so `service_addr` is untouched. Step 2 adds the placement
annotation as the design-correct, fail-closed seam (so no one later adds a raw `host`).

**Honest caveat (recorded, see Step 2):** on single-machine, placement has NO behavioral effect
on host derivation — its only live consumer today is validation (fail-closed on premature
multi-node use). This is a forward-looking design-alignment seam, not a behavior change. The
leaner alternative was to drop it and document only; the user chose to plant the typed seam now.

---

## Context — authorities mapped by a 10-subagent whole-repo survey
Method: rg-anchored + targeted end-to-end reads (no LSP needed — small non-macro crate).

### Authority A — fleet root (compile-time leak) — THE fix
- **Two duplicated derivations**, both `env!("CARGO_MANIFEST_DIR").parent()`:
  - `weles/src/main.rs:175-181` `state_path()` → `<root>/run/weles/state.json` (status/down).
  - `weles/src/supervisor.rs:623-628` `workspace_root()` → `prep::Layout::discover` (up/deploy;
    root threaded into the `rollout.lock` path, `Reporter.state_path`, prepare-hook cwd).
  Identical today by construction only — nothing enforces the identity.
- **Cross-tool constraint (reviewer Finding 1):** the root also fixes `<root>/run/rollout.lock`,
  which MUST stay byte-identical to its siblings so the one-Postgres mutual exclusion holds:
  - `tools/devctl/src/supervisor.rs:750-756` — root = `CARGO_MANIFEST_DIR.parent().parent()`
    (always repo root, **cwd-independent**).
  - `tools/verifyctl/src/runner.rs:357-364` — **walks cwd up to a `Cargo.toml` +
    `tools/processctl` marker** (repo root from any subdir).
  A flat-cwd weles root would diverge from these and silently defeat the lock.
- `std::env::current_exe()` not used for root today (test fixtures only). `cli.rs` has no root
  flag; parses per-verb, rejects unknown `--` flags (`cli.rs:67-75`).
- **Verified SAFE, do not touch:** the verifyctl→weles borrow + managed-gateway spawn set
  `cwd: ctx.root` (`weles_managed_gateway.rs:617,770`) and the borrow credential carries a
  trusted `lock_path` (`lock.rs:51-57`) — cwd-robust. The break is confined to the human
  `acquire` path (`lock.rs:108-128`).
- **Test-only** `CARGO_MANIFEST_DIR` (`fleet_toml.rs:344-358`, `prep_tests.rs:133`,
  `weles/tests/prep.rs:23-25`) locates fixtures — **stays**.

### Authority B — service address — NOT a current-code authority; placement is the manifest seam
- `weles/src/manifest.rs:252-258` `service_addr()` → `format!("127.0.0.1:{port}")` is the single
  address formatter, and its loopback is **correct** on one machine (the agent is local; resolve
  is loopback). Per `weles-design.md:161-166,232-233`, the multi-machine address authority is the
  **agent's resolve answer (automatic)**, not the manifest. ⇒ `service_addr` is NOT changed.
- The design-sanctioned manifest datum is **placement** (`weles-design.md:245`), NOT an address.
  `ServiceEntry`/`ServiceDef` gain an optional `placement` (Step 2); single-machine legal values
  are absent or a reserved local sentinel, anything else fails validation closed (no node
  registry exists yet). host derivation from placement is the multi-machine seam (Step 5 doc).
- (Sibling loopback literals — `agent_url`/`agentapi` bind `:496`, `health::probe`/
  `ensure_no_stale_listener` — all **correctly** single-host: the service→agent hop stays
  loopback even multi-machine per `weles-design.md:351-369`; health probes an agent's own
  locally-spawned children. Untouched.)

### Authority C — Told/Asks replica asymmetry (latent bug) — fix
- `peer_addr` (`manifest.rs:268`, Told env path) uses `fleet.iter().find(...)` → silently the
  **first** match; `PeerAddrs::from_fleet` (Asks path) returns **all** instances (correct).
- `fleet_toml::validate` (`:246`) checks unique ports / unique names / peer existence+kind+
  boot-order — **none** enforce provider-uniqueness. A `fleet.toml` with two
  `provider="characters"` validates today. Repo convention = loud fail-closed (`bail!`).

### The `topology` vestige — rename
- `FleetState.topology` (`state.rs:104`) + `Reporter.topology` (`supervisor.rs:532`) no longer
  branch behavior — pure label; local var `fleet_label` already exists. ~23 sites, 6 test files,
  no serde attr, no golden/baseline, wipe strategy for `state.json`.

### Scoping guardrails — do NOT touch
- `run/rollout.lock` (weles hand-copy of processctl) — inherently single-machine, must stay
  byte-compatible (`ROLLOUT_LOCK_VERSION`/`BORROWED_LEASE_ARG`/`CONSUMED_MARKER`).
- weles's own agent bind, health probes, processctl/splitproof/devctl loopback — correctly
  single-host.
- Domain-flavoured example comments — kept (pedagogy, not coupling).

### Known-gaps to DOCUMENT (Step 4), not implement
- Host address authority = future agent resolve (automatic), not fleet.toml (the Finding-2
  clarification, stated positively so no one later adds a `host` field).
- `core/remote` `EdgeDialer` dials any numeric `host:port` (host-agnostic — good) but
  `SocketAddr::parse` only ⇒ no DNS.
- mTLS `localhost` fiction: `core/edge` `DevCA::leaf` SANs `localhost`/`127.0.0.1`/`::1` +
  `Client::dial` `ServerName="localhost"` — LAN hop passes only by mutual fake identity.
- agent↔master mTLS hop nonexistent; port-minting × master-down; cross-host artifact
  distribution — already named in `weles-design.md` (augment, don't duplicate).
- Process contract (review point 2): weles is generic over *domains*, not *process shape*
  (PORT/EDGE_ADDR/`/readyz`/`Edge|Http`/loopback).

---

## Ordered steps

### Step 1 — Unify the root authority; source root from runtime, fail-closed off-checkout `[opus / core-implementer, think hard]`
**(a) What:** `cli.rs` (add `--root <path>` on `up`/`deploy`/`status`/`down`, threaded into
each `Command` variant), new single `resolve_root(flag: Option<PathBuf>)` in `prep.rs`, rewrite
`main.rs:175 state_path()` + `supervisor.rs:623 workspace_root()` to call it (threading the
parsed flag through `run_up`/`discover_layout`/`connect_target`/`state_path`), delete both
runtime `CARGO_MANIFEST_DIR` uses. `main_tests.rs:11` updated.
**(b) Why now / order:** the one real current authority; touches the entry points; first so
later steps run under the unified root. Independent of Steps 2-4.
**(c) How (non-mechanical, addressing Findings 1 & 3):** resolution chain, single authority:
  1. `--root <path>` (threaded from the ONE `cli.rs` parse — NOT re-parsed inside
     `resolve_root`, which would be a second argv authority);
  2. else `WELES_ROOT` env;
  3. else **walk cwd up to the repo marker** (`Cargo.toml` + `tools/processctl`, matching
     `verifyctl/src/runner.rs:357-364`) — this keeps the dev `rollout.lock` path byte-identical
     to devctl/verifyctl so the one-Postgres mutual exclusion is preserved;
  4. else (no marker — a real off-checkout deploy) **`bail!`** telling the operator to pass
     `--root`/`WELES_ROOT`. Fail-closed, per repo convention — never a silent flat-cwd that
     mis-locates state/lock/deploy.
  Drop `CARGO_MANIFEST_DIR` from runtime entirely (the compile-time leak we remove); `current_exe`
  rejected (a deployed weles installs separately from the fleet's `deploy/`, so its location is
  unrelated to root). **Prove the failing branch:** (i) `resolve_root` from a NESTED cwd returns
  the marker root, not the nested dir (pins the lock-path-compat branch — the exact Finding-1
  bug); (ii) `--root`/`WELES_ROOT` override cwd; (iii) `state_path` and `workspace_root` return
  the SAME root from one source (pins de-duplication); (iv) no-marker-no-flag ⇒ error. Sweep:
  grep the crate for any other runtime `CARGO_MANIFEST_DIR` before finishing.
**Commit:** `refactor(weles): single runtime root authority (--root/WELES_ROOT/marker-walk, fail-closed), drop compile-time CARGO_MANIFEST_DIR`

### Step 2 — Placement annotation: the design-sanctioned manifest seam, fail-closed `[opus / core-implementer, think]` — CONFIRMED (typed seam)
**(a) What:** `fleet_toml.rs` `ServiceEntry` gains `placement: Option<String>` (`#[serde(default)]`;
`deny_unknown_fields` already present), threaded into `ServiceDef` via `to_service_def`;
`fleet_toml.rs` `validate` gains a placement check; `fleet_toml_tests.rs` positive + negative.
**(b) Why now / order:** plants the design-correct address-adjacent seam (`weles-design.md:245`)
so no one later adds a raw `host` field (the Finding-2 second-authority trap). Independent of
Steps 1/3/4; before the doc (Step 5) so the doc can point at the shipped seam.
**(c) How (leanest honest form — flagged as forward-looking, not behavior):** `service_addr` is
**untouched** (single node ⇒ loopback ⇒ M1 byte-identical). Placement's only live consumer today
is `validate`: legal single-machine values are **absent** or a reserved sentinel `"local"`; any
other value ⇒ `bail!` "multi-node placement not supported yet — omit or use 'local'" (fail-closed,
per repo convention; there is no node registry yet, so a real node name can't be honoured and
must not silently no-op). This is the anti-corruption marker: it exists in the type system,
rejects premature multi-node use, and pre-declares the seam host-derivation will hang off at
machine two — WITHOUT implementing multi-machine. **Prove the branch:** `placement="local"`
(and absent) ⇒ Ok, `service_addr` still `127.0.0.1:port`; `placement="node-b"` ⇒ validation error.
**User confirmed (typed seam):** plant the validated field now; the near-inert caveat is
accepted as the cost of the anti-corruption marker.
**Commit:** `feat(weles): placement annotation (fail-closed, single-node) — design-sanctioned manifest seam`

### Step 3 — Fail closed on a Told peer to a replicated provider `[opus / core-implementer, think]`
**(a) What:** `fleet_toml.rs` `validate_peers` (`:303-341`) gains a provider-multiplicity check;
`fleet_toml_tests.rs` gains positive + negative cases.
**(b) Why now / order:** closes the Told/Asks latent bug. Independent; own dispatch, own commit.
**(c) How (addressing Finding 6):** build a `provider → count` map over `fleet.services`
**keyed on `Some(name)` only** (skip `provider=None` — the monolith, `manifest.rs:216,339`, else
two monolith entries miscount as a replica). For each **Told** peer whose provider resolves to
`count > 1`, `bail!` naming consumer, `env_key`, provider, count. **Asks-only / unreferenced
replicas stay legal** (Told peers always name a concrete provider — `validate_peers` loops
`svc.addrs.told()`, `fleet_toml.rs:305` — so an Asks-only replica never enters the branch). Do
NOT add multi-address Told (one env var can't carry N peers — that's the Asks contract). **Prove
the failing branch:** two `provider="x"` + a Told peer to `x` ⇒ validation error; same two + only
an Asks consumer ⇒ Ok; two monolith (`None`) entries ⇒ no false positive.
**Commit:** `fix(weles): reject a Told peer referencing a replicated provider (first-match trap)`

### Step 4 — Rename `topology` → `fleet_label` `[sonnet]`
**(a) What:** `state.rs:104`, `supervisor.rs:532,562,702,711` (+ doc `:531`), `control.rs` ×5,
`main.rs:164`, 12 test-fixture literals across 6 files.
**(b) Why now / order:** cosmetic, fully independent — standalone so it never blocks the
authority work. Skip `cli_tests.rs:25` (`up_rejects_a_topology_token` = CLI arg grammar, NOT the
field — explicit non-target).
**(c) How (addressing Finding 5):** pure field rename; the local var `fleet_label` already exists
in `run_up`. **Operational note in the commit body:** the rename changes the persisted
`state.json` key, so a live fleet must be brought DOWN before swapping to the renamed binary (a
post-rename binary can't `status`/`down` a pre-rename fleet — deserialize fails); low risk under
one-rollout-at-a-time + wipe strategy, but stated, not silent.
**Commit:** `refactor(weles): rename vestigial topology field to fleet_label`

### Step 5 — Audit doc: single-host known-gaps via dated errata + honesty corrections `[inline]`
**(a) What:** `docs/reference/weles-design.md` — a new **"Single-host assumptions (current)"**
block, added via the doc's OWN convention.
**(b) Why now / order:** last, so it reflects the post-fix reality (root now runtime) and the
Finding-2 host-authority clarification. Judgment-heavy synthesis I hold in context ⇒ `[inline]`.
**(c) How (addressing Finding 4 — the doc's errata convention):** the doc **preserves stale body
prose and prepends dated errata** (`:13-52`, convention stated `:49-52` per "historical docs are
archives"). So:
  - Update the top **Status header** (`:8-11` "M1 not started") — meta, legitimately editable in
    place; M1 is partially shipped (agentapi hello/resolve `:655`, managed-gateway stage passes
    `:543`).
  - For the body overclaim at `:235-237` (present-tense master/agent split not yet built) and
    `:120`: **do NOT rewrite in place** — add a dated correction/errata block that reads against
    them, matching the file's established method.
  - New consolidated content the audit surfaced (no single place states it today): a
    **"what breaks if you copy the weles binary to a second machine today"** paragraph anchored to
    the trigger line `:251-255` (the planned real-hardware multi-machine proof); the
    **host-address-authority = agent resolve (automatic, not fleet.toml)** clarification (Finding
    2, stated positively so no one adds a `host` field later); the **process contract** (review
    point 2); and cross-references (not restatements) to existing coverage of mTLS/DNS/minting/
    master-down (`:346-349,419-461,612-620,641-645`). Only genuinely-missing item to add fresh:
    the root/privilege angle (`:532-536` never states it).
**Commit:** `docs(weles): single-host known-gaps errata + status-header correction + host-authority clarification`

---

## Verification (after all steps)
- `cargo test -p weles` (self-contained crate; no shared-Postgres rollout, safe alone — still
  `pgrep -x cargo`/`rustc` first).
- `cargo run -p verifyctl -- --fast` — confirm `weles-async-island` `AGENT_PORT` check and the
  `weles-managed-gateway` stage still pass (Step 1 must not change the lock path devctl/verifyctl
  compute; the marker-walk exists to guarantee that).
- Trailer audit: `git log -5 --format="%h %B" | grep "Co-Authored"` — Steps 1-2 → Opus 4.8,
  Step 3 → Sonnet 4.6, Step 4 inline.

## Out of scope (named, not dropped silently)
- Any multi-machine path (agent-owned resolve / real host:port / node registry / mTLS hardening /
  DNS in EdgeDialer / port-minting) — documented as known-gaps only. Placement (Step 2) is the
  *annotation* only, fail-closed on real multi-node use — it does NOT implement placement-driven
  host resolution.
- A raw `fleet.toml` `host` address field (Finding 2 — would be a second address authority; the
  loopback literal is correct until the agent owns resolve). Placement replaces this idea.
- `run/rollout.lock` semantics, weles's own agent bind, health probes, processctl/splitproof/
  devctl loopback, domain-flavoured example comments — all correct as-is.

## Trailer audit (renumbered): Steps 1-3 → Opus 4.8, Step 4 → Sonnet 4.6, Step 5 inline.
