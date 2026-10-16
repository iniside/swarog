# Swarog

Swarog is a game backend written in Rust. It is built as a **modular monolith with
a proven split**: the whole backend runs as a single `cmd/server` process, and
every domain module also compiles and boots as its own `cmd/<name>-svc` process.
Both topologies run the same domain code and are exercised end to end by a live
integration suite on every verification run.

The architecture follows the **Open/Closed principle at the level of the system**:
features are added by writing new modules, not by modifying existing ones. Because
every module already runs as a separate process, any module can be deployed
independently without a rewrite.

## Highlights

- 18 domain modules plus a gateway, each a "fortress" that never imports another
  module's implementation and boots as its own service.
- Three extensibility seams: a module registry, a synchronous capability registry
  that swaps local implementations for QUIC-backed remote clients, and a durable
  event bus over Postgres.
- Durable cross-module events: at-least-once delivery with a stable event id and
  exactly-once effects, with the handler effect and the subscription checkpoint
  committed in one transaction.
- A single public front door (HTTP, a WebSocket push hub, and a QUIC player
  plane), with an internal mTLS QUIC edge between services.
- Architecture rules enforced by tooling rather than convention: dependency
  checks, per-process boot checks, public-API and wire-contract snapshots, and a
  split-versus-monolith parity proof.
- Native processes only. Development, verification, and supervision tooling run
  on Linux, macOS, and Windows without containers.

## Architecture

### Three seams

**Module registry (`core/lifecycle`).** Every feature is a `lifecycle::Module`
registered in a `cmd/*` composition root. Dependencies are declared as a manifest
(`requires()`) rather than sorted topologically: a two-phase build runs every
provider's `register` before any module's `init`, so initialisation order does not
matter. A process whose module set lacks a required capability fails at startup.
Lifecycle phases are `register` → `init` → `migrate` → `start` → `stop`; `register`
and `init` perform no I/O, and `stop` runs in reverse registration order.

**Service registry (`registry::provide` / `require`).** Synchronous calls go
through capability traits. A consumer depends on the provider's contract crate,
never its implementation, and resolves `dyn Trait` under a registry key. In a
split deployment a `remote::Stub` provides a QUIC-backed client under the same
key; that registry swap is the only difference between the two topologies.
Generated RPC operations are not retried unless the contract marks a method
`#[retry_safe]`.

**Event bus (`ctx.bus()`).** Every cross-module event is durable. A producer
appends to a shared, XID-ordered Postgres log inside its own database transaction,
without knowing its consumers. Each consumer owns a versioned pull subscription
with an explicit start position. Ordering is per subscription; a failing event
backs off and pauses its one subscription and is never skipped automatically
(operators inspect and resume it with `eventctl`). The bus is fire-and-forget:
request/response goes through the service registry, and state projected from
events is eventually consistent. The durable plane is owned by the application
runtime (`core/asyncevents`), not by any module. Replica-local caches are
refreshed by a separate LISTEN/NOTIFY invalidation plane (`core/invalidation`).

Two minor seams complete the picture: **contribution slots**
(`ctx.contribute` / `ctx.contributions`), a multi-value registry for cross-cutting
collections such as admin pages, HTTP operations, and readiness checks; and
**`edge::EDGE_SLOT`**, where a module unconditionally contributes its internal-edge
handlers and the runtime applies them only when the process serves an internal
edge.

### Fortresses and contract crates

Every directory under `modules/` is a fortress: it never imports another module's
implementation crate, and it compiles and boots as its own `cmd/<name>-svc`. The
only dependencies between fortresses are the contract crates under `api/<name>/`:

| Crate | Contents | May be imported by |
|---|---|---|
| `<name>api` | capability traits and operation metadata, transport-free | any module |
| `<name>events` | event payloads and `bus::define` descriptors | any module |
| `<name>rpc` | generated transport glue | its own module, `cmd/*` roots, other `api/*/rpc` crates |

Foundations in `core/` never depend on a module or a contract crate.

**Modules are topology-blind.** Domain code contains no optional transports, no
"if split" branches, and no topology environment switches. The `cmd/*` roots are
the only topology-aware code and differ only in their module list and in which
QUIC planes they serve.

### Persistence

All modules share one PostgreSQL database with full logical isolation: one schema
per module and no cross-module foreign keys. A reference to another module's data
is a plain id column, resolved through a capability call or kept in sync through
durable events.

## Domain modules

- **accounts** — player identity: one player id, many provider identities; opaque
  sessions with short-lived access tokens and rotating refresh-token families;
  federated login (Epic, Google, guest), account linking, and self-service account
  deletion that downstream modules purge on.
- **characters / inventory** — the reference pair for new modules: plain-id
  relations, synchronous ownership checks over the wire, and starter grants and
  wipes driven by durable character events.
- **config** — database-backed settings with a monotonic revision, live reload
  through the invalidation plane, and a durable `config.changed` event.
- **admin** — the GameOps portal at `/admin`: session authentication with CSRF
  protection and login lockout, rendering admin pages contributed by other
  modules, including remote pages fetched over QUIC.
