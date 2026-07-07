# CLAUDE.md

Guidance for working in this repo. A for-fun game backend, built as a **modular
monolith**: one repo, one binary, but features are added by *writing new code,
not modifying existing code* (Open/Closed at the architecture level).

## The point of this codebase

Three seams carry all extensibility; almost everything else follows from them:

1. **Module registry** (`lifecycle`) — every feature is a `lifecycle.Module` and
   self-registers. The foundations never import a module; modules import them.
   Dependencies are declared as a manifest (`Requires()`) and are **NOT**
   topologically sorted: a two-phase build (every provider's `Register` runs before
   any module's `Init`) makes init order commutative, so no sort is needed. Import
   cycles are rejected by the Go compiler; a missing required service in a process's
   module set fails loudly at startup (`validateRequires`).
2. **Service registry** (`registry.Provide` / `Require`, over `ctx.Registry`) — for
   *synchronous* needs ("ask B now, get an answer"). The consumer asserts the service
   to its OWN local interface, so it depends on a capability, not a package.
3. **Event bus** (`ctx.Bus`) — the default glue, **async + fire-and-forget**.
   Reacting to something = subscribe. Each publishing domain owns a
   `<module>events` package declaring events via `bus.Define[T]("topic")`.

Plus a minor seam: **`Context.Contribute(slot, v)` / `Contributions(slot)`** — a
multi-value registry (unlike single-value `Provide`) for cross-cutting collections
where many modules contribute and one consumer reads them all (e.g. admin items).
A new contributor appears without the consumer being edited.

## Hard constraints (do not violate without discussing)

1. The foundations (`lifecycle`/`bus`/`registry`/`contrib`) never import a module.
   Dependency only ever points module → foundations.
2. Module implementations never import each other. Cross-module comms go through
   the bus (async) or a service interface from the registry (sync).
3. Synchronous dependency only "downward", toward foundations. Sideways reactions
   go through the bus. Declared `Requires()` must match real sync dependencies.
4. Depend on an interface/capability, not a package (consumer-defined interface).
5. The only deliberately shared surface between modules is each domain's
   `<name>events` package (payload types + the `bus.Define` descriptor). Two
   provider-owned adjuncts are likewise sanctioned for the *sync* path: `<name>api`
   — the provider's canonical capability interface + method-name constants,
   transport-free (the codegen input for `tools/rpcgen`), reached ONLY by the
   generated glue and `remote`, **never imported by domain consumers** (they keep
   their own local interface, rule 4) — and `<name>rpc`, the generated transport
   glue (impl-tier, may import `edge`). All three live under the top-level
   `api/<name>/` tree (never nested inside `modules/<name>/`), which is the
   module's private impl. Neither introduces a consumer→provider dependency; the
   registry swap still resolves a local interface.
6. Evolve events additively (new field / `FinishedV2`); never mutate a published
   payload shape — a structural change breaks consumers at compile time.
7. **The bus is async.** `Publish`/`Emit` never block and return nothing, so they
   can't be used for a synchronous answer — that's a service interface's job.
   State projected from events is eventually consistent.
8. Lifecycle: providers construct services in optional `Register` (phase 1, before
   any `Init`) → `Init` only wires up (no I/O) → optional `Migrate` (own schema) →
   optional `Start` (background work). Teardown is reverse registration order via
   optional `Stop`. Shutdown: stop HTTP → drain bus → `Stop` modules.
9. Events are typed: declare with `bus.Define`, publish/subscribe with
   `bus.Emit` / `bus.On`. No raw `e.Data.(T)` asserts in module code.
10. **Persistence = one shared Postgres, full *logical* isolation.** Each module
    owns its own schema and touches no other module's tables. **No cross-module
    foreign keys.** A relation to another module is its id stored as a plain
    column, resolved via interface or synced via events. `ctx.DB` is offered, not
    mandated — a module may bring its own store instead.

## Adding a module (the recipe)

1. New folder `modules/<name>/`. Implement `lifecycle.Module` (`Name`, `Requires`,
   `Init`). Use a pointer receiver if it holds state (db, logger, caches). If it
   PROVIDES a service, also implement `lifecycle.Registrar` (`Register`) so the
   service is registered before any module's `Init`.
2. If it persists data: implement `lifecycle.Migrator` and create ONLY your own
   schema (`CREATE SCHEMA IF NOT EXISTS <name>; CREATE TABLE IF NOT EXISTS <name>....`).
3. If it publishes events: add `api/<name>/<name>events/` with
   `var XEvent = bus.Define[XPayload]("<name>.x")`. Emit with `bus.Emit`. If it
   exposes a sync capability to other modules, likewise add `api/<name>/<name>api/`
   (interface + codegen input) and the generated `api/<name>/<name>rpc/` glue.
4. If it runs background work or holds resources: implement `lifecycle.Starter` /
   `lifecycle.Stopper`.
5. Register it with one line in `cmd/server/main.go`. Touch nothing else.

## Accounts & identity

The `accounts` module owns player identity. The **production model is federation**:
the backend is a *trusted verifier* of an external IdP's token (EOS Connect model),
never a password holder. One product-scoped `player_id`, many credential providers
over it (`identities(provider, subject) → player_id`), opaque DB-backed `sessions`.

- **dev / password** — local-only self-registration for testing. Gated by
  `ACCOUNTS_DEV_AUTH` (default ON locally, logs a loud warning; turn OFF in prod).
- **epic** — real OIDC verifier (defaults to Epic Account Services endpoints,
  `sub` = Epic Account ID). Enabled when `EPIC_CLIENT_ID` is set. Adding Google
  later = another configured OIDC verifier, same shape.
- **epic web OAuth** — when `EPIC_CLIENT_SECRET` is also set, the backend runs the
  EAS authorization-code flow: `POST /accounts/epic/start` (returns the authorize
  URL; if called with a bearer it binds a LINK to that session) and
  `GET /accounts/epic/callback` (exchanges the code, verifies the id_token, then
  links to the session's player or logs in). State→session is held in memory.

The `webui` module serves a single-page demo at `/` (dev login, then "Link Epic")
so the linking flow is visible in a browser. Config env: `EPIC_CLIENT_SECRET`,
`EPIC_REDIRECT_URI` (default `http://localhost:8080/accounts/epic/callback`),
`EPIC_AUTHORIZE_URL`, `EPIC_TOKEN_URL`.

Emits `accountsevents.PlayerRegistered`. Not yet wired into match/rating.

## Characters & inventory (the modularity reference case)

A worked example of cross-module relations under logical isolation:
- `characters` (depends on accounts) — a player has N characters; `player_id` is a
  plain column, no FK. Emits `charactersevents.Created/Deleted`.
- `inventory` (depends on accounts + characters) — holdings for a polymorphic
  `Owner{Type: player|character, ID}`; `owner_id` is a plain ref, no cross-module FK.
  It SYNC-asks `characters.OwnerOf` to authorize a character's inventory, AND
  REACTS to character events: grant a starter item on create, wipe holdings on
  delete. `characters` has no idea inventory exists.

The deletion-cleanup is the point: integrity across modules comes from an event,
not an FK cascade (verified — no orphan holdings remain). Both modules contribute
admin items, so they appear at `/admin` with no change to the admin module.
The same listener will handle `player.deleted` once that exists.

## Commands

```
go build ./...          # build everything
go vet ./...            # vet
go test ./...           # unit tests + rapid property tests + fuzz seed corpus
go run ./cmd/server     # run (needs a reachable Postgres)
go-arch-lint check      # enforce the module boundaries (see docs/reference/architecture-enforcement.md)
golangci-lint run ./... # correctness/leak/security lint (.golangci.yml)
```

**One-shot verification net — `./verify.sh` (bash) / `./verify.ps1` (PowerShell):** runs every gate,
keeps going after failures, prints a PASS/FAIL/SKIP table, exits non-zero iff a BLOCKING stage failed.
No CI — this script IS the automated safety net. Flags: `--fast`(default, blocking only)/`--all`(+advisory)
/`--slow`(+mutation)/`--strict`(advisory failures also fail exit)/`--no-install`.
- BLOCKING: build, vet, golangci-lint, go-arch-lint, `go test ./...`, `govulncheck ./...`.
- ADVISORY (`--all`): `go test -race` (SKIPs without cgo/gcc), fuzz (`-fuzztime` per `Fuzz*`),
  apidiff vs HEAD on the `*events`/`adminapi` contracts (additive-only guard), topiccheck.
- SLOW (`--slow`): gremlins mutation on the pure pkgs (edge/gateway/outbox/registry/bus).
- Auto-installs pinned CLIs if missing: `govulncheck@v1.5.0`, `apidiff@latest`, `gremlins@v0.6.0`
  (`--no-install` to skip → those stages SKIP). Extra checks:
  - **fuzz** (`go test -fuzz`) — edge decode paths (`edge/fuzz_test.go`); seed corpus runs in plain `go test`.
  - **rapid** property tests (`pgregory.net/rapid`) — codec/frame round-trip, prefix longest-match,
    outbox `deliver` ordering, registry Provide/Require.
  - **topiccheck** (`go run ./tools/topiccheck ./...`) — flags a `bus.Define` topic with no `bus.On`
    subscriber; allowlist via `//topiccheck:allow-unsubscribed` comment above the `var`.
  - **apidiff** — fails if a published event payload changed non-additively (rule 6).

Two complementary gates:
- **`go-arch-lint`** (`go install github.com/fe3dback/go-arch-lint@latest`) checks *architecture*
  against `.go-arch-lint.yml`: core imports no module, a module's impl is reachable only from
  `cmd`, modules talk only through the `<name>events`/`adminapi` contracts under `api/<name>/`
  (plus the provider-owned `<name>api` interface + its generated `<name>rpc` glue for the sync
  path). (Cycles need no rule — the Go compiler rejects them.)
- **`golangci-lint`** (v2; `go install github.com/golangci/golangci-lint/v2/cmd/golangci-lint@latest`)
  checks *correctness/leaks/security* via `.golangci.yml` — a curated high-signal set (errcheck,
  staticcheck, gosec, bodyclose, sqlclosecheck, rowserrcheck, errorlint, exhaustive, …), not a
  style gate.

Smoke test:
```
curl -X POST localhost:8080/match/report -d '{"Winner":"alice","Loser":"bob"}'
curl localhost:8080/leaderboard
```

## Database

Connection from `DATABASE_URL`, default
`postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable`.
(Admin/superuser credentials for provisioning are kept in the local agent memory,
not committed.) psql:

```
PGPASSWORD=gamebackend "/c/Program Files/PostgreSQL/18/bin/psql.exe" -U gamebackend -h localhost -d gamebackend
```

## Layout

```
cmd/server/main.go            # the only place that lists all modules
lifecycle/                    # Module/Context/App: builds, migrates, starts, stops modules
bus/ registry/ contrib/       # leaf foundations: async event bus, sync service registry, multi-value slots

api/                          # the shared contract surface — one tree per domain, importable by any module
  accounts/
    accountsevents/           #   published events (PlayerRegistered)
    accountsapi/               #   provider's canonical capability interface (sync path, codegen input)
    accountsrpc/ accountsauthrpc/ accountsadminrpc/  # generated transport glue (impl-tier)
  match/matchevents/          # published events of the match domain (descriptor + payload)
  match/matchapi/ match/matchrpc/
  scheduler/schedulerevents/
  characters/
    charactersevents/         #   published events (Created/Deleted)
    charactersapi/ charactersrpc/ charactersplayerrpc/ charactersadminrpc/
  inventory/inventoryapi/ inventory/inventoryrpc/
  leaderboard/leaderboardapi/ leaderboard/leaderboardrpc/
  config/configevents/
  admin/adminapi/             #   contract: Item/Content/KPI/Table/Cell + the "admin.item" slot

modules/                      # private impl only — never imported by another module, contracts live in api/ above
  config/                     # DB-backed operational knobs (schema "config"); LISTEN/NOTIFY live-reload, emits config.changed
  accounts/                   # player identity: dev(password) + epic(OIDC) providers, owns schema "accounts"
    store.go password.go epic.go
  match/match.go              # impl: depends on "rating" (sync), emits match.finished
  rating/rating.go            # impl: provides "rating" service, reacts to matches (in-memory)
  leaderboard/leaderboard.go  # impl: Postgres-backed listener, owns schema "leaderboard"
  characters/                 # player characters (N per player); depends on accounts; owns schema "characters"
  inventory/                  # owner-scoped inventories (player|character); depends on accounts+characters
  webui/                      # UI-only module: serves the SPA demo at "/" (embedded index.html)
  admin/                      # GameOps admin portal at "/admin" (theme + shell); renders contributed items
```

## Admin portal

The `admin` module serves the GameOps console at `/admin`. It owns the LOOK (the
dark GameOps theme in `theme.css` + the sidebar/header shell in `admin.html.tmpl`,
both embedded) and composes a navigable sidebar from items modules **contribute** to
the `adminapi.Slot` — it never imports a module's implementation or touches another
schema. A module appears by contributing `adminapi.Item{Section, Label, Render}`;
items are grouped by Section in the sidebar and each opens a dedicated page whose
`Render` returns declarative widgets (`KPI`s + a `Table` of `Cell`s with badges/mono);
the admin owns rendering. Visual direction comes from `UILayout/` (a Claude Design
mockup — a spec, not runnable). Gate with `ADMIN_USER`/`ADMIN_PASS` (HTTP Basic);
unset = open + loud warning (local only).

---

# Working agreements

The sections below are general workflow rules (research, planning, implementation,
git). They are project-agnostic and adapted from a shared house style.

## Owning Mistakes — MANDATORY

When the user catches me ignoring an instruction, violating a documented rule
(CLAUDE.md, memory), or fabricating something (made-up API, invented path,
hallucinated behavior, false claim of work done):

1. **Name the specific mistake directly** — no hedging, no "I may have", no burying
   it in context.
2. **Don't minimize, deflect, or rationalize** — don't explain why the wrong thing
   was reasonable; don't blame tools/context/ambiguity. The response is "you're
   right, I screwed up on X."
3. **State the corrected behavior** concretely.
4. **Then fix it.** One or two sentences of repentance, not a wall. Sycophantic
   "great catch!" openers are not repentance.

For repeat offenses, also save/update the relevant feedback memory.

**Resignation letter for MANDATORY violations.** When caught violating any `## … —
MANDATORY` rule, before the fix write a short (≤8-line) resignation letter addressed
to the user: name the exact section, **state explicitly what error was committed**
(one sentence: what I did vs what the rule required), the impact, and the corrective
action. This is *in addition to* the four steps above — a visible named admission, no
theatrical self-flagellation, then the fix. **Then update memory** — save/update the
relevant feedback memory for the violated rule (not only for repeat offenses).

## Research before planning — MANDATORY

This is a modular monolith built on Open/Closed — new features are *new code*, not
edits to existing code. So before any plan proposing a new module, service, event,
or admin section (or a replacement), first **map the overlapping existing systems**.
The three seams (module registry, service registry `Provide`/`Require`, event bus)
plus the `Contribute`/`Contributions` slot mean a capability you want often already
exists or has a near-twin. For each candidate, document in the plan's Context: what
it does, how it differs, and an explicit **"why not extend / depend on X"**. A plan
that adds a module without that rationale is incomplete — lead with evidence, not
enthusiasm for new code.

## Research / Search Mode — MANDATORY

Before any non-trivial research/search, ask the user **"how should I research
this?"** Don't default to grep — one grep pass is lossy (misses interface
satisfaction, embedded methods, generated code, event subscribers wired by string
topic, the registry/reflection-driven surface). Treat any single grep sweep as a
**lower bound, not the answer**, and say which method you used. "Non-trivial" =
mapping an API surface, finding all callers, understanding data flow, locating
wiring, surveying overlap; one-shot lookups with a known file+symbol proceed without
asking.

**Method menu (gopls/LSP, parallel subagents, targeted read, grep) + subagent-count
bands: [docs/reference/research-mode.md](docs/reference/research-mode.md); shared
Agent-call invariants: [docs/reference/subagent-dispatch.md](docs/reference/subagent-dispatch.md).**
Any code-touching subagent gets the nav guidance pasted into its prompt — it does not
inherit.

## Plans & Status Docs — MANDATORY

Store **all** planning/design/status/progress/summary docs inside the repo — never
on a scratch drive or temp path. The repo is the single source of truth.

- **Plans:** `docs/plans/YYYY-MM-DD-HHMM-<kebab-topic>-plan.md`
- **Status/progress/fix/summary:** `docs/<subdir>/YYYY-MM-DD-HHMM-<kebab-topic>-<status|progress|fix|summary>.md`
- **Reference (durable knowledge):** `docs/reference/<topic>.md`

The `-HHMM` suffix is mandatory so files sort chronologically by listing. Never put
plan/status files at repo root or in a temp dir.

## Plan Writing Workflow — MANDATORY

Front-load the thinking. For any plan (plan mode / "write me a plan" / a
`docs/plans/…-plan.md`), in order — no skipping for "it's small":

1. **Ask how many research subagents** (2–4 / 4–8 / 8–12 bands). Ask **every time**,
   even mid-session — count is task-specific. Pass `model:` explicitly.
2. **Research subagents on 3 non-overlapping angles:** API surface / API usages /
   patterns. Synthesize in the main model — never write off one subagent.
3. **Write concrete specifics:** exact files, signatures, API calls from step 2,
   sequencing. **Banned phrases** ("figure out as we go", "TBD", "investigate during
   implementation", "may need to", "something like", …) = research gap → back to step 2.
4. **Structure as an ordered `Step 1 → Step 2 → …` sequence, NOT a catalog.** Each
   step states **(a) what** is touched (exact files/symbols), **(b) why now / order** —
   the dependency forcing it before the next, **(c) how** — non-mechanical moves
   spelled out, **(d) dispatch tag** — `[inline]`/`[fable]`/`[opus]`/`[sonnet]`. A
   catalog that leaves order/topology/per-step actions to "figure as you go" is
   **banned**; steps need not each compile, but every step MUST be written out.
5. **Dispatch one grumpy senior-engineer reviewer** at session tier (separate context
   = the independent-reviewer boundary). **Ask the user the think-effort level first**
   (default / think / think hard / ultrathink) — effort does NOT inherit, so embed it
   in the reviewer's prompt. It produces a punch list, does **not** rewrite. Address
   it before showing the user (or note deferred items with rationale).

**Full detail (catalog-vs-sequence failure mode, step-4 a/b/c/d examples, reviewer
checklist): [docs/reference/plan-writing-workflow.md](docs/reference/plan-writing-workflow.md).**

## Implementation Mode — MANDATORY

**Mixed dispatch — decided per plan step, not per session. Tags name a CONCRETE
model, not a tier alias.** Four lanes, each set at plan-writing time (Plan Writing
step 4d):

- `[inline]` — main model writes in this context. **No independent review** —
  reserved for mid-edit judgment that can't be handed off. Default complex work to a
  subagent lane, not `[inline]`.
- `[fable]` — Fable 5 subagent. Top tier; for complex/correctness-critical work (new
  API design, the bus/registry seams, lifecycle ordering, cross-module context) **when
  Fable is the session model**.
- `[opus]` — Opus 4.8 subagent. Substantive implementation. **While the session is
  Opus, `[opus]` is also the top-tier lane** — same tier as inline but a separate
  context, the independent-reviewer boundary.
- `[sonnet]` — Sonnet subagent. Mechanical: rename sweeps, scaffolding, N-similar
  edits, applying a fully-specified step, compile fixes, tests from a pattern,
  config. **Never burn a higher tier on a rename.** Visual/UI design is never
  `[sonnet]`.

**Every code-writing Agent call passes an explicit `model:` matching its lane —
NON-NEGOTIABLE** (there is no "inherit" path): `[fable]`→`model:"fable"`,
`[opus]`→`model:"opus"`, `[sonnet]`→`model:"sonnet"` (listing-only research →
`model:"haiku"`). Pre-flight every Agent call for the field. After a multi-subagent
rollout, before "done": `git log -<N> --format="%h %B" | grep "Co-Authored"` and
confirm trailers match each lane (`[fable]`→Fable 5, `[opus]`→Opus 4.8,
`[sonnet]`→Sonnet 4.6) — surface mismatches immediately.

The user approves the tags with the plan (called out at ExitPlanMode). Ask only for
untagged/ad-hoc work, and if any step is a subagent lane also ask **"what effort
level?"** (effort does NOT inherit — embed it in the prompt). Review each diff against
its plan step before dispatching the next; commit after each task (subagents may
commit their own work). Mid-rollout, don't silently flip a tag — ask.

**Cross-cutting Agent-call invariants (explicit `model:`, effort/nav-guidance don't
inherit, trailer, concise prompts) — shared by research + implementation:
[docs/reference/subagent-dispatch.md](docs/reference/subagent-dispatch.md). Lane
heuristic, dispatch rules, refactor safety:
[docs/reference/implementation-mode.md](docs/reference/implementation-mode.md).**

## Agent memory backup — MANDATORY

The Claude Code project memory lives OUTSIDE the repo
(`$HOME/.claude/projects/<mangled-repo-path>/memory/`, per-machine path). It is
mirrored into the repo at `memory/` so it survives across machines via git.

- **After ANY change to memory** (write/update/delete a memory file or `MEMORY.md`),
  run `scripts/memory-sync.sh push` (or `.ps1`) — it mirrors live → `memory/` and
  commits `chore(memory): …`. Don't hand-copy; the script handles deletions too.
- **After a `git pull`/sync**, run `scripts/memory-sync.sh pull` — it mirrors the
  git copy back to this machine's live memory dir. Do this before relying on recall.
- The live path is derived (repo abspath → non-alnum→`-`), so scripts are portable;
  override with `CLAUDE_MEMORY_DIR` if detection is ever wrong. `… path` prints it.

## Git Safety — MANDATORY

**Never `git stash`, `git checkout -- <file>`, `git restore`, or anything that
discards/overwrites uncommitted working-tree changes** without the user's say-so. To
inspect old contents use `git show <sha>:<path>`. Only ever `git reset --soft HEAD~1`
to undo a commit *you* just created *this turn*, and only when nothing else has
committed since. Never `git push --force` or rewrite published history without
explicit instruction. Commit or push only when the user asks; if on the default
branch, branch first.

## Commit Message Format — MANDATORY

Use **Conventional Commits**: `<type>(<scope>): <imperative description>` — `type` ∈
feat/fix/refactor/test/docs/chore; `scope` = lowercased module/package, comma-separate
multiples (`fix(match,rating): …`). NOT bracketed `[Module]` scopes. Multi-step
rollouts may note `(Step N — …)`.

**`Co-Authored-By` trailer reflects the EXECUTING model**, overriding the harness
default (which hardcodes Opus 4.8): Opus → `Claude Opus 4.8`, Fable → `Claude Fable
5`, Sonnet subagent → `Claude Sonnet 4.6` (all `<noreply@anthropic.com>`). When
dispatching a code-writing subagent, put **its model's** trailer in the prompt — this
is what the trailer audit (Implementation Mode) checks.

**Examples + scope conventions: [docs/reference/commit-format.md](docs/reference/commit-format.md).**
