# CLAUDE.md

Guidance for working in this repo. A for-fun game backend, built as a **modular
monolith**: one repo, one binary, but features are added by *writing new code,
not modifying existing code* (Open/Closed at the architecture level).

## The point of this codebase

Three seams carry all extensibility; almost everything else follows from them:

1. **Module registry** (`core`) — every feature is a `core.Module` and self-
   registers. The core never imports a module; modules import the core.
   Inter-module dependencies are declared (`DependsOn`) and topologically ordered;
   cycles and missing deps fail loudly at startup.
2. **Service registry** (`Context.Provide` / `Require`) — for *synchronous* needs
   ("ask B now, get an answer"). The consumer asserts the service to its OWN local
   interface, so it depends on a capability, not a package.
3. **Event bus** (`Context.Bus`) — the default glue, **async + fire-and-forget**.
   Reacting to something = subscribe. Each publishing domain owns a
   `<module>events` package declaring events via `core.Define[T]("topic")`.

Plus a minor seam: **`Context.Contribute(slot, v)` / `Contributions(slot)`** — a
multi-value registry (unlike single-value `Provide`) for cross-cutting collections
where many modules contribute and one consumer reads them all (e.g. admin sections).
A new contributor appears without the consumer being edited.

## Hard constraints (do not violate without discussing)

1. Core never imports a module. Dependency only ever points module → core.
2. Module implementations never import each other. Cross-module comms go through
   the bus (async) or a service interface from the registry (sync).
3. Synchronous dependency only "downward", toward foundations. Sideways reactions
   go through the bus. Declared `DependsOn` must match real sync dependencies.
4. Depend on an interface/capability, not a package (consumer-defined interface).
5. The only deliberately shared surface between modules is each domain's
   `<module>events` package (payload types + the `core.Define` descriptor).
6. Evolve events additively (new field / `FinishedV2`); never mutate a published
   payload shape — a structural change breaks consumers at compile time.
7. **The bus is async.** `Publish`/`Emit` never block and return nothing, so they
   can't be used for a synchronous answer — that's a service interface's job.
   State projected from events is eventually consistent.
8. Lifecycle: `Init` only wires up (no I/O) → optional `Migrate` (own schema) →
   optional `Start` (background work). Teardown is reverse order via optional
   `Stop`. Shutdown: stop HTTP → drain bus → `Stop` modules.
9. Events are typed: declare with `core.Define`, publish/subscribe with
   `core.Emit` / `core.On`. No raw `e.Data.(T)` asserts in module code.
10. **Persistence = one shared Postgres, full *logical* isolation.** Each module
    owns its own schema and touches no other module's tables. **No cross-module
    foreign keys.** A relation to another module is its id stored as a plain
    column, resolved via interface or synced via events. `ctx.DB` is offered, not
    mandated — a module may bring its own store instead.

## Adding a module (the recipe)

1. New folder `modules/<name>/`. Implement `core.Module` (`Name`, `DependsOn`,
   `Init`). Use a pointer receiver if it holds state (db, logger, caches).
2. If it persists data: implement `core.Migrator` and create ONLY your own schema
   (`CREATE SCHEMA IF NOT EXISTS <name>; CREATE TABLE IF NOT EXISTS <name>....`).
3. If it publishes events: add `modules/<name>/<name>events/` with
   `var XEvent = core.Define[XPayload]("<name>.x")`. Emit with `core.Emit`.
4. If it runs background work or holds resources: implement `core.Starter` /
   `core.Stopper`.
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
an admin section, so they appear at `/admin` with no change to the admin module.
The same listener will handle `player.deleted` once that exists.

## Commands

