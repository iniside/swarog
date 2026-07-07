# Plan — Hoist the public contract surface to a top-level `api/` tree

**Date:** 2026-07-07 23:24
**Status:** DRAFT (pre-review)
**User decision (locked):** the public/consumed-by-other-modules surface must live in a
separate top-level `api/` folder next to `modules/` (`api/<name>/…`), so importing another
module's contract no longer reads as reaching into its private impl folder. Motivated by
memory [[separate-public-surface-from-impl]].

---

## Context

### The smell (user caught it on a glance)

Every module accreted its public surface INSIDE its own folder:
`modules/characters/{charactersapi, charactersevents, charactersrpc, charactersplayerrpc,
charactersadminrpc}`. A consumer that needs characters' events or generated client imports
`modules/characters/charactersevents` / `…/charactersrpc` — textually reaching into the
characters module. The `<module>events` co-location predates this work (rule 5), but the
unified-operation-transport program multiplied it to 3–4 sub-packages per module without
questioning the layout. This plan separates **public contract** (`api/<name>/`) from
**private impl** (`modules/<name>/`).

### What is actually cross-module-consumed (research, 2026-07-07, 6 subagents)

- **`*events` (5):** 4 of 5 consumed cross-module — `charactersevents`→inventory,
  `configevents`→inventory, `matchevents`→rating+leaderboard, `schedulerevents`→audit;
  `accountsevents` is published but has no consumer yet (still a contract). All 5 are the
  rule-5 shared surface. **All belong in `api/`.**
- **`*rpc` (9):** 7 of 9 consumed cross-module via `remote`/`cmd/gateway-svc`
  (`accountsrpc`, `accountsauthrpc`, `accountsadminrpc`, `charactersrpc`,
  `charactersplayerrpc`, `charactersadminrpc`, `inventoryrpc`). 2 are provider-local
  (`leaderboardrpc`, `matchrpc` — only their own `ops.go`). **Split nuance:** only the
  `Client`/`RouteBindings`/method-consts are consumed cross-module; the same package's
  `RegisterServer` is provider-only. Go package granularity ships the whole package
  together — a client/server package split is possible but NOT pursued here (adds churn for
  little gain; `RegisterServer` riding along in `api/` is generated glue the owner imports
  back, which is fine).
- **`*api` interfaces (6):** `adminapi` IS cross-module (7 modules contribute
  `adminapi.Item`). The 5 `<name>api` (accounts/characters/inventory/leaderboard/match) are
  NOT imported by other domain modules today — only their own module + their own rpc glue.
  BUT they are the canonical contract + the rpcgen input, and rpcgen requires `api`+`rpc` as
  **siblings** (the `//go:generate -out ../<name>rpc/…` relative paths). So they move as a
  unit with their rpc. `opsapi` is already top-level (`opsapi/`) — **does not move.**

### Blast radius (research)

- **rpcgen is move-friendly.** If `<name>api` and `<name>rpc` move together as siblings
  under `api/<name>/`, the 9 `//go:generate -out ../<name>rpc/…` directives are UNCHANGED
  (the relative sibling relationship is what matters). rpcgen derives the output package name
  from the `-out` dir basename and the api import path from `go/packages` — both self-adjust.
  The only hardcoded coupling is `edgePkgPath`/`opsapiPkgPath` consts in `tools/rpcgen/main.go`
  — **unchanged** because edge/opsapi don't move. `verify` directive discovery is a textual
  grep — location-agnostic, no change.
