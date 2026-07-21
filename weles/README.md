# weles

The standalone mini-orchestrator (binary: `weles`) for the GameBackend fleet.
It supervises native OS processes — no containers, no Docker, no Kubernetes.

> **Design source of truth:** [`docs/reference/weles-design.md`](../docs/reference/weles-design.md).
> Read it before proposing any change to Weles's shape — it records the decided
> design *and* what has already been rejected (and why). This README is the short
> operational summary; that file is the contract.

## What it is

`weles` runs the same split/monolith fleet that `devctl` runs, but with a
different job: **it is a supervisor, not a dev harness.** Its one differentiator
over `devctl` is **per-service restart-on-crash with capped backoff** — where
`devctl up` tears the whole fleet down on a failure, `weles` restarts the
individual crashed process and keeps the rest serving.

Three rules define it:

- **Native processes only.** Deploy = copy binary + supervise. Cross-platform on
  Windows, Linux, and macOS: process groups on Unix, Job Objects on Windows, a
  kqueue supervisor on Darwin. Graceful stop is a **wire drain command**, not
  `SIGTERM` (which doesn't exist on Windows); signals are only the fallback for a
  process that stopped answering.
- **Zero-sharing.** `weles` never imports a workspace crate (`core/*`, `api/*`,
  `modules/*`, `tools/*`) — patterns are copied from `devctl`/`processctl`, never
  imported. The only coupling to the shipping graph is a wire-only JSON contract
  with its own types on each side. (`tools/verifyctl` *may* dev-dep `weles` to
  test it — the reverse arrow the shipping graph never takes.)
- **It never builds.** No `cargo` invocation, ever. `weles` executes artifacts
  that were staged into `<root>/deploy/` by `weles deploy <src-dir>`.

## Commands

```sh
weles deploy target/debug --fleet <fleet.toml>   # stage already-built binaries into deploy/, stamping
                                                  # the chosen fleet def (weles never builds)
weles up                      # boot whatever fleet was deployed — no split/monolith argument
weles up --dry-run            # validate the deployed fleet.toml; no rollout lock, no prepare, no spawn
weles status                  # query the running supervisor
weles down                    # stop the fleet it owns
```

`weles` has no concept of monolith/split — it supervises *a fleet*, and monolith
vs split is just a fleet of one process vs twelve. The fleet definition is a
hand-authored, strict `fleet.toml` (`#[serde(deny_unknown_fields)]`, no
layering, no templating): per-service ports/peers plus fleet-level `[[prepare]]`
hooks (opaque commands run once before the fleet boots — e.g. minting the edge
CA with `edgeca`, seeding the admin account with `adminctl`). `deploy --fleet`
stamps that file into the generation; `up` reads it back from `<root>/deploy`.

Typical flow: build with Cargo, `weles deploy target/debug --fleet weles/fleet.split.toml`,
then `weles up`.

## Rollout safety

`weles` participates in the canonical `run/rollout.lock` **bit-compatibly** with
`devctl`/`verifyctl` — the three can never run fleets concurrently against the one
shared local Postgres (see the one-rollout-at-a-time rule in
[`CLAUDE.md`](../CLAUDE.md)). Its runtime state — `state.json`, per-service logs,
and the operator control endpoint — lives under `run/weles/`. The operator control
path authenticates a **local OS caller** (owner DACL on Windows, UDS peer-cred on
Unix incl. macOS `LOCAL_PEERCRED`), a separate trust domain from the service edge.

## Status

M0 shipped (2026-07-15): supervisor, restart-on-crash, `deploy/` generations,
control endpoint, `rollout.lock` bit-compat; pre-M1 hardening closed (2026-07-16).
M1 (rollback, hello+resolve managed mode, SQLite state, port minting) is not
started. The managed-vs-standalone boot modes and the four-point managed
convention are specified in the design doc.
