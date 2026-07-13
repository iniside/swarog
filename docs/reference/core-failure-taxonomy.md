# Core Failure Taxonomy — evidence base for specialized implementer/reviewer agents

**Provenance.** Mined from the 2026-07-11 → 2026-07-13 remediation window (~217 commits,
of which ~110 are `fix(...)` commits). Six parallel research subagents each read the FULL
diffs of one core-layer slice (lifecycle/app · durable event plane · edge/remote/RPC ·
rollout tooling · verify net · domain modules) and returned records under one schema
(defect → failure class → what hostile review missed → the deciding authority → the
failing-branch proof → fix-on-fix chain). This document is the synthesis. It is the
source that the `core-implementer` / `core-reviewer` agent checklists derive from — update
it when a NEW failure class shows up in a real commit, not from guesswork.

Companion rules already in CLAUDE.md: **Fix the Authority, Not the Symptom**, **Adversarial
Subagent Review**. This doc is the *empirical catalog* those two rules operate on.

---

## The one finding that reframes the problem

**Fix-on-fix dominates the window.** Not a few cases — the majority:

- **processctl: 16 of 18 fixes** belong to multi-commit chains against ONE authority —
  the rollout lock/lease (8: `b879fc3 062a5e7 7611a65 aa304a3 db02527 18dcb65 26a7abc
  baa8bbd`), guardian/Windows-spawn (3), build-env allowlist (4).
- **RPC retry classification: 4 commits** (`4ac75cd → ace9e96 → f4b4060 → c829a21`) because
  the first fix classified on a *derived* value (`opsapi::Status`) instead of the raw
  `edge::Error`/Quinn error where the information still existed.
- **devctl teardown status: 6 commits** on one struct (`FleetStatus`/`ManagedStatus`), each
  adding a new input to "is this teardown actually a success".
- Retention (`7cf9957 → e546c08`), golden (`be3aae3 → 606d321`), archcheck rules
  (`23ec41a → 5e8ae52`, `971cf2c → 428182d`), docs scanner (`0d4ae71 → ed2742e`),
  pool budget (`d96e4bf → 5a0ab31`), inventory quantity (`5bfe36b → 2c01e3c`) — all
  round-2 patches of a round-1 fix's OWN new seam.

**What this means.** The hostile-review discipline was *working* — nearly every round-2 fix
exists because an adversarial pass caught a hole. The disease is upstream of review: fixes
were authored **symptom-first**, so each one created a fresh boundary (a new constant, a new
error branch folded into success, a resource owned by the wrong scope, a lock acquired in the
wrong place) that the next review then broke. **More review rounds ≠ fewer bugs; they were a
symptom of the same authorless fixes.** The lever is to make the implementer *locate the
deciding authority before writing*, and to make the reviewer *attack the fix's own new seam
by class*, keyed to what the change touched — not generic hostility.

This is exactly why generic agents kept missing it: the failure classes below are subtle,
repo-specific, and invisible to "does it look plausible / does it compile / does the test
pass" review. A test passing is the #1 disguise in this catalog.

---

## Failure-class catalog (ranked by cross-slice recurrence)

Each class: **definition** · **where it recurred** · **the attack** (what a reviewer must
try) · **the authority move** (where the fix belongs).

### 1. `error-folded-into-success` — *the most recurrent class; every slice had it*
A real failure (an error, a serialize failure, a cleanup failure, a name collision, a
truncated parse) is absorbed into an apparently-successful result: `Ok(())`, `Value::Null`,
a green diff, `FleetStatus::Stopped`, a logged-but-not-propagated counter.
- **Recurred:** retention sweep returning `Ok` while a topic persistently failed
  (`71e3a6a`, sibling of the landmark `7ca0b51`); rpc-macro `to_value(..).unwrap_or(Null)`
  (`56ce6fb`); csharp-gen last-writer-wins DTO collision (`982ec8d`); guardian cleanup
  errors discarded (`192ffda`); devctl teardown reporting Stopped despite cleanup failures
  (`18c9076`, whole 6-commit chain); splitproof `.ok().flatten()` swallowing SQL error as
  zero-rows (`20fcf9c`); conformance `KnownGap` conflated with `Fail` — the *inverse*
  polarity, a tracked non-blocker diluting the stop-the-line signal (`0d5ff32`);
  invalidation/asyncevents treating explicit `0` as absent (`e5c1b7f`).