- **audit** — an append-only ledger fed by independent raw durable subscriptions,
  with scheduled retention.
- **scheduler** — data-driven schedules that fire exactly once per due time under
  per-schedule advisory locks.
- **match / rating / leaderboard** — idempotent match reports, a persistent MMR
  projection, and a persistent leaderboard, connected by the durable
  `match.finished` event.
- **apikeys** — per-key API access policy; the gateway requires an API key on
  every operation and enforces its role's policy.
- **wallet** — virtual currency: an operator-owned catalog, per-player balances,
  and an append-only ledger; credits and debits are internal-only and idempotent.
- **notifications** — a per-player inbox built from other modules' durable events,
  with operator messages and scheduled pruning.
- **mail** — outbound email through a durable outbox and a retrying worker, with a
  development log sink and an SMTP provider.
- **friends** — the social graph: friend requests by player handle, with symmetry
  and pair uniqueness enforced by the database schema.
- **groups** — social groups with roles and invitation/request membership states.
- **storage** — player cloud saves with owner-only writes, public or private
  reads, and compare-and-swap versioning.
- **party** — short-lived player parties with lease-based membership and real-time
  state pushed to members.
- **gateway** — the single public front door: HTTP operation routing (local or
  remote, decided by slot presence), a WebSocket push hub, the player QUIC plane,
  HTTP passthroughs, rate limiting, and native TLS termination. Domain services
  never host it; they serve operations only over the internal mTLS edge.

`demos/webui` is a development demo SPA for the accounts flow. It is not shipped
and is registered only in the monolith.

Current gaps and planned work per module are tracked in
[docs/roadmap/feature-tracker.md](docs/roadmap/feature-tracker.md).

## Design principles

- **Extend by addition.** New capability is new code: a new module, a new
  contract crate, a new subscription. Existing modules are not edited to
  accommodate it.
- **Every module is extractable.** Any domain module runs as its own process today.
  A feature that works in only one topology is treated as a defect.
- **One shared Postgres.** A single database server with schema-per-module
  isolation is a deliberate operational choice, not an interim step towards
  database-per-service.
- **Durable, transactional events.** Delivery is at-least-once; effects are
  exactly-once because the consumer's effect and its checkpoint share one
  transaction. There is no inbox table and no deduplication layer.
- **Additive contract evolution.** A published event payload is never changed in
  place. A breaking change is a new contract version with new subscription ids.
- **Fail closed.** Development conveniences (dev authentication, seeded API keys,
  seeded currencies, simulated purchases) are off unless explicitly enabled, and a
  process missing a required capability refuses to start.
- **Mechanical enforcement over convention.** Each architectural rule above is
  checked by a verification stage; see
  [docs/reference/architecture-enforcement.md](docs/reference/architecture-enforcement.md).

## Getting started

Prerequisites:

- The Rust toolchain pinned in `rust-toolchain.toml` (stable, installed by
  `rustup` on first use).
- A local PostgreSQL reachable at `DATABASE_URL`, defaulting to
  `postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable`.
  `scripts/db-provision.sh` (or `scripts/db-provision.ps1`) creates the role and
  database. Module schemas are created by the services at boot. Integration tests
  use this database directly; there are no containers.

Run either topology under the development supervisor:

```sh
cargo run -p devctl -- up monolith
cargo run -p devctl -- up split
cargo run -p devctl -- status
cargo run -p devctl -- down
```

`devctl up` builds the selected topology, mints the development edge CA, seeds the
development admin account (`admin` / `admin`), enables the development
conveniences, starts each process with a typed environment, health-checks it, and
keeps ownership in the foreground until shutdown. While it runs, inspect or stop
the fleet with the already-built `target/debug/devctl` binary instead of starting
another Cargo command.

Smoke test against the monolith:

```sh
curl -X POST localhost:8080/match/report -H "X-Api-Key: dev-key-server" \
  -d '{"ReportId":"demo-1","Winner":"alice","Loser":"bob"}'
curl localhost:8080/leaderboard -H "X-Api-Key: dev-key-client"
```

Per-platform details (Linux, macOS, Windows) are in
[docs/reference/platform-notes.md](docs/reference/platform-notes.md).

## Verification

```sh
cargo run -p verifyctl -- --fast
cargo run -p verifyctl -- --all
cargo run -p verifyctl -- --all --strict
cargo run -p verifyctl -- --slow
```

`verifyctl` runs a manifest of stages, prints a PASS/FAIL/SKIP table, and exits
non-zero on any applicable blocking failure.

- **Blocking** (`--fast`, the default): build, clippy with warnings denied, tests,
  dependency audit, `fortress`, `routecheck`, `codegen-freshness`,
  `contract-golden`, `conformance`, `docs-current`, `supported-targets`, three
  `weles` contract and boot stages, and `split-proof`.
- **Advisory** (`--all`; blocking with `--strict`): public-API snapshots, fuzzing,
  the external C# client, event-topic checks (`topiccheck`), and admin
  extension-point checks (`admincheck`).
- **Slow** (`--slow`): everything above plus mutation testing.

An audit installation, invocation, or network failure is a FAIL, not a green
SKIP. A stage is skipped only for an explicit `--no-install` with a missing tool or
a declared platform non-applicability.

