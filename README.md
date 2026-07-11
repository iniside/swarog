# GameBackend

A for-fun game backend in **Rust** (Cargo workspace), built as a **modular monolith
with a proven split**: one repo, one `cmd/server` binary running everything — and
every domain module *also* compiles and boots as its own `cmd/<name>-svc` process.
Both topologies are first-class, continuously proven by a live 12-process
integration suite.

The design goal is **Open/Closed at the architecture level**: features are added by
writing new code, not by modifying existing code. The second goal follows from the
first: any module is extractable into a microservice at any time, because it already
runs as one.

> The project started as a Go modular monolith (archived at
> `experiments/go-sketch/`), was explored on the JVM
> (`experiments/jvm-kotlin-sketch/`, `experiments/jvm-quarkus-sketch/`), and was
> fully ported to Rust in July 2026. The Rust workspace at the repo root is the
> living codebase; `experiments/` is reference only.

## Core idea: three seams

Three seams — and almost nothing else — carry all extensibility:

### 1. Module registry (`core/lifecycle`)

Every feature is a `lifecycle::Module` that self-registers in a `cmd/*` main.
Foundations never import a module. Dependencies are a *manifest* (`requires()`),
not a topological sort: the two-phase build (every provider's `register` runs
before any module's `init`) makes init order commutative. A missing required
capability in a process's module set fails loudly at startup.

Lifecycle phases: `register` (provide services, no I/O) → `init` (wiring only —
contribute slots, subscribe, mount routes) → `migrate` (own schema only) →
`start` (background work, first I/O) → `stop` (reverse order).

### 2. Service registry (`registry::provide` / `require`)

For *synchronous* needs — "ask B now, get an answer". The consumer imports the
provider's **contract crate** (a pure trait, never the impl crate) and `require`s
`dyn Trait`. In a split process, a `remote::Stub` provides a QUIC-backed client
under the *same registry key* — the registry swap is the only difference between
running as a monolith and running split.

### 3. Event bus (`ctx.bus()`)

The async glue. Every cross-module event is **durable**: the producer appends into
a shared XID-ordered log inside its own DB transaction, and the consumer pulls
from its own checkpointed subscription. Delivery is at-least-once per subscription
with a stable `event_id`; effects are exactly-once when the handler effect and the
checkpoint advance share a transaction. The bus is
fire-and-forget by design — no request/response through it (that's the registry's
job), so state projected from events is eventually consistent. Event payloads
evolve additively only, enforced mechanically.

The durable transport is owned by the app runtime (`core/asyncevents`), not by any
module: a process with a DB hosts the plane, a process without one doesn't, and
modules never know or care.

Two minor seams round it out: **contribution slots** (`ctx.contribute`) — a
multi-value registry for cross-cutting collections (admin pages, typed HTTP ops,
readiness checks) — and **`edge::EDGE_SLOT`**, where a module unconditionally
contributes its QUIC endpoint registrations and the runtime applies them only if
the process actually serves an internal edge.

## The fortress rule

Every folder in `modules/` is a *fortress*: it never imports another module's impl
crate, and it must compile and boot as its own `cmd/<name>-svc`. The only gates
between fortresses are the contract crates under `api/<name>/`:

| crate | contents | who may import it |
|---|---|---|
| `<name>api` | pure traits + op metadata, transport-free | any module |
| `<name>events` | event payloads + descriptors | any module |
| `<name>rpc` | generated transport glue | only its own module and `cmd/*` roots |

**Modules are topology-blind.** No `Option<transport>`, no `if split`, no env
branches in domain code. `cmd/*` mains are the only topology-aware code and differ
only in their module list and which QUIC planes they serve. All of this is enforced
mechanically — `archcheck` (dependency law), `topiccheck` (event topic drift), a
`public-api` diff (additive-only contracts), and a `fortress` build stage (every
svc must compile).

## Domain modules

11 fortresses plus the gateway:

- **accounts** — identity: one `player_id`, many identities, opaque DB sessions;
  dev/password auth, Epic OIDC verifier, Epic web OAuth link/login.
- **characters / inventory** — the modularity reference case: plain-id relations,
  synchronous ownership authz over the wire, starter-grant/wipe via durable events.
- **config** — DB-backed knobs with LISTEN/NOTIFY live reload; remote consumers get
  a snapshot-filled cache invalidated by `config.changed`.
- **admin** — GameOps portal at `/admin`; renders contributed admin pages, remote
  pages fan out over QUIC.
- **audit** — append-only ledger; zero-coupling raw event sinks; scheduled pruning.
- **scheduler** — data-driven schedules, per-name advisory locks, exactly-once
  firing via `UPDATE` + emit in one transaction.
- **match / rating / leaderboard** — match reports → durable `match.finished` →
  a persistent MMR projection (restarts preserve MMR) + persistent leaderboard projection.
- **apikeys** — per-key API access policy (anon/service-key model); the gateway
  requires an `X-Api-Key` on every op and enforces the key's policy.
- **gateway** — the single public front door: HTTP op routing (local vs remote
  purely by slot presence), authenticated player-QUIC plane, passthroughs, rate
  limiting. Domain services never host it; they serve ops only over the internal
  mTLS edge.
- **webui** (`demos/`) — dev demo SPA; the sanctioned monolith-only exception.

## Persistence

One shared Postgres, **full logical isolation**: schema per module, no
cross-module foreign keys. A relation to another module is a plain id column,
resolved via a capability call or kept in sync via durable events. This is what
keeps every module independently extractable.

## Layout

```
cmd/            composition roots — the ONLY topology-aware code
  server/         monolith (all modules local)
  gateway-svc/    pure-transport front door (stubs only, no DB)
  <name>-svc/     one process per domain module
core/           foundations — never import modules or api/ crates
  app/ bus/ registry/ contrib/ lifecycle/ opsapi/
  edge/           internal mTLS QUIC + player plane
  asyncevents/    the durable event plane
  remote/ metrics/ httpmw/
api/<name>/     contract surface per domain (api / events / rpc)
modules/        private impls — the fortresses
demos/          dev demo SPA (monolith-only)
tools/          rpc-macro, archcheck, topiccheck, edgeca, playercli
experiments/    archived sketches (Go original, JVM explorations)
```

## Building and verifying

```sh
cargo build --workspace
cargo test --workspace          # unit + live-Postgres integration + proptests
cargo run -p archcheck          # fortress dependency law
cargo run -p topiccheck         # event topic drift
./verify.sh                     # the tiered safety net (build, clippy, test,
                                # audit, fortress, split-proof; --all / --slow)
./split-proof.sh                # boots the real 12-process split + gateway,
                                # asserts named end-to-end scenarios, then
                                # re-runs the monolith for parity
./run.sh                        # mint a dev CA + boot the split locally
```

`split-proof` is the heart of the repo's promise: it boots every domain service as
a separate process (each with its HTTP port and internal QUIC edge), routes real
traffic through the gateway — registration, login, authz negatives, cross-process
flows, live config reload, scheduler exactly-once, API-key policy — and then runs
the same scenarios against the monolith. **A feature that only works in one
topology is a bug.**

Postgres is expected locally (`DATABASE_URL`, defaults to
`postgres://gamebackend:gamebackend@localhost:5432/gamebackend`). Integration
tests hit it directly — no containers.

## Status

A hobby project and an architecture playground — the point is the seams, the
mechanical enforcement, and the split proof, not production readiness. No CI:
`./verify.sh` *is* the safety net, run locally before every push.
