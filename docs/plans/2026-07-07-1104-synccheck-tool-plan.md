# Plan: `synccheck` — static regression-guard for async-bus-used-as-disguised-sync-RPC

**Date:** 2026-07-07 11:04 (rev. 2 — post grumpy-review)
**Branch:** feat/synccheck (off master — see Step 0)
**Status:** DRAFT rev.2 — punch list addressed; pending user approval + dispatch tags.

## What this tool honestly is (read first)

A new `tools/synccheck` analyzer, sibling of `tools/topiccheck`, that flags one
seam-misuse the enforcement net cannot catch: an async `bus.Emit` whose side effect is
then **synchronously awaited by polling** (fire the event, then busy-wait in a
`for{ sleep; read }` loop for its effect). CLAUDE.md hard-constraint #7 forbids exactly
this ("Publish never blocks and returns nothing… that's a service interface's job").

**It is a REGRESSION GUARD, not a debt-catcher — and the grumpy review forced this
honesty.** Research found **zero** production sites with the smell. The only real
"emit→poll" instances live in test files (`inventory_test.go:186-203`,
`config_test.go:168-177`) and are *legitimate* test-synchronization idioms (assert the
async path really is async), not debt. Moreover `packages.Load` **without `Tests:true`
does not load test files at all**, so those idioms are naturally out of scope and the
tool sees them never. Net: the tool guards against the DB/store-poll shape being
*introduced* into production later — the same value proposition as topiccheck guarding
future dead topics. It does **not** find anything on today's tree, and the plan no longer
claims it does.

Given that, this rev. **cuts** the two speculative layers the first draft carried (SSA
channel-join and the marker inversion) — see "Cut / deferred" with rationale. The
shippable tool is Layer 1 alone, advisory.

## Context — overlapping existing systems (Research-before-planning)

| System | What it does | Why not extend it |
|---|---|---|
| **`tools/topiccheck`** | Whole-module `packages.Load` analyzer; flags `bus.Define` topics with no `bus.On`. | Clone the plumbing, don't extend the finding domain (orthogonal). Lift verbatim: `isBusFunc`, `objectOf`, `stringConst`, `allowlisted`, `loadMode`, the `analyze(pkgs) []Finding` + `testdata/` harness. `tools/` holds only topiccheck today; synccheck is the natural sibling. |
| **`apidiff` stage** | Additive-only guard on `*events`/`adminapi`. | Different axis (payload shape vs call-site behavior). Shares only the advisory-stage wiring we mirror. |
| **`golangci-lint`** | Correctness/leak/security linters. | No off-the-shelf linter models "emit-then-poll" — it's tied to *our* `gamebackend/bus` API. Its `//nolint:… // reason` convention informs our directive style. |
| **`go-arch-lint`** | Import-boundary enforcement. | Cannot see intra-function control flow. The gap this fills. |
| **`gamebackend/registry` (Provide/Require/TryRequire)** | The legitimate sync seam. | Not a tool — the thing the analyzer must NOT flag. |

**Naming (research-corrected):** real packages are `gamebackend/bus` (`Emit[T]`, `On[T]`,
`Define[T]`, `(*Bus).Publish`, `(*Bus).Subscribe`) and `gamebackend/registry`.
`Contribute`/`Contributions` are `lifecycle.Context` methods — a third primitive, not to
be misclassified. CLAUDE.md's `core.Emit` is doc shorthand; no `core` package exists.

**Calibration (must NOT flag):** the pre-emit `rs.MMR()` interface call in
`modules/match/match.go:34` (sync registry call *before* the emit); the
`modules/config/listen.go:89` LISTEN/NOTIFY publisher loop (emits then loops back to
read — the canonical *correct* async publisher; spared only by the emit-in-loop guard,
Step 2); the `outbox/relay.go` scheduled drainer.

**Verified available (no go.mod change):** `x/tools v0.45.0` direct dep; cache has
`go/cfg`, `go/ssa`, `go/pointer`, `go/analysis`.

---

## Step 0 — Branch [inline]

**What:** `git switch -c feat/synccheck master` (confirm base with user; current
`feat/config-module` carries unrelated inventory WIP).
**Why now:** isolate the new tool from in-flight changes before adding files.

## Step 1 — Harness clone + config struct `tools/synccheck/main.go` [sonnet]