- **Attack:** trace every error/`.ok()`/`.unwrap_or`/`let _ =`/`take_while` to its TERMINAL
  caller and confirm the caller's control flow (readyz stamp, exit code, golden diff, the
  status the operator trusts) actually *observes* the failure. "It's logged / there's a
  metric" is NOT evidence the failure is surfaced.
- **Authority:** the return-value contract of the function the caller trusts (`sweep()` now
  `bail!`s; `teardown_with` decides status from a `cleanup_failures` vec; the codegen
  template emits `Internal` on serialize failure).

### 2. `unbounded-operation` — *dominant in transport + domain; ties #1 overall*
A peer-controlled or resource-consuming operation has no bound of its own — missing timeout,
missing capacity cap, missing grace, or a bound that covers the wrong phase.
- **Recurred:** `axum::serve` detached per-connection tasks surviving teardown (`5943eed`);
  unbounded IP-limiter visitor table (`bdbd325`); plane DDL advisory lock with no
  `lock_timeout` (`23a8aee`); player-QUIC stream read/write unbounded — the twin the internal
  plane already fixed (`ac87caa`); `Stub::start` boot hook awaited unbounded, stalling ALL
  later module starts (`6d505e2`); devctl control protocol with no frame/time bound
  (`049a6f5`); **gateway credential admission bounded only at dial, not the RPC round-trip**
  (`cec0d73`); gateway `RouteTable` Mutex held across `dial` serializing all providers
  (`5be0d0f`); scheduler wedge on a pooled connection (`3f35d4a/addc824`); inventory quantity
  overflow inside the durable delivery tx (`5bfe36b`); unbounded session-token / match
  inputs (`384f5f0/a035fd2`, `aefcb1f`).
- **Attack:** for every new I/O call or lock acquisition ask "**what specifically bounds
  this, and does the bound cover the actual hang point?**" A *dial* timeout is not an *RPC*
  timeout. A *connection* idle timeout does not bound a *stream*. A cache mutex must never
  span an `.await`. A future holding a session-scoped DB lock must never be wrapped in a
  cancelling timeout (bound at the DB layer or use a dedicated connection whose drop closes
  the session).
- **Authority:** one seam wrapping the WHOLE operation (`FrontDoor::admit()`,
  `PLAYER_STREAM_GRACE`, `Stub::start_with_boot_timeout`) — plus the sibling/twin plane.

### 3. `ordering-not-structural` — *decision or sequence that isn't enforced by construction*
Something that must happen in a fixed order, at a fixed time, or as one unit is left to
"happens to run in the right order" — recomputed per-request when it should freeze at boot,
split across two transactions when it must be one, or reshaped to satisfy the wrong caller.
- **Recurred:** the **Send-bound rewrite** (`d2b202a → 32c7c01`) — production phase
  sequencing/teardown was rewritten into detached spawned tasks purely to satisfy a
  `tokio::spawn(run(...))` in 4 tests; reverted wholesale once review located that the Send
  requirement belonged to the *test caller*, not production. `/readyz` membership resolved
  per-request instead of snapshotted at boot (`c6466eb`). accounts register + session
  issuance in two transactions (`cfff987`). bless actions dispatching before the rollout
  lease was acquired (`f257b67`). lock/lease handle closed before/after marker cleanup kept
  flipping (the 8-commit chain).
- **Attack:** for any "the compiler/test needs this" justification, ask **which caller
  actually has the requirement** — production `.await` and a test's `tokio::spawn` are
  different callers; the fix belongs at the one with the need, never at the shared production
  authority. For any two-step operation ask "what is observable *between* step 1's commit and
  step 2". For any lazily-recomputed value ask "what if the inputs drift after boot".
- **Authority:** freeze the decision at one structural point (`snapshot_readiness_checks`,
  one threaded `PgConnection`/tx, lease-before-dispatch).

