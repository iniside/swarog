# weles ↔ processctl fleet parity

weles is zero-sharing: its fleet manifest (`weles/src/manifest.rs`) is a
hand-copied port of `tools/processctl/src/fleet.rs` (the Development flavor),
not an import. The BLOCKING verifyctl stage `weles-fleet-parity`
(`tools/verifyctl/src/stages/weles_fleet_parity.rs`) machine-checks that copy
against the real processctl source of truth on every `--fast` run — per
service: name/pkg, http/edge/player ports, `has_db`, `pool_max`, the full
normalized composed env (peer `*_EDGE_ADDR`/`*_HTTP_ADDR`, `DATABASE_POOL_MAX_CONNECTIONS`,
dev-seeds, `TLS_MODE`, security CIDR — **with one deliberate exception for the
managed gateway, below**), and boot-order-vs-dependency-graph consistency. It is
pure in-memory (no DB, no rollout), so it is cheap and safe under `--fast` —
hence blocking.

> Nothing enforces this document: `docs_current.rs` does not reference it, so
> every claim below is unenforced prose. The enforced statements live in
> `weles_fleet_parity.rs`'s module doc and its tests; this file must be read as
> a guide to them, never as the authority.

## What is NOT compared (read this hostilely)

Two corrections to what this document claimed before M1 Step 7, because both
statements are now FALSE and a gate's "deliberately not covered" prose is the
least-audited thing in the file:

1. It is **not true that "everything topology-shaped is compared"** — a managed
   process's peer addresses are not (below).
2. It is **not true that this is weles's ONLY parity gate**. Three other stages
   constrain weles today: `weles-async-island` (tokio feature bans, checked with
   `cargo tree -e features` + a positive control), `weles-wire-contract` (the
   hand-copied `AddrKind`/`ErrorCode`/request shapes round-tripped through the
   production derives against `core/remote`'s twin), and — the one that matters
   here — `weles-managed-gateway`, which boots the real fleet under weles.

### Exclusion 1 — the ambient `SERVICE_ENV_ALLOWLIST` passthrough

PATH/HOME/…: weles reads them from real ambient env, processctl from an injected
snapshot, so their value is the operator's shell, never a topology decision. This
is the only key-only, service-blind exclusion. The two hand-copied allowlists are
themselves diffed, so a 12th key cannot silently widen it.

### Exclusion 2 — a MANAGED process's peer-address wiring

Since M1 Step 4, weles spawns `gateway-svc` managed (`weles::manifest::Addrs::Asks`):
it is handed `ORCHESTRATOR_URL` and asks the agent to resolve each of its eight
peer addresses. processctl still composes those eight keys at spawn, because
split-proof's standalone topology runs no agent. **That divergence is deliberate
and permanent** — the two tools are no longer supposed to agree here.

**What pays for it — two stages, one per arm.** The design's law is that every
service leaving this gate's assertion is paid for by a live proof (which is why
the proofs landed BEFORE this exclusion). This exclusion gives up ground on both
sides of the diff, and the two halves are paid for by **different** stages —
crediting both to one would be an unpaid departure with a receipt stapled to it:

| arm | ground given up | paid for by |
|---|---|---|
| weles-only | `ORCHESTRATOR_URL`; weles composing no addresses | `weles-managed-gateway` — boots `weles up split` and proves the managed gateway resolved an address from the agent and **used** it |
| processctl-only | the eight address keys | **split-proof** — boots the processctl fleet and drives ops through gateway-svc at :8082 |

`weles-managed-gateway` boots *weles's* fleet and never reads a processctl
`ServiceSpec`, so it is **structurally incapable** of detecting a defect in
`tools/processctl/src/fleet.rs`'s `gateway_env`. It cannot pay for the
processctl-only arm, which is the bulk of the ground given up.

**Why it cannot silently widen:**

- It is keyed on the def's `Addrs`, **never on the name `gateway-svc` and never
  on a key list**. A hardcoded set could not shrink when a service stops being
  managed; this one follows the data, and a permanently-widened green gate is
  worse than the red one it replaced.