**What (exact files):** `tools/synccheck/main.go` (`package main`);
`tools/synccheck/analyzer_test.go` (whitebox `package main`); `tools/synccheck/testdata/`.

**Why now / order:** every detector hangs off the shared "is this THE bus Emit, which
topic" resolution and the whole-module load. Harness must compile and load first.

**How (non-mechanical moves):**
1. Copy `loadMode` verbatim: `NeedName|NeedTypes|NeedTypesInfo|NeedSyntax|NeedDeps|
   NeedFiles`. **Explicitly leave `Config.Tests` unset** — test files stay out of scope by
   design (see "What this tool is"). Document this as a deliberate choice in a doc comment,
   NOT an accidental omission, so a future reader doesn't "fix" it without also adding
   test-variant dedup (`Tests:true` yields up to 4 pkg variants per dir → double findings).
   **No `--include-tests` flag** (rev.1 spec'd it; it was unimplementable without the
   dedup and is dropped).
2. Lift **verbatim** from `tools/topiccheck/main.go`: `isBusFunc(pkg, call, name)`
   (unwraps `*ast.IndexExpr`/`*ast.IndexListExpr`, resolves `TypesInfo.Uses[id].(*types.
   Func)`, gates on `fn.Pkg().Path()=="gamebackend/bus" && fn.Name()==name`; review
   confirmed it handles both generic `Emit` and method `(*Bus).Publish`), `objectOf`,
   `stringConst`.
3. Generalize `allowlisted` to anchor on an arbitrary `token.Pos` (a statement/func, not
   only a `var` decl): keep the three-source comment gathering (`gd.Doc`, `vs.Doc`,
   line-adjacency fallback `Fset.Position(cg.End()).Line == line-1`); directive regexps
   `//\s*synccheck:allow` + `reason="([^"]*)"`.
4. **Config struct from the start** (addresses reviewer m7): `type config struct{ strict
   bool }` threaded into `analyze(pkgs, cfg) []Finding`. `main()` parses `--strict`
   (default false); patterns default `./...`; `packages.Load` error → `os.Exit(2)`; print
   findings always; exit 1 only if `strict && len(findings)>0`. Byte-for-byte the
   topiccheck exit contract.
5. `Finding{ Pos token.Position; Kind string; Msg string }` — `Kind="emit-then-poll"`
   (only one kind now that SSA/marker are cut; keep the field for forward extensibility).

**Acceptance:** `go run ./tools/synccheck ./...` builds, loads the module, prints the
clean message; empty-fixture test passes.

## Step 2 — Layer 1 detector + fixtures: emit-then-poll (CFG) [opus]

**What:** the detector pass in `analyze`, using `golang.org/x/tools/go/cfg`, **plus** its
regression fixtures under `testdata/` (merged from rev.1's Step 3 — reviewer m8: the
acceptance references fixtures, so they must be born in the same step).

**Why now / order:** the sole reason the tool exists; depends only on Step 1.