### 4. `resource-owned-by-wrong-scope` — *a handle/lease/connection/permit owned by the wrong thing*
A resource is owned implicitly by a library type or an async frame when the code actually
needs typed, cancellation-correct ownership — so it leaks, is returned to a pool poisoned, or
is released on cancel while the work keeps running.
- **Recurred:** pooled connection returned in ABORTED-TRANSACTION state after failed DDL,
  then the recovery ROLLBACK's own failure discarded (`23a8aee → 4b7d41c`); Windows process
  handle owned opaquely by `std::process::Child` when the tool needed it for job/console-ctrl
  (`02ff417`); ambient env read via ad-hoc `std::env::vars()` at multiple sites instead of one
  captured snapshot (`8e96c15`); argon2 permit owned by the async handler frame instead of the
  blocking closure (accounts/admin — see CLAUDE.md admin section); scheduler aborted task's
  JoinHandle never awaited (`0431926`); the whole lock/lease chain.
- **Attack:** trace **who actually calls `Drop`/close/release** on this resource, and whether
  two code paths (owner vs borrower, async frame vs blocking closure, two `env::vars()` calls)
  can observe it in different states. **Attack the fix's OWN recovery action** — "what if the
  rollback / the cleanup / the abort itself fails".
- **Authority:** one owning type (`EnvironmentSnapshot`, `PlatformChild`, a unified rollout-
  lock owner); on recovery failure, `detach()` + drop rather than return-to-pool.

### 5. `constant-shadows-config-knob` / `hardcoded-threshold` / `duplicated-authority`
A threshold/limit/derivation exists in more than one place, or duplicates a value already
computed elsewhere, or is a bare literal with an explanatory comment that reads as intentional.
- **Recurred:** `RETENTION_STALL_MAX` hardcoded 3h shadowing the configurable housekeep
  interval (`47a986c`, the landmark class); rate-pair `*_BURST` parsed without the policy its
  `*_RPS` sibling got (`14cd49e → e5c1b7f`); overflow guard `> u64::MAX` that excludes nothing
  (`e546c08`); scheduler interval ceiling triplicated across DDL + two SQL strings, only 2 of 3
  pinned (`ae749d6 → fb23e4e`); apikeys key-length cap split across two fortress modules
  (`710c0a3`); pool budget `HARNESS_RESERVE=10`/`97` bare literals + spot-checked services
  (`d96e4bf → 5a0ab31`); credential-admission `0` with ambiguous disable-vs-instant semantics
  (`6fe744a`).
- **Attack:** for every literal threshold, **grep for a second source of the same value** and
  hand-evaluate the comparison operator at its exact boundary (`>` vs `>=` vs equal-to-MAX). A
  constant with a good doc comment is the disguise, not the exemption. For a cross-module cap,
  read the write-path limit and the read-path limit side by side.
- **Authority:** one derivation (`plane.retention_stall_after()`, `env_rate_pair`, a shared
  `apikeysapi::MAX_KEY_BYTES` contract const + a DDL CHECK so raw psql is covered too).

### 6. `coverage-gap` / `false-pass` — *the verify-net signature: a gate that can't see the thing it gates*
A check is architecturally present and green, but its authority (a match guard, a constant, a
stance, an argv, a lock branch) has silently detached from what it's supposed to gate — so it
matches zero targets, tests a hand-built fixture that bypasses the real classifier, or asserts
"a response came back" instead of "the seam works".
- **Recurred:** archcheck rule 9 `Kind::Other` vs real `Kind::Core("bus")` — dead, always
  green (`23ec41a → 5e8ae52`); `CONFORMANCE_POLICY_CRATE="conformance"` vs real
  `conformancecheck` — permanently vacuous (`3cbc2f0`); bare `Slot::new` via `use` bypassing
  the qualified-string tripwire (`971cf2c → 428182d`); audit argv missing the `audit`
  subcommand, fixture never asserted argv (`8e297f5`); splitproof asserting 201 but not that
  the created row is reachable through the remote list binding (`d0512a6`); golden fingerprint
  blind to `Option<T>→T` demotion and testing the Rust struct instead of the production SQL
  trigger (`be3aae3 → 606d321`).
- **Attack:** for any string/enum tripwire, write a test that calls the **actual production
  function** against a fixture proven to trigger the real violation — never assert on a
  hand-built array that skips the classifier. Ask "**if I renamed/aliased/re-imported the
  target, would this rule still fire?**" A rule that can silently match zero targets must
  itself be a violation. For a scenario assertion ask "does this prove the seam it claims, or
  just that *a* response returned".