```
go build ./...          # build everything
go vet ./...            # vet
go test ./...           # unit tests (core registry/lifecycle order, cycle detection)
go run ./cmd/server     # run (needs a reachable Postgres)
```

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
core/                         # Module, Context, Registry, Bus, typed events — no game knowledge
modules/
  accounts/                   # player identity: dev(password) + epic(OIDC) providers, owns schema "accounts"
    accountsevents/           #   published events (PlayerRegistered)
    store.go password.go epic.go
  match/matchevents/          # published events of the match domain (descriptor + payload)
  match/match.go              # impl: depends on "rating" (sync), emits match.finished
  rating/rating.go            # impl: provides "rating" service, reacts to matches (in-memory)
  leaderboard/leaderboard.go  # impl: Postgres-backed listener, owns schema "leaderboard"
  characters/                 # player characters (N per player); depends on accounts; owns schema "characters"
    charactersevents/         #   published events (Created/Deleted)
  inventory/                  # owner-scoped inventories (player|character); depends on accounts+characters
  webui/                      # UI-only module: serves the SPA demo at "/" (embedded index.html)
  admin/                      # GameOps admin portal at "/admin" (theme + shell); renders contributed sections
    adminapi/                 #   contract: Section/Content/KPI/Table/Cell + the "admin.section" slot
