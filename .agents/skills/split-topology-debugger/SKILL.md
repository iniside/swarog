---
name: split-topology-debugger
description: Diagnose "works in the monolith, fails/hangs/404s in the split" problems in this repo — registry key mismatches, missing stubs, edge registration, mTLS certs, PEER_SLOT wiring, gateway routing, event delivery across processes. Use whenever split-proof fails, a *-svc process fails startup, a request 404s/401s only through gateway-svc, an event consumer never fires cross-process, or the user says "w splicie nie działa", "split-proof failed", "działa tylko w monolicie". Diagnose by tracing — do not fix blind.
---

# Split Topology Debugger

The monolith and split run IDENTICAL module code; the only differences are the
`cmd/*` module lists, registry stubs, and which QUIC planes a process serves.
So a split-only failure is almost always a WIRING gap in that thin layer.
**Build the trace first, then fix** — name the failing hop before touching code.

## First: classify the failure

**Sync call fails** (op 404/panic/timeout) → trace the request path (§A).
**Event never consumed cross-process** → trace the delivery path (§B).
**Process fails startup** → §C. **TLS/connect errors** → §D.

## §A — Sync request trace (client → gateway-svc → domain svc)

Walk the hops in order; the failure is at the first broken one:

1. **Gateway route exists?** HTTP ops route Local vs Remote purely by slot
   presence. If gateway-svc doesn't know the op: is the op contributed to the
   `opsapi` binding slots, and does gateway-svc have a `remote::Stub` for that
   capability contributing to `opsapi::PEER_SLOT`? (Peer addresses come from
   `cmd/gateway-svc` env → `ProcessWiring`; the gateway module never reads env.)
2. **API key / auth layer?** 401 = missing/invalid `X-Api-Key` (or `api_key`
   envelope field on player-QUIC); 403 = key policy denies the wire method.
   Split-proof uses `dev-key-client`/`dev-key-server` (require
   `APIKEYS_DEV_SEED=1`). Session verifier needs the accounts capability (or
   `ACCOUNTS_DEV_AUTH=1`).
3. **Registry key match?** Consumer `require`s
   `registry::key(provider, snake_trait)`; the stub must provide under the SAME
   key. A key mismatch = works in monolith (local provider registered under the
   right key) + "capability missing" in split.
4. **Stub present in THAT process?** The stub list is per-`cmd/*` lib. A
   consumer module hosted in svc X needs the stub registered in svc X's
   `modules()` — check the actual lib.rs, not your memory.
5. **Edge registration on the provider?** The provider module must contribute
   its `edge::EdgeReg` to `EDGE_SLOT` in `init` (unconditionally); `app::run`
   applies it iff the process serves the internal edge. No registration = the
   svc listens but the method dispatches nowhere.
6. **Right port?** Cross-check the address gateway-svc was given against the
   svc's edge port. The authoritative typed fleet lives in
   `tools/processctl/src/fleet.rs`; `tools/splitproof` fails its fleet-drift
   preflight when that set disagrees with `cmd/*-svc`. Convention:
   characters :8080/:9000 … apikeys :8091/:9009, gateway HTTP :8082 +
   player-QUIC :9100 — trust the fleet source over this summary.

## §B — Durable event trace (producer tx → shared log → consumer checkpoint)

There is NO per-process routing config — producer and consumer share the
Postgres log. So cross-process event bugs are one of:

1. **Plain `emit` instead of `emit_tx`** — in-process only; fires in monolith,
   silently nothing in split. The #1 cause. Check the producer.
2. **No subscription hosted in any running process** — the consumer module (or
   its subscription) isn't in the process set. `cargo run -p topiccheck`
   validates per deployment profile; also check the consumer svc actually
   hosts the subscribing module.
3. **Subscription paused (poison event)** — a failing handler backs off and
   pauses ITS subscription (never auto-skipped). Inspect with the
   already-built operator binary — `target/debug/eventctl.exe -- list`
   (`target/debug/eventctl` on Unix; adjust for `CARGO_TARGET_DIR`), NOT
   `cargo run -p eventctl`: when you're diagnosing a live split the foreground
   `devctl up` fleet already owns Cargo, and a second Cargo command violates
   "one test rollout at a time" (same rule as `devctl status/down`). Read
   lag/retry/pause state, then retry/skip/resume deliberately.
4. **Checkpoint position** — a new subscription with `StartPosition::End`
   won't see events emitted before it first ran. Check the spec's `start` and
   the checkpoint row.
5. **No DB ⇒ no plane** — a DB-less process hosts no event plane; the
   subscribing module must live in a DB-backed process.

Evidence over inference: query `asyncevents` state via eventctl / psql —
did the event row get appended? Did the checkpoint advance?

## §C — Startup failures

- `app::validate_requires` fail = a declared capability has no provider AND no
  stub in that process's module set → add the stub or fix `requires()`.
- Gateway process without accounts capability fails unless
  `ACCOUNTS_DEV_AUTH=1`; without apikeys capability unless `APIKEYS_DEV_ALLOW=1`;
  these are deliberate fail-closed gates — set the env or host the capability,
  don't weaken the gate. Admin no longer accepts `ADMIN_USER`/`ADMIN_PASS`: seed
  a user through `adminctl`; zero-user boot warns, while `ADMIN_OPEN=1` is the
  explicit local bypass.
- Invalidation plane: startup fails if a registered callback's first refresh
  fails (e.g. `CachedConfig` boot-fill) — check DB reachability from that
  process, not the callback.

## §D — mTLS / connect

`devctl up` and the split-proof harness mint the dev CA via `tools/edgeca`.
Cert errors after adding a process usually mean it was not given the CA paths
its peers expect. Compare its typed environment and peer wiring with a working
service in `tools/processctl/src/fleet.rs`.

## Output

Report the trace: each hop checked, evidence (command output, file:line of the
wiring), and the FIRST broken hop. Fix that hop; then re-verify on the split, not
just the monolith. verifyctl has no scenario/stage filter — it runs one whole
manifest (`--fast|--all|--slow`), and the blocking split-proof stage is part of
it — so the terminal check is a full `cargo run -p verifyctl -- --fast` (respect
the safe-verification protocol), which reruns every named split-proof assertion.