- **Authority:** the match guard / constant / argv / stance itself, compile- or test-coupled
  to the real source of truth (a rule that matches zero packages fails loudly; a golden gen
  driven from the same artifact production uses).

### 7. `notapplicable-hides-gap` — *a "this doesn't apply" rationale that's actually false*
An empty/opaque/unrestricted classification, backed by plausible prose, that on inspection
covers a real, reachable gap.
- **Recurred:** conformance `characters.name/class`, `accounts.loginEpic id_token`,
  `accounts.verifySession token`, `match.report` fields all marked NA/Opaque/Unrestricted
  while genuinely wire-reachable and uncapped — **3 modules independently** (`577d4f5`,
  `2915efd`, `0d5ff32` → swept by `a9211c6`); contrib empty-read returning `Vec::new` without
  reserving the canonical type (`d0bcd93`); topiccheck dedup keyed on topic not `(topic,
  version)`, rejecting the mandated additive-version evolution (`89353e6`).
- **Attack:** for every `NotApplicable`/`Opaque`/`Unrestricted`/"currently accepts no
  free-text" stance, verify the field's actual wire-reachability and byte-cap against the
  handler code — never accept the prose rationale. Test the READ-before-any-WRITE and the
  FORGED/DUPLICATE-first orderings, not just the sequence the feature was designed around.
- **Authority:** replace the stance with an executable probe (`CapCase`,
  `conformance_*_rejected`); reserve the invariant on first *read*, not only first write.

### 8. `hand-maintained-list-drift` — *an allowlist/enumeration with no self-check against the source of truth*
- **Recurred:** Windows `BUILD_ENV_ALLOWLIST` missing a var per incident, never enumerated —
  and `b50c553` bolting on a SECOND uncoordinated authority (a directory scanner) instead of
  fixing the list's self-check (`1190001`, `54dea2e`, `b50c553`); pool-budget test
  spot-checking 2-3 services not all (`5a0ab31`); binary-resolution duplicated in `csharp.rs`
  vs the shared `WorkspaceLayout` (`c2546dc`); package-name constant vs real crate name
  (`3cbc2f0`).
- **Attack:** does this list have a test that catches the **next** missing entry, or only one
  pinning the entry just added? (Repo memory: *"didn't-forget" tooling must self-check* —
  diff against the real source of truth and die pre-work.) A second hand-rolled authority for
  the same problem is the smell that the first authority is wrong — replace, don't add.
- **Authority:** derive from the real source (loop over `fleet.services()`; one
  `WorkspaceLayout`; a completeness scan that fails on an unenumerated case).

### 9. `topology-blind-violation` / `monolith-only-not-proven-on-split` — *rare, highest severity*
A gate/conditional that is correct in the monolith but bypassed in the split, because the
`cmd/*-svc` edge registration and `gateway-svc` route table don't share the monolith's `if`.
- **Recurred:** **`INVENTORY_DEV_GRANT` gated only the monolith op-contribution** — the
  inventory-svc internal edge and gateway-svc routing served the dev-only IAP grant
  UNCONDITIONALLY in split; a dev endpoint live in "production" topology (`25f2163`). Gateway
  `RouteTable` lock-across-await is dormant in monolith (no remote dials) and manifests ONLY
  under split (`5be0d0f`).
- **Attack:** for any env-gated capability, trace the gate through **every** exposure path —
  HTTP op contribution, player-QUIC allow-list, internal mTLS edge registration — not just the
  monolith branch. **These bugs are invisible to a review that reads only `modules/<name>` or
  runs only the monolith.** Prove on the split (routecheck / splitproof), not a monolith unit
  test.
- **Authority:** move the gate INTO the capability method every exposure path traverses
  (`Holdings::grant` itself), and contribute ops unconditionally so monolith and split route
  sets are structurally equal by construction.

### 10. Long tail (one or two instances each — still real, still catalog them)
- `retry-semantics-wrong` — classifying reset/replay on a post-mapping value; classify on the
  raw error before erasure (`RPC-RETRY-01`).
- `budget-starvation` / fairness-not-preserved — a correctly-bounded budget whose allocation
  order starves the same tail every pass; replay ≥2 passes and check the starved SET changes
  (`addc824 → 3615b9e`).
