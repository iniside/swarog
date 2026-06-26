# GameBackend

A modular-monolith game backend. One repo, one binary — but built so features are
added by **writing new code, not modifying existing code** (Open/Closed at the
architecture level).

## Core idea

Three seams, and nothing else, carry all extensibility:

1. **Module registry** (`core`) — every feature implements the `Module` interface
   and self-registers. The core never imports a module; modules import the core.
   Dependencies between modules are declared (`DependsOn`) and topologically
   ordered at startup; cycles and missing deps fail loudly.

2. **Service registry** (`Provide` / `Require`) — for *synchronous* needs ("I must
   ask B right now and get an answer"). The consumer asserts the service to its
   own local interface, so it depends on a capability, not on a package.

3. **Event bus** (`Bus`) — the default glue, **asynchronous and fire-and-forget**.
   "I want to react when X happens" => subscribe. `Publish` never blocks and
   returns nothing, so it structurally cannot be used for a synchronous answer
   (use a service interface for that). Each subscriber has its own goroutine and
   FIFO mailbox: per-subscriber order is preserved, a slow/panicking subscriber
   is isolated, and `Close` drains in-flight events on shutdown. State built from
   events is therefore **eventually consistent**. Each publishing domain owns a
   `<module>events` package (depends only on the core foundation) that declares
   its events with `core.Define[Payload]("topic")`. Publish and subscribe go
   through the typed `core.Emit` / `core.On`, so topic-vs-payload mismatches are
   compile errors — the only deliberately shared surface is that descriptor.

## Lifecycle

Modules have three phases. `Init` (required) only wires things up — register
services, subscribe, mount routes; no I/O. `Start` and `Stop` are optional
capabilities (implement `core.Starter` / `core.Stopper` only if you need them):
`Start` kicks off background work in dependency order, `Stop` tears down in
reverse so a module's dependencies outlive it. Shutdown is: stop HTTP, drain the
bus, then `Stop` modules. A fourth optional capability, `core.Migrator`, runs
between Init and Start to create a module's own schema.

## Persistence — full logical isolation

One shared Postgres database, exposed by the core as `ctx.DB` (offered, not
mandated — a module may ignore it and bring its own store). Isolation is
**logical, not physical**: there is a single Postgres, but each module owns its
own schema and touches no other module's tables. No cross-module foreign keys.
A relation to another module is just that module's id stored as a plain column,
resolved via its interface or kept in sync via events (eventually consistent).
This keeps every module's schema private and independently extractable later.

- `leaderboard` owns schema `leaderboard`; its win counts persist across restarts.
- `accounts` owns schema `accounts` (players, identities, sessions).

## Accounts & identity

The backend is a **trusted verifier**, not an identity provider (the EOS Connect
model): one product-scoped `player_id`, many credential providers mapping
`(provider, subject) → player_id`, with opaque DB-backed sessions.

- **dev / password** — local self-registration for testing, gated by
  `ACCOUNTS_DEV_AUTH` (default ON locally, logs a warning; OFF in production).
- **epic** — verifies an EOS Connect ID Token against Epic's JWKS; enabled only
  when `EPIC_CLIENT_ID` is set (`EPIC_JWKS_URL`, `EPIC_ISSUER_PREFIX` configurable).
  First verified token auto-provisions a player. Google would be the same shape.

```
curl -X POST localhost:8080/accounts/register -d '{"email":"a@b.c","password":"pw","displayName":"Al"}'
curl localhost:8080/accounts/me -H "Authorization: Bearer <token>"
```

### Web demo (account linking)

The `webui` module serves a one-page demo at `http://localhost:8080/`: register or
log in with the dev provider, then **Link Epic account** runs the real Epic Account
Services OAuth flow and attaches the Epic identity to your player. Enable it with:

```
EPIC_CLIENT_ID=...  EPIC_CLIENT_SECRET=...  go run ./cmd/server
```

Register `http://localhost:8080/accounts/epic/callback` as a redirect URI on the
Epic app, with the `openid` and `basic_profile` scopes enabled.

### Admin portal

The `admin` module serves the GameOps console at `http://localhost:8080/admin` —
a dark sidebar/header shell whose dashboard is composed from sections that modules
**contribute** (`accounts` contributes a live Players table + KPIs). A module shows
up by contributing an `adminapi.Section`; the admin owns the theme and never reads
another module's schema. The visual direction lives in `UILayout/` (a design spec).
Gate it with `ADMIN_USER`/`ADMIN_PASS` (HTTP Basic); unset = open + warning (local).

### Characters & inventory

A player has N **characters** (`/characters`), and **inventory** is owner-scoped —
a player's own inventory (e.g. IAP) or a character's. It shows off cross-module
relations under logical isolation: `inventory` references owners by id (no FK),
asks `characters` who owns a character (sync), and **reacts to character events**
— granting a starter item on create and wiping holdings on delete, so integrity
comes from an event rather than an FK cascade. `characters` never knows `inventory`
exists. Both appear in the admin portal automatically.

```
curl -X POST localhost:8080/characters -H "Authorization: Bearer <t>" -d '{"name":"Aria","class":"mage"}'
curl localhost:8080/inventory/character/<id> -H "Authorization: Bearer <t>"   # has the starter item
curl -X POST localhost:8080/inventory/me/grant -H "Authorization: Bearer <t>" -d '{"item_id":"coin","qty":100}'
```

## Dependency rules

- Implementations never import each other.
- Implementations may import `*events` contract packages and the core.
- `*events` packages depend on nothing.
- Synchronous dependency only "downward", toward foundations; sideways reactions
  go through the bus.
- Evolve events additively (new field / `FooV2`), never mutate a published shape.

## Layout

```
cmd/server/main.go            # the only place that lists all modules
core/                         # Module, Context, Registry, Bus — no game knowledge
modules/
  match/
    matchevents/              # published events of the match domain (pure data)
    match.go                  # impl: depends on "rating", emits match.finished
  rating/rating.go            # impl: provides the "rating" service, reacts to matches
  leaderboard/leaderboard.go  # impl: Postgres-backed listener, owns schema "leaderboard"
```

## Run

Needs a reachable Postgres. Connection comes from `DATABASE_URL`, falling back to
`postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable`.

```
go run ./cmd/server
```

```
curl -X POST localhost:8080/match/report -d '{"Winner":"alice","Loser":"bob"}'
curl localhost:8080/leaderboard
```

## Status

Baseline architecture for iterating on constraints before planning anything larger.