```

## Admin portal

The `admin` module serves the GameOps console at `/admin`. It owns the LOOK (the
dark GameOps theme in `theme.css` + the sidebar/header shell in `admin.html.tmpl`,
both embedded) and composes the dashboard from sections modules **contribute** to
the `adminapi.Slot` — it never imports a module's implementation or touches another
schema. A module appears by contributing `adminapi.Section{Title, Render}` whose
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
topic, reflection/registry-driven surface). Treat any single grep sweep as a **lower
bound, not the answer**, and say which method you used.

Offer the fitting subset:

- **LSP / gopls** — Go symbol nav with a file+line anchor (definition, references,
  implementations). Preferred for "where is X defined / who calls Y / what satisfies
  this interface".
- **Parallel research subagents** — fan out cheap subagents, each a distinct
  **non-overlapping** angle (e.g. API surface / callers+consumers / event
  publishers+subscribers / config+env wiring). Pass `model:` explicitly. If picked,
  ask **"how many?"** → **2–4** narrow / **4–8** multi-module / **8–12** whole-repo
  survey. A subagent does not inherit this fallback chain — paste "LSP/gopls first,
  grep is a labelled lower bound" into each prompt, and require each to report which
  method it used.
- **Targeted main-model read** — small surface, one file end-to-end.
- **Grep/Glob** — only when nothing else fits; acknowledge it's a lower bound.

"Non-trivial" = mapping an API surface, finding all callers, understanding data
flow, locating wiring, surveying overlap. One-shot lookups with a known file+symbol
can proceed without asking.

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
`docs/plans/…-plan.md`), in order, no skipping for "it's small":

1. **Ask how many research subagents** (2–4 / 4–8 / 8–12). Ask **every time**, even
   mid-session — count is task-specific. Pass `model:` explicitly.
2. **Research subagents on 3 non-overlapping angles:** *API surface* (every public
   function / type / interface with full signatures), *API usages* (concrete call
   sites — who constructs, who consumes, how args are filled), *patterns* (idioms to
   reuse — how existing modules register, migrate, subscribe). Synthesize in the main
   model — never write off one subagent.
3. **Write concrete specifics:** exact files (repo-relative paths), exact
   function/type signatures, exact API calls drawn from step 2, sequencing + what
   each step compiles/tests against. **Banned phrases:** "figure out as we go",
   "TBD", "investigate during implementation", "may need to", "something like",
   "we'll see what shape this takes" — any of these = research gap; go back to step 2.
4. **Structure the plan as an ordered step sequence, NOT a catalog.** A catalog
   (files-to-create table + a list of call-sites + one big "build at the end") leaves
   implementation order and dependency topology as "figure as you go" — that is the
   failure mode and it is **banned**. The plan body must be `Step 1 → Step 2 → …`
   where each step spells out: **(a) what** is touched (exact files/symbols), **(b)
   why now / in what order** — the dependency that forces this step before the next,
   **(c) how** — the concrete actions, **(d) dispatch tag** — `[inline]`, `[session]`,
   or `[sonnet]` (see Implementation Mode). Steps do NOT each have to compile in
   isolation, but every step MUST be written out: a reader follows them top-to-bottom
   without inventing the order. Reference material (Context, file tables) is fine as
   supporting sections, but it does not replace the ordered steps.
5. **Dispatch one grumpy senior-engineer reviewer on the session model** (omit
   `model` so it inherits the session model — the independent-reviewer boundary is
   the point). It hunts logical holes, missing pieces (migration? test? a declared
   `DependsOn` that doesn't match the real sync dependency? an event mutated instead
   of evolved additively?), ambiguity, "figure-it-out-later" smell. It produces a
   punch list, does **not** rewrite. Address the list before showing the user (or
   note deferred items with rationale).

## Implementation Mode — MANDATORY

**Mixed dispatch — decided per plan step, not per session.** Three lanes:
`[inline]` (main model writes in this context), `[session]` (a subagent on the
**session model**, separate context), `[sonnet]` (a subagent on Sonnet). Every plan
step carries a dispatch tag set at plan-writing time:

- `[session]` — **the default for complex/correctness-critical work** (new API
  design, the bus/registry seams, lifecycle ordering, cross-module context). A
  subagent runs on the session model but in a **separate context**, so the main model
  reviews its diff from the outside instead of grading its own homework. The review
  boundary is the whole point.
- `[inline]` — reserved for genuine mid-edit judgment that **can't be handed off**:
  the decision depends on context the main model is holding live. Default complex
  work to `[session]`, not `[inline]`. Accept that inline work gets no independent
  review.
- `[sonnet]` — mechanical work: rename sweeps, scaffolding, N-similar edits, applying
  a fully-specified plan step, compile fixes, tests from an existing pattern,
  config. **Never burn main-model tokens on a rename** — if a step is mechanical, it
  is `[sonnet]` even when surrounding steps are `[session]`/`[inline]`.

The user approves the tags together with the plan. Subagent dispatch rules:

1. **Every code-writing Agent call carries a deliberate model decision.** A
   `[sonnet]` step **MUST** pass `model:"sonnet"` explicitly (omitting silently runs
   on the session model — a bug); a `[session]` step **MUST** omit `model` so the
   subagent inherits the session model. If a mechanical step lacks `model:"sonnet"`,
   STOP and add it.
2. **Review between tasks.** Main model reviews each diff against the plan step (did
   what the plan said? touched out-of-scope files? introduced conflicting patterns —
   e.g. a module importing another module's package, a cross-module foreign key?)
   before dispatching the next. No parallel fan-out for sequential plan steps.
3. **Trust but verify.** Read the actual edits — self-reports describe intent, not
   result.
4. **Commit after each task.** Granular history beats per-commit-compiling — commit
   right after a task verifies. A bad subagent commit is fixed with a follow-up
   commit, never by discarding history.

## Git Safety — MANDATORY

**Never `git stash`, `git checkout -- <file>`, `git restore`, or anything that
discards/overwrites uncommitted working-tree changes** without the user's say-so. To
inspect old contents use `git show <sha>:<path>`. Only ever `git reset --soft HEAD~1`
to undo a commit *you* just created *this turn*, and only when nothing else has
committed since. Never `git push --force` or rewrite published history without
explicit instruction. Commit or push only when the user asks; if on the default
branch, branch first.

## Commit Message Format — MANDATORY

Use **Conventional Commits**: `<type>(<scope>): <imperative description>`.

- `type` ∈ `feat`, `fix`, `refactor`, `test`, `docs`, `chore`.
- `scope` is the lowercased module/package (`accounts`, `match`, `rating`,
  `leaderboard`, `admin`, `core`). Multiple scopes comma-separated:
  `fix(match,rating): …`.
- Multi-step rollouts may note the step in the description: `(Step 1 — A+B+C)`.

```text
feat(accounts): add Epic OIDC verifier behind EPIC_CLIENT_ID
fix(leaderboard): create schema in Migrate, not Init
test(core): cover cycle detection in the module registry
```

Do **NOT** use bracketed `[Module]` scopes — that is the wrong format.