- `unsupervised-task-death` — a bare `tokio::spawn` with nothing observing its panic/exit;
  name the mechanism that notices death and where it surfaces (`a9300c`/`a9c7e32`).
- `torn-write-published-early` — a secret/file published non-atomically; temp-then-rename with
  a validated temp (`a83dc79`).
- `sentinel-collision` — two special integer values sharing a space near a saturation boundary
  (`7cf9957`).
- `lost-update-no-concurrency-token` — render-then-submit forms with no optimistic-concurrency
  check; **3 admin modules** (`99025bd`, `4c6bd44`, `79cabed`).
- `missing-ttl-security-cache` — a credential cache that refreshes on miss but never expires a
  rotated-out key (`945e9d0`).
- `security-boundary-missing-binding` — OAuth `state` not bound to the initiating browser
  (`6cc0e7c`); TOCTOU on identity link (`b8c7812`).
- `policy-authority-drift` — the sole `ALTER` in `modules/`, violating idempotent-DDL policy
  (`1970677`).
- `silent-truncation-parse` — `take_while(is_ascii_digit)` silently recording a wrong version
  (`75d4042`).
- `generated-surface-escapes-governance` — codegen emitting a new `pub` item without re-blessing
  public-api; treat any new `pub` from codegen as public-API-affecting by construction
  (`627c33e`).

---

## Cross-cutting review checklist — keyed to what the change touched

The point of specialized review is NOT generic hostility — it's loading the right 3-5 attacks
for the files in the diff. Route by what changed:

- **Touches `core/app::run` / lifecycle sequencing / teardown/drain** → classes 3, 2, 4. Ask:
  which caller needs this constraint (prod vs test)? does drain cancel work spawned *inside*
  the dropped future? is membership frozen at boot or recomputed per request? does every stop
  path have its bound?
- **Touches `core/asyncevents` / retention / delivery** → classes 1, 5, 2. Ask: does the
  caller see this error (readyz stamp)? is this threshold a duplicate of a configured value?
  is the lock/handler bounded? does the poison/failing branch have a live-PG test?
- **Touches `core/edge` / `core/remote` / RPC glue** → classes 2, 1 (retry), 4. Ask: dial
  bound vs RPC bound? is the retry classifying on the raw error or a collapsed status? is the
  twin plane (public vs internal) swept? does serialize failure become `Internal` or `null`?
- **Touches `tools/` rollout (processctl/devctl/splitproof)** → classes 4, 1, 8, 6. Ask: is
  this the 2nd+ commit on this authority this week (STOP, redesign)? who owns the
  handle/lease/env? does teardown status see cleanup failures? does the list self-check? does
  the harness prove the seam or just a 200?
- **Touches `tools/` verify (verifyctl/archcheck/conformance/topiccheck/golden)** → classes 6,
  7, 8. Ask: does the test call the real gate fn or a bypass fixture? would a rename make this
  rule vacuous? does a NotApplicable/Opaque stance hold against the handler? is the gate's own
  tool-acquisition failure a FAIL not a green SKIP?
- **Touches `modules/*` (domain)** → classes 2, 9, 5, and the long tail. Ask: is an env gate
  traced through edge + gateway routing, not just the monolith `if`? is this proven on split?
  is a cap duplicated across fortresses? render-then-submit without a concurrency token?

---

## Implications for the specialized agents (the bridge)

The evidence says the two agents must encode, not just "be hostile":

1. **`core-implementer` — authority-first.** Before writing a fix it must name, in one
   sentence, the single deciding place the change belongs (class → authority column above),
   and state the minimal-sufficient closure. It refuses to add a second special case beside an
   earlier fix (the `build_env` / `b50c553` anti-pattern). It ships a test that runs the
   *previously-wrong branch* on the *at-risk topology* (split for class 9). This directly
   attacks the fix-on-fix disease at its root.
2. **`core-reviewer` — class-keyed, attack the fix's own new seam.** Not generic. It routes by
   the files in the diff to the 3-5 classes above and runs those specific attacks, opening the
   files (never trusting a summary), confirming the negative-path test exercises the failing
   branch, and stating the fix's failure mode out loud. A zero-finding review of a non-trivial
   core diff is a signal to re-review.

Both are vehicles for THIS catalog. Keep the catalog current; the agents are only as good as
the classes they carry.