**`split-proof`** is the central promise of the repository. It boots every domain
service and the gateway as separate processes, drives real traffic through the
gateway (registration and login, authorisation negatives, cross-process event
flows, live config reload, scheduler exactly-once, API-key policy, the social and
storage modules, parties over the push socket, account-deletion purge) and
asserts database state directly. It then boots the monolith on the same player
front for parity checks and proves its graceful shutdown.

All rollout-bearing tools (`devctl`, `verifyctl`, `weles`) share one lease at
`run/rollout.lock`, so only one fleet or verification run can use the shared
Postgres at a time.

## Operations tooling

- **`weles`** — a standalone supervisor for native processes with per-service
  restart-on-crash and capped backoff. It never builds; it runs binaries staged
  with `weles deploy` against a strict `fleet.toml`. See
  [weles/README.md](weles/README.md) and
  [docs/reference/weles-design.md](docs/reference/weles-design.md).
- **`eventctl`** (`cargo run -p eventctl`) — operator CLI for durable
  subscriptions: list, lag, retry, pause, resume, skip, and retire. See
  [docs/reference/event-plane-ops.md](docs/reference/event-plane-ops.md).
- **`adminctl`** (`cargo run -p adminctl`) — creates admin portal users or resets
  their passwords, reading the password from stdin, never from argv; wrapped by
  `install.sh` / `install.ps1`.

## Repository layout

```
cmd/            composition roots, the only topology-aware code
  server/         monolith (all modules in one process)
  gateway-svc/    public front door (remote stubs only, no database)
  <name>-svc/     one process per domain module
core/           foundations; never import modules or api/ crates
api/<name>/     contract crates per domain (api / events / rpc)
modules/        domain module implementations (the fortresses)
demos/          development demo SPA (monolith only)
weles/          standalone process supervisor
tools/          devctl, verifyctl, processctl, splitproof, architecture checkers,
                code generators, operator CLIs, test support
clients/        external C# client (generated bindings)
docs/           reference documentation, plans, reviews, roadmap
experiments/    archived earlier implementations (Go, JVM), reference only
```

The project began as a Go modular monolith, was explored on the JVM, and was
ported to Rust in July 2026. The earlier implementations remain under
`experiments/` as reference and are not developed further.

## Development with AI agents

Swarog is developed by one human maintainer directing AI coding agents. Most of
the code, tests, and documentation are written by agents running in Claude Code;
runtime adapters under `.agents/adapters/` also cover Codex, Cursor, and Grok. The
maintainer owns the architecture, approves plans, and decides what lands. AI is
used only to build the project: the backend itself contains no language-model
integration and makes no calls to AI services at runtime.

The agents follow written instructions kept in the repository:
[CLAUDE.md](CLAUDE.md) and [AGENTS.md](AGENTS.md), with shared rules in
[.agents/shared/](.agents/shared/). Work proceeds in a fixed sequence:

1. **Plan.** Non-trivial changes start as a dated plan in `docs/plans/`, written
   from research into the existing code and reviewed by an independent agent. The
   plan is an ordered list of steps, each tagged with the model that will
   execute it.
2. **Implement.** Each step is dispatched to a specialised agent defined in
   [.claude/agents/](.claude/agents/): `core-implementer` for correctness-critical
   and cross-seam work, `mockup-implementer` for admin UI built against design
   mockups, and lower-cost models for mechanical changes.
3. **Test.** Tests are a separate step after the implementation lands, written by
   `test-author` and required to exercise the branch that was previously wrong,
   on the topology at risk.
4. **Review.** One adversarial review by `core-reviewer`, in a separate context,
   that defaults to rejection and attacks the diff by failure class.
   `proof-auditor` is added when the change touches a verification gate.
5. **Document.** `docs-writer` corrects prose against the landed code, deleting
   claims the code does not support.

The failure classes agents are reviewed against are recorded in
[docs/reference/core-failure-taxonomy.md](docs/reference/core-failure-taxonomy.md),
derived from the fix commits of the project's own remediation history. Agent working memory is
mirrored into `memory/` so it is versioned with the code.

The central design decision is that agents are not trusted to follow the
architecture; the build enforces it. Dependency direction, the fortress rule,
per-process boot, wire contracts, public API surfaces, documentation references,
and split-versus-monolith parity are all checked by `verifyctl` stages, so an
agent's mistake surfaces as a failed gate rather than relying on a reviewer to
notice it.

## Project status

Swarog is pre-production and has no deployed users. Consequences of that phase:

- There is no hosted CI. `cargo run -p verifyctl -- --fast`, run locally, is the
  gate before every push.
- Schema and event-contract breaks are handled by dropping the affected schemas
  and booting fresh. There is no data-migration machinery; development data comes
  from seed scripts.
- The external C# client stage cannot run on macOS, and fuzzing cannot run on
  Windows; both are declared platform exceptions in `verifyctl`.

## Licence

Copyright © Łukasz Baran.

Licensed under the European Union Public Licence v. 1.2 (EUPL-1.2). See
[LICENSE](LICENSE).