- Only the two **asymmetric** directions are excluded, each only for a pair the
  delegation actually explains: `ORCHESTRATOR_URL` bearing exactly the agent's
  own URL, and a processctl-only key whose **own `(provider, kind)`**, read from
  the key's spelling, the agent's resolve map answers with **exactly** that
  address. So a peer address drifted to a port the agent does not serve still
  FAILs — and so does one pointing at *another service's real address*
  (`CHARACTERS_EDGE_ADDR` → inventory's edge), or at the right provider's *wrong
  kind* (`ACCOUNTS_EDGE_ADDR` → accounts' http port). That copy-paste class is
  likelier than the typo class, and an "is this any fleet address" check —
  this exclusion's first cut — went green on all of it.
- Everything else about the managed service is still compared in full: ports,
  `has_db`, `pool_max`, dedicated sessions, `TLS_MODE`, `PLAYER_EDGE_ADDR`,
  `PORT`, the CA material, and its boot-order position.

Seven tests pin the narrowness (`weles_fleet_parity_tests.rs`): drift on an
unmanaged service still fails *even when its value is a resolvable address*; a
non-address key on the managed service still fails *in both asymmetric
directions*; a key/value mispair among the fleet's own addresses still fails
(three cases, including the wrong-kind one); an unresolvable peer address still
fails; the exclusion **evaporates** when the def stops asking (driven through
`view_from_weles` from a real def, so what is tested is the derivation from
`Addrs`); `ORCHESTRATOR_URL` on an unmanaged service still fails; and a wrong
agent URL on the managed service still fails.

**Residual gap, recorded not smuggled:** if processctl **dropped** one of
gateway's eight keys, the key would be in neither map, enter no union, and
produce no diff at all — before Step 4 that was an `absent in processctl` FAIL.
Nothing in this gate can catch it: a managed def declares no peer keys, so the
gate no longer knows which keys to expect. split-proof is a weak net for it,
because `cmd/gateway-svc`'s `ADDR_SPECS` carry standalone defaults equal to the
real addresses — a dropped key would silently fall back and still work. Closing
it needs a declared expectation of gateway's key set (its `ADDR_SPECS`); that is
a separate step, not smuggled in here.

## The dev/prod seam (an M1 warning, not a today problem)

Both manifests fold three unrelated concerns into one untagged bag of env pairs
(`weles::manifest::ServiceDef::env_extra`; `processctl` `ServiceSpec::env`):

1. **Topology wiring** — `PORT`, `EDGE_ADDR`, peer `*_EDGE_ADDR`/`*_HTTP_ADDR`,
   `PLAYER_EDGE_ADDR` (structural, identical in any deployment flavor).
2. **Dev-mode seeds / opt-ins** — `ACCOUNTS_DEV_AUTH`, `APIKEYS_DEV_SEED`,
   `INVENTORY_DEV_GRANT`, `ADMIN_COOKIE_SECURE=0` (development-only; a real
   deployment must NOT ship these).
3. **A security knob** — `TRUSTED_PROXY_CIDRS` (a production concern living in
   the same bag as the dev seeds).

processctl bolts a production-ish variant on top with a post-hoc
`if flavor == FleetFlavor::Proof { … }` overlay (`tools/processctl/src/fleet.rs`),
mutating the already-composed dev env in place. When weles grows an M1 prod
flavor it must NOT copy that pattern: a post-hoc mutation of a dev baseline is
how a forgotten dev seed leaks into production. Instead, structurally separate
the three concerns — the wiring belongs to the topology, the seeds belong to a
development-only overlay that a prod flavor never applies, and the security knob
is its own deliberate input — so a prod flavor is built by OMITTING the dev
overlay, not by patching it away afterward. No prod flavor exists today and this
note deliberately does NOT add one (that would smuggle in an unbuilt seam); it
records the constraint for whoever does.

Evidence base: [weles pre-M1 backlog research](../status/2026-07-15-1815-weles-pre-m1-backlog-research-status.md).
