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
mechanically by the Rust verification runner: `archcheck` and the `fortress`
stage enforce dependency and process boundaries, while route, event-topic,
external-client codegen, contract-golden, conformance, current-documentation, and
public-API checks protect the surfaces that cross those boundaries.

## Domain modules

11 fortresses plus the gateway:

- **accounts** — identity: one `player_id`, many identities, opaque DB sessions;
  dev/password auth, Epic OIDC verifier, Epic web OAuth link/login.
- **characters / inventory** — the modularity reference case: plain-id relations,
  synchronous ownership authz over the wire, starter-grant/wipe via durable events.
- **config** — DB-backed knobs with LISTEN/NOTIFY live reload; remote consumers get
  a snapshot-filled cache kept fresh by the `config_changed` invalidation channel.
  The durable `config.changed` event is a separate audit/consumer contract.
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
tools/          devctl/verifyctl/processctl/splitproof, architecture checkers,
                generators, edgeca, playercli
weles/          standalone mini-orchestrator (restart-on-crash supervisor)
experiments/    archived sketches (Go original, JVM explorations)
```

## Run locally

```sh
cargo run -p devctl -- up monolith
cargo run -p devctl -- up split
cargo run -p devctl -- status
cargo run -p devctl -- down
```

`devctl up` is a foreground supervisor. It builds the selected topology, creates
the development edge CA, seeds the development admin account, starts each process
with a typed environment, health-checks it, and keeps ownership until shutdown.
Bounded state/control metadata lives under `run/devctl/`, alongside ordinary owned
per-process logs.
`status` and `down` verify the recorded supervisor identity before talking to it;
shutdown reaps exactly the processes owned by that supervisor and does not kill by
process name or stale PID.

`cargo run -p devctl -- up ...` keeps its parent Cargo process active for the life
of the foreground supervisor. While it is running, do not start another Cargo
command. Inspect or stop that fleet with the already-built direct binary:
`target/debug/devctl.exe status` / `down` on Windows or
`target/debug/devctl status` / `down` on Unix (under the configured
`CARGO_TARGET_DIR` when it differs).

Rollouts run natively on Windows, Linux, and macOS. The macOS (Apple Silicon)
port landed 2026-07-17: `processctl` grew a kqueue/`posix_spawn` Darwin backend,
and `cargo run -p verifyctl -- --fast` passes all 16 blocking stages — including the
12-service split proof — on a Mac. Two containment backstops are structurally
weaker on Darwin (no `PR_SET_PDEATHSIG`/`PR_SET_CHILD_SUBREAPER` equivalents); the
per-OS command spellings and the exact trade-offs are in
[docs/reference/platform-notes.md](docs/reference/platform-notes.md).

### weles — the standalone supervisor

`devctl` is the dev harness; **`weles`** is a separate, zero-sharing
mini-orchestrator for supervising the same fleet in a more production-shaped way.
Its one differentiator over `devctl` is **per-service restart-on-crash with capped
backoff** — `devctl up` tears the whole fleet down on a failure, `weles` restarts
just the crashed process. It runs **native processes only** (no containers) and
**never builds**: it executes binaries you first stage into `deploy/`.

```sh
weles deploy target/debug     # stage built binaries into deploy/
weles up split                # or: weles up monolith  — supervise, restart-on-crash
weles status
weles down
```

It shares the same `run/rollout.lock` as `devctl`/`verifyctl`, so it can never run
a fleet concurrently with them. See [`weles/README.md`](weles/README.md) and the
[design doc](docs/reference/weles-design.md).

## Verify

```sh
cargo run -p verifyctl -- --fast
cargo run -p verifyctl -- --all
cargo run -p verifyctl -- --all --strict
cargo run -p verifyctl -- --slow
```

`--fast` is the default blocking safety net: build, clippy, tests, dependency
audit, fortress/architecture checks, route checks, external C# codegen freshness,
contract-golden wire inventory, convention conformance, `docs-current`, and the
live split proof. `--all`
adds the advisory public-API, fuzz, external C# client, and event-topic stages;
`--strict` includes those advisory stages and makes their failures blocking.
`--slow` runs blocking plus advisory stages and adds mutation testing; advisory
failures remain non-blocking unless `--strict` is also present.
The runner prints a PASS/FAIL/SKIP table and exits non-zero for every applicable
blocking failure. Audit invocation, installation, and network failures are FAIL,
not a green SKIP. Across the manifest, an explicit `--no-install` turns a missing
tool into a labeled SKIP; documented platform non-applicability may also skip.

All rollout-bearing tools share one lease at `run/rollout.lock`. A running
`devctl up` and a verification run therefore cannot overlap. `verifyctl` owns the
lease for the whole run and passes a private inherited lease to its split-proof
child, so that child participates in the same rollout instead of reacquiring or
bypassing the lock. Before an ad-hoc `cargo test`, also confirm that no `cargo` or
`rustc` process is already running. Never start a second compile/test rollout in
the background against the shared Postgres.

`split-proof` is the heart of the repo's promise: it boots every domain service as
a separate process (each with its HTTP port and, where applicable, an internal QUIC
edge), routes real
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
`cargo run -p verifyctl -- --fast` is the safety net, run locally before every
push.