- **~43 intra-module import lines** (each module's impl importing its own sub-packages) +
  **cross-module importers** (`remote`, `cmd/gateway-svc`, `inventory`, `rating`,
  `leaderboard`, `audit` test) rewrite `gamebackend/modules/<name>/<sub>` →
  `gamebackend/api/<name>/<sub>`.
- **`.go-arch-lint.yml`:** `contracts.in` (10 entries), 9 rpcglue components, `mayDependOn`
  name references. Moving to a **disjoint `api/` tree** ELIMINATES the fragile
  "subpath-before-parent prefix-precedence" hazard (the rpcglue components no longer overlap
  `modules/<name>` prefixes) — enabling a cleaner component tree.
- **`verify.ps1` `$contractPkgs` + `verify.sh` `CONTRACT_PKGS`** (8 import paths).
- **Docs:** CLAUDE.md Layout + rule-5 recipe (step 3 names `modules/<name>/<name>events/`) +
  `docs/reference/architecture-enforcement.md`. Historical plan docs read stale — NOT edited.

### Target layout

```
api/
  accounts/    accountsapi  accountsevents  accountsrpc  accountsauthrpc  accountsadminrpc
  characters/  charactersapi charactersevents charactersrpc charactersplayerrpc charactersadminrpc
  inventory/   inventoryapi  inventoryrpc
  leaderboard/ leaderboardapi leaderboardrpc
  match/       matchapi      matchevents      matchrpc
  config/      configevents
  scheduler/   schedulerevents
  admin/       adminapi
modules/
  <name>/      # private impl ONLY (root .go files); imports its own api/<name>/… contract
```

**Package names keep their descriptive prefix** (`api/characters/charactersevents`, package
`charactersevents`) — flattening to `api/characters/events` would collide (many `events`
packages) and force import aliasing. `opsapi` stays at repo root (already top-level, not a
`<module>api`). `modules/<name>/` keeps only the impl root files.

**What does NOT get an `api/` folder:** modules with no public surface — `audit`, `rating`
(consume only), `remote` (transport), `messaging`, `gateway`, `webui` (infra). They keep no
sub-packages (verified: they have none).

### Scope decision (locked, with rationale)

Move the **whole api+events+rpc unit per module** to `api/<name>/`, including the 2
provider-local rpc packages (`leaderboardrpc`, `matchrpc`) — not because they're cross-module
(they aren't) but because (a) rpcgen needs them as siblings of their api, and (b) uniform
"public contract lives in api/" beats a per-package cross-module test that would scatter the
layout. The client/server package split (separating public `Client` from provider-private
`RegisterServer`) is explicitly deferred — not worth the churn now.

---

## Steps

### Step 1 — Create `api/` and `git mv` the 20 sub-packages `[sonnet]`

**(a) What.** Create `api/<name>/` per module and `git mv` each sub-package directory:
`modules/<name>/<sub>/` → `api/<name>/<sub>/` for all 20 (6 api + 5 events + 9 rpc). Keep
`api`+`rpc` siblings under the SAME `api/<name>/` so the `//go:generate -out ../<name>rpc/…`
relative paths stay valid. `adminapi` → `api/admin/adminapi`; `configevents` →
`api/config/configevents`; `schedulerevents` → `api/scheduler/schedulerevents`.

**(b) Why now / order.** Everything else (imports, arch-lint, verify) follows the move.

**(c) How — non-mechanical.** Use `git mv` (preserves history). Do NOT rename packages, only
relocate directories. After the move the tree WON'T build (imports stale) until Step 2 —
that's expected; Steps 1+2 land as one commit. Verify each `api/<name>/` keeps `<name>api`
and `<name>rpc` as direct-child siblings (rpcgen's relative `-out` assumption).

**(d) Dispatch:** `[sonnet]` — pure `git mv`, fully enumerated.

### Step 2 — Rewrite every import path `[sonnet]`

**(a) What.** For EACH of the 20 old import paths `gamebackend/modules/<name>/<sub>`, grep the
WHOLE repo (`--include=*.go`) for its importers and rewrite → `gamebackend/api/<name>/<sub>`.
Do NOT work from an illustrative list — iterate all 20 paths and rewrite every hit. Known
importer classes the executor MUST cover (reviewer M2 — the first cut's list was incomplete):
each module's root impl files (`~43` intra-module edges), the 5 `ops.go`, `parity_test.go` ×2,
`remote_test.go`, `modules/remote/remote.go`, `cmd/gateway-svc/main.go`,
`modules/inventory/inventory.go`, `modules/rating/rating.go`, `modules/leaderboard/leaderboard.go`,
**`modules/audit/audit.go`** (imports `schedulerevents` + `adminapi` — IMPL, not just the test),
`modules/config/listen.go`, `modules/scheduler/scheduler.go`,
`modules/accounts/{epic,password,epic_oauth,store}.go`, `tools/topiccheck` (imports events?),
and the **cross-tree** edges `charactersapi`→`adminapi` and `accountsapi`→`adminapi`
(a `characters`/`accounts` package importing `admin`'s contract — a same-name sed WON'T catch
these; they need the per-path grep). The grep-per-path mechanism catches all of them.

**(b) Why now / order.** Restores compilation after Step 1. Same commit as Step 1.

**(c) How — reviewer-corrected.** Scripted per-path rewrite (`grep -rl --include=*.go
"gamebackend/modules/<name>/<sub>"` → `sed -i`), for all 20 paths. The sed hits the generated
`*_gen.go` import lines too (they're plain `.go`), so **the goldens auto-update to the new
api path** — an explicit `go generate` is belt-and-suspenders, NOT required (reviewer m3). Use
plain **`gofmt -w`** during the rewrite, NOT `goimports` — goimports on a half-broken tree can
DROP a genuinely-used import it can't resolve (reviewer m2); run goimports only after the tree
is green. **END Step 2 with `go build ./...` GREEN + `rpcgen -check` green** before touching
arch-lint (reviewer: make the buildable checkpoint explicit).

**(d) Dispatch:** `[sonnet]` — mechanical per-path rewrite; verify-driven.

### Step 3 — Restructure `.go-arch-lint.yml` for the `api/` tree `[opus]`

**(a) What — the EXACT remap (reviewer M1: enumerate, don't "run until green").** go-arch-lint
FAILS HARD on any unattached file, so every one of the 20 moved packages must be relisted at
its new `in:` path. Component NAMES stay identical (so **no `mayDependOn` edits** — reviewer
M1 correction; the first cut overstated this). Only the `in:` paths change:

`contracts.in` (11) → `api/accounts/accountsevents`, `api/accounts/accountsapi`,
`api/characters/charactersevents`, `api/characters/charactersapi`, `api/config/configevents`,
`api/inventory/inventoryapi`, `api/leaderboard/leaderboardapi`, `api/match/matchevents`,
`api/match/matchapi`, `api/scheduler/schedulerevents`, `api/admin/adminapi`.

rpcglue components (9) → `charactersrpc: api/characters/charactersrpc`,
`charactersplayerrpc: api/characters/charactersplayerrpc`,
`charactersadminrpc: api/characters/charactersadminrpc`, `accountsrpc: api/accounts/accountsrpc`,
`accountsauthrpc: api/accounts/accountsauthrpc`, `accountsadminrpc: api/accounts/accountsadminrpc`,
`inventoryrpc: api/inventory/inventoryrpc`, `leaderboardrpc: api/leaderboard/leaderboardrpc`,
`matchrpc: api/match/matchrpc`.

**DROP the "declare-subpath-before-parent" ordering workaround** (the comment block at
`.go-arch-lint.yml:58-67`): since `api/<name>/…` is a **disjoint tree** from `modules/<name>/`,
the rpcglue components no longer prefix-overlap the module-impl components — verified: e.g.
`api/characters/charactersplayerrpc` is not a prefix-subpath of `modules/characters` any more,
so the ordering hazard genuinely disappears. Update the surrounding comments to reflect the new
disjoint layout.

**(b) Why now / order.** After Step 2's `go build` is green; arch-lint must accept the new tree.

**(c) How — verify each package attaches to EXACTLY ONE component.** `go-arch-lint check` fails
on an unattached OR doubly-matched file. After the remap, run it and confirm zero
"not attached" / "matched by multiple" errors. Preserve the invariants: leaf packages
(`bus`/`registry`/`opsapi`/`edge`/`contrib`) import no module; each module `mayDependOn`
contracts + its own rpcglue (names unchanged); `app` gains no module dep. No new top-level `api`
umbrella component (it would doubly-match the per-package entries) — keep per-package `in:` lines.

**(d) Dispatch:** `[opus]` — the 20-entry remap + invariant preservation; must be exact.

### Step 4 — Update verify + docs `[sonnet]`

**(a) What.** `verify.ps1 $contractPkgs` + `verify.sh CONTRACT_PKGS`: the 8 import paths →
`gamebackend/api/<name>/<sub>`. CLAUDE.md: Layout section (show `api/<name>/` alongside
`modules/<name>/`), rule-5 recipe step 3 (`api/<name>/<name>events/` not
`modules/<name>/<name>events/`), and rule 5's wording (the shared surface lives under `api/`).
`docs/reference/architecture-enforcement.md`: the contracts-surface description. Close out
memory [[separate-public-surface-from-impl]] (mark resolved).

**(b) Why now / order.** After the code + arch-lint land; keeps docs/verify honest.

**(c) How.** Mechanical path/text edits. apidiff's `$contractPkgs` MUST point at the new
paths or the advisory apidiff stage can't find the contracts.

**(d) Dispatch:** `[sonnet]` — path/text edits.

### Step 5 — Full verification `[opus]` gate

**(a) What.** `verify.ps1 --all` (all BLOCKING green) + both smokes
(`smoke-split-messaging.sh`, `smoke-split-operations.sh`) + `rpcgen -check` green + a monolith
curl sanity. Confirm `go-arch-lint check` green and that **no `*.go` file** still imports
`gamebackend/modules/<name>/<sub>` for a moved package (`grep --include=*.go` — scope to `.go`
so historical plan/CLAUDE.md/memory docs, which the plan deliberately leaves stale, don't
false-positive — reviewer m1).

**(b) Why now / order.** Final gate — proves the relocation is behavior-preserving.

**(c) How — honest about apidiff (reviewer M3).** apidiff CANNOT verify this commit: its
advisory stage snapshots the base by loading each `$contractPkgs` path **inside the HEAD
worktree**, but after Step 4 those paths are `gamebackend/api/<name>/…` which DON'T EXIST at
HEAD → the base-snapshot load fails and apidiff is effectively blind for this one commit (it's
advisory, so `--all` non-strict still passes, but it proves nothing here). So the REAL proof
that this is a pure relocation is: **`go build ./...` + `go vet` + `go-arch-lint check` +
`rpcgen -check` (goldens still generate byte-identically to their pre-move shape) + both split
smokes** (the split exercises the moved rpc clients + events cross-process over mTLS edge). Do
NOT cite apidiff as evidence — state plainly it can't verify a move. The next commit's apidiff
re-baselines against the new paths and resumes guarding shape.

**(d) Dispatch:** `[opus]` — final verification; honest that apidiff is blind for this commit.

---

## Dispatch summary (for approval)

| Step | Work | Lane |
|---|---|---|
| 1 | `git mv` 20 sub-packages → `api/<name>/` | `[sonnet]` |
| 2 | rewrite imports repo-wide + regenerate goldens | `[sonnet]` |
| 3 | `.go-arch-lint.yml` `api/` component tree (drop prefix hack) | `[opus]` |
| 4 | verify `$contractPkgs` + CLAUDE.md + arch-enforcement doc | `[sonnet]` |
| 5 | full verify + both smokes gate | `[opus]` |

Steps 1+2 land as ONE commit (the tree doesn't build between them). Commit per remaining step.

## Risks / notes (reviewer-corrected)

- **Low-risk, high-churn.** No behavior change — pure relocation. The BLOCKING gates
  (`go build`, `go vet`, `go-arch-lint check`, `rpcgen -check`) + the two split smokes catch
  any stale import, unattached arch-lint package, or path error. Confirmed: **no git pre-commit
  hook**, so Steps 1+2's deliberately non-building intermediate is safe within one commit.
- **Goldens auto-update via the sed (reviewer m3):** the per-path rewrite hits `*_gen.go`
  import lines too, so after `gofmt` the goldens reference the new api path and `rpcgen -check`
  passes — an explicit `go generate` is belt-and-suspenders, not required.
- **`gofmt` during the rewrite, `goimports` only after green (reviewer m2)** — goimports on a
  half-broken tree can drop a used-but-unresolvable import.
- **apidiff is BLIND for this commit (reviewer M3)** — new `$contractPkgs` paths don't exist at
  HEAD, so its base snapshot fails; it's advisory so `--all` passes but proves nothing. Real
  proof is build + arch-lint + rpcgen-check + smokes. Re-baselines next commit.
- **`opsapi` and `edge` do NOT move** — leaves, not module contracts; rpcgen's hardcoded
  `edgePkgPath`/`opsapiPkgPath` consts stay valid (verified).
- **Not pursued:** splitting each rpc package into public-client vs provider-server packages
  (defer — churn without clear payoff now).

## Review disposition (think-hard, 2026-07-07)

No blockers. M1→Step 3 now enumerates all 20 `in:` paths + notes mayDependOn needs no edits.
M2→Step 2 rewrites by per-path grep over all 20 paths, explicitly naming `audit.go` + the
cross-tree `*api`→`adminapi` edges. M3→Step 5 states apidiff is blind for this commit; proof =
build+arch-lint+rpcgen-check+smokes. m1→grep-zero scoped `--include=*.go`. m2→`gofmt` not
goimports mid-rewrite. m3→goldens auto-update via sed. m4→no pre-commit hook (confirmed).
Step order confirmed correct; Step 2 ends with an explicit `go build` checkpoint before Step 3.

## Resolved decisions (user, 2026-07-07)

1. **Package names: descriptive, all-lowercase** — `api/characters/charactersevents` (package
   `charactersevents`), the current convention, just relocated. Flattening (`events`/`rpc`)
   rejected — it forces import aliasing in `remote`/`gateway-svc`/`inventory` (they import
   several) and ambiguous call sites. camelCase considered (lint-clean here, `revive`/
   `stylecheck` are off) but rejected — stay lowercase-idiomatic. No package RENAMES; only
   directory relocation.