**How (CFG usage corrected per reviewer M5, blind spots owned per M4):**
1. **Per-body CFGs, including nested func literals (M4).** `cfg.New` is per-body, and
   `go/cfg` treats `*ast.GoStmt` as a no-op that does **not** descend into the goroutine
   body. So: walk every `*ast.FuncDecl` **and every `*ast.FuncLit`** in a file, building a
   separate CFG per body via `cfg.New(body, mayReturn)`. `mayReturn` returns `false` for
   `panic`/`os.Exit`/`log.Fatal`-family (resolve via `TypesInfo.Uses`→`*types.Func`),
   `true` otherwise.
   **Documented blind spot (ship like topiccheck's string-topic note):** an Emit and its
   poll split across a func/goroutine boundary (`Emit` in outer body, `for{sleep;read}` in
   a spawned `go func(){}`) land in disjoint CFGs and are **not** joined. v1 analyzes each
   body independently; cross-body join is out of scope and stated in the tool's doc
   comment + CLAUDE.md line. (Rev.1 implied full reachability sophistication; this rev.
   admits the limit.)
2. **Locate Emit nodes:** scan each `cfg.Block.Nodes` for the `*ast.CallExpr` where
   `isBusFunc(pkg, call, "Emit")` or `"Publish"`. Record `(blockIndex, nodeIndex)`.
3. **Forward-reachable set:** BFS over `block.Succs` restricted to `block.Live`, from the
   Emit's block; include the Emit block's tail (nodes after `nodeIndex`).
4. **Find the suspect loop — via CFG block kinds, NOT by scanning Nodes for `ForStmt`
   (reviewer M5).** `go/cfg` does not store control statements in `Block.Nodes`; loop
   bodies are blocks with `Block.Kind == cfg.KindForBody` / `KindRangeBody` and the loop
   available via `Block.Stmt`. Identify loop-body blocks in the forward-reachable set;
   for each, test its member nodes for BOTH predicates:
   - **sleep/backoff:** `*ast.CallExpr` resolving to `time.Sleep`, `time.After`,
     `(*time.Ticker)`/`(*time.Timer).C` receive (resolve via `TypesInfo`; never bare
     `.Sleep`).
   - **read (scoped honestly — reviewer M3):** `*ast.CallExpr` resolving to a **DB/store
     read** — `database/sql` `(*DB/Tx).Query/QueryRow/QueryContext/Exec`, pgx equivalents,
     and configurable store/repo receiver-package prefixes. **Explicitly NOT** broadened
     to "any method call" (would fire on nearly every loop). **Known gap, stated in
     docs:** in-memory projection getters (e.g. the test idiom's `m.starterSpec()`) are
     NOT DB reads and won't fire — acceptable because (a) those live in out-of-scope test
     files and (b) broadening to catch them destroys precision. The tool targets the
     *DB-poll* shape specifically.
5. **Flag** only when one reachable loop body has BOTH predicates. **Emit-in-loop guard
   (reviewer confirmed load-bearing):** do NOT flag when the Emit itself is inside the
   candidate loop body — that is a periodic publisher (`config/listen.go`), not a poll.
   This guard is what spares the repo's flagship async files.
6. **Suppression + fixtures:** before appending a `Finding`, run generalized `allowlisted`
   on the enclosing func/loop comment groups. Fixtures under `testdata/`: `poll/`
   (emit-then-`for{sleep;query}` → fires), `clean/` (emit-then-response, like `match.go` →
   silent), `publisher/` (emit-in-loop → silent, guards the `listen.go` shape), `allowed/`
   (`poll/` + `//synccheck:allow reason="fixture"` → silent). `analyzer_test.go` asserts
   exactly `poll/` fires.

**Acceptance:** `go test ./tools/synccheck/` green with the four fixtures behaving as
above; `go run ./tools/synccheck ./...` on the real tree prints clean (zero findings —
the honest expected result).

## Step 3 — Wire ADVISORY stage + docs [sonnet]

**What:** `verify.ps1`, `verify.sh`, `CLAUDE.md`.
**Why now / order:** last — wiring a nonexistent tool would break verify.

**How:**
1. `verify.ps1`: `Invoke-SynccheckStage` cloned from `Invoke-TopiccheckStage` (~L193-206):
   `go run ./tools/synccheck ./...` (+`--strict` when `$StrictOn`),
   `Add-Result 'synccheck' … $false` (**advisory** — reviewer agreed advisory is correct
   for a heuristic detector with zero production targets). Call inside `if ($RunAdvisory)`
   after `Invoke-TopiccheckStage` (~L241).
2. `verify.sh`: mirror `synccheck_stage` after `topiccheck_stage` (~L238), inside
   `if [ "$RUN_ADVISORY" -eq 1 ]`.
3. Both script header comments: append `synccheck` to the advisory list.
4. `CLAUDE.md` (~L119-131): append synccheck to the ADVISORY bullet + per-stage list.
   **Documentation must state the tool's honest scope**: regression guard for the DB-poll
   shape; does not catch in-memory-getter polls; does not join across goroutine/func
   boundaries; test files out of scope. `//synccheck:allow reason="…"` directive.

**Acceptance:** `./verify.ps1 --all` shows `synccheck | PASS`; `--strict` on a seeded
fixture flips exit.

## Step 4 — Verify end-to-end [inline]

**What/How:** `go run ./tools/synccheck ./...` (expect clean), `go test
./tools/synccheck/`, `./verify.ps1 --all`; then a temporary emit-then-`for{sleep;query}`
edit in a real module to confirm it fires, then revert. Per the `verify` skill: drive the
behavior, not just tests.

---

## Cut / deferred (was rev.1 Steps 4–5) — with rationale

**CUT: marker inversion `//sync:via-service` (rev.1 Step 4).** The grumpy review showed
the rule "flag any read forward-reachable from an Emit unless marked" models the **wrong
invariant**. Emitting and then reading eventually-consistent state is normal and
everywhere; the actual invariant is "await the event's *own* effect." Applied literally,
Step 4 flags `config/listen.go:89` — the codebase's canonical *correct* async publisher —
and the marker name is semantically wrong there (it's a publisher, not a service
consumer). Users would reflexively sprinkle markers on correct code → the directive
becomes noise. Its trust-but-verify check (look for a `registry.Require`) is also naive:
`match.go:27` registers an anonymous `HandleFunc` closure with no receiver type to hang
the check on, and a module can legitimately `Require` a service *and* misuse the bus.
**Reopen only with a redesign that keys on "await own effect," not "read after emit."**

**CUT: Layer 2 SSA emit-then-block-on-channel (rev.1 Step 5).** Zero handlers in the repo
capture a channel (all handler state is struct-pointer fields), so it fires on nothing
today and no likely shape. It carries no regression fixture from real code (only a
synthetic one → silent bit-rot). Its "same struct field of same named type" join can't
distinguish two instances — safe here only because modules are singletons, i.e.
unfalsifiable rather than correct. **Reopen when a channel-capturing handler actually
exists**; at that point the SSA design (`ssautil.AllPackages`+`InstantiateGenerics`,
`ssa.Send`/`UnOp ARROW`/`MakeClosure.Bindings` join by `EventType` object) from research
angle 4 is the starting point — and must handle bound method values, not only closures.

## Reviewer punch-list dispositions

| # | Sev | Disposition |
|---|---|---|
| B1 test-loading premise false | BLOCKER | **Fixed** — Step 1.1: `Tests` deliberately unset, test files out of scope by design, `--include-tests` dropped; honesty reframed at top (regression guard, zero current targets). |
| B2 marker FPs on `listen.go` | BLOCKER | **Fixed** — marker inversion CUT (wrong invariant). |
| M3 read predicate misses idiom / over-broad | MAJOR | **Fixed** — Step 2.4 scopes read to DB/store, states the in-memory-getter gap honestly; scope-overclaim removed. |
| M4 goroutine/func split missed | MAJOR | **Fixed** — Step 2.1 analyzes each FuncLit body separately, documents cross-body join as an owned blind spot. |
| M5 `ForStmt` in `Block.Nodes` wrong | MAJOR | **Fixed** — Step 2.4 uses `Block.Kind==KindForBody/RangeBody` + `Block.Stmt`. |
| M6 cut Step 5, don't flag | MAJOR | **Fixed** — Layer 2 CUT with reopen criterion. |
| m7 flag/config inconsistency | MINOR | **Fixed** — config struct in Step 1.4; only `--strict`. |
| m8 fixtures vs step ordering | MINOR | **Fixed** — fixtures merged into Step 2. |
| m9 trust-but-verify naive | MINOR | **Moot** — belonged to cut Step 4; noted in Cut rationale. |
| m10 `mayReturn`/`t.Fatal` | MINOR | **Moot** — test files out of scope; noted. |

## Dispatch summary (for approval)

| Step | Lane | Rationale |
|---|---|---|
| 0 Branch | `[inline]` | trivial git |
| 1 Harness + config | `[sonnet]` | mechanical clone of a fully-specified pattern |
| 2 Layer 1 CFG + fixtures | `[opus]` | correctness-critical CFG reachability + type resolution |
| 3 Verify wiring + docs | `[sonnet]` | mechanical mirror of topiccheck stage |
| 4 Verify | `[inline]` | drive + observe |

Session is Opus → `[opus]` is top-tier (separate context = independent-reviewer boundary);
`[fable]` unused (not the session model). Each subagent gets its lane's `Co-Authored-By`
trailer + nav guidance pasted in.

## Open questions for the user

1. **Is a zero-target regression guard worth building now?** Honest framing post-review:
   this finds nothing on today's tree; its value is preventing a future DB-poll-on-the-bus
   regression. Reasonable to build (cheap, ~Layer-1-only) OR to shelve this plan until a
   near-miss actually appears. Recommendation: **build Layer 1** — it's small, mirrors
   topiccheck, and the guard is cheap insurance on a core architectural invariant.
2. **Base branch** for Step 0 (`master` vs stacked on `feat/config-module`).
