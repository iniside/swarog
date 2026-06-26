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
