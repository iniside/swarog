# Weles: close the M1 gaps + land replicas, round-robin, gateway routing-as-data

**Date:** 2026-07-21 15:40 · **Scope decided with user:** do everything we don't have (M1
gaps) AND the path to `replicas` + client-side round-robin + gateway routing-as-data. Phased
single document (user choice). Based on an 11-subagent whole-repo survey (rg-anchored + targeted
end-to-end reads + macro-source reads; no LSP — small non-macro crates).

---

## The reframing the research forced (read this first)

Three of the four "big" targets are **much closer than the design's prose suggests**, and two
design statements are **stale and must be corrected in the same rollout** so nobody builds
machinery the architecture already deleted:

1. **Replica-safety is nearly free.** The durable event plane is ALREADY a consumer group by
   construction — `core/asyncevents/src/worker.rs:199-215` claims each subscription row with
   `FOR UPDATE SKIP LOCKED`, so two replicas sharing one subscription id deliver each event
   exactly once, cursor never raced. `rating` MMR is already DB-backed
   (`modules/rating/src/lib.rs:71-82`, upsert in the delivery tx — the design's precondition is
   already met). `scheduler` is already advisory-locked per fire. `config`/invalidation is
   fresh-per-replica by design. **Two domain modules break under `replicas: 2`, both the same
   class — a request-spanning single-redemption in-memory token store:** (i) `accounts` Epic
   web-OAuth (`modules/accounts/src/epic_oauth.rs:53`, `states: Mutex<HashMap<..>>` — the OAuth
   callback lands on the wrong replica → login fails); (ii) `admin` show-once reveal
   (`modules/admin/src/lib.rs:184,496`, `reveals: Arc<Mutex<RevealStore>>` — `stash_reveal` on the
   POST `:1385`, `take_reveal` on a LATER GET `:1156` → the show-once secret, an API-key plaintext
   never re-derivable, is silently lost when the GET LB-routes to the other replica). The
   in-line doc justifying admin's in-memory store (`:179-181`) reasons about monolith-vs-split,
   NOT replicas — it becomes false the moment admin has two instances. (Reviewer finding 1 — the
   original survey's "only accounts" claim was incomplete.) Plus two SOFT (non-correctness)
   rate-limit dilutions (admin/gateway per-process limiters multiply ×N).
   → **STALE DESIGN CLAIM #1:** `weles-design.md:707-709` ("the relay needs an advisory lock per
   `EVENTS_ORIGIN`") describes a **superseded relay/push architecture that no longer exists**
   (`core/asyncevents/src/transport.rs:5`: "no outbox, no relay, no per-process origin"). `grep
   EVENTS_ORIGIN` hits ONLY the design doc. This must be corrected as a dated errata.

2. **`resolve`-returns-a-list is already built.** `weles::manifest::PeerAddrs::from_fleet`
   (`manifest.rs:327-379`) already returns ALL instances per `(provider, kind)`, not a
   1-per-provider map (its own doc comment `:341-345` calls out the deliberate no-first-match
   design); the wire already carries `Vec<String>` (`core/remote/src/resolve.rs:232,263`).
   Single-host replicas need **loopback + distinct ports only** — NO mTLS work, NO DNS work
   (both are multi-machine-only per `weles-design.md:107-113`). The only reason `resolve` returns
   1 element today is that no shipped `fleet.toml` declares two `[[service]]` with the same
   `provider`. `validate_no_told_peer_to_replicated_provider` (`fleet_toml.rs:275`) already
   forces a consumer of a replicated provider onto `resolve="asks"`.

3. **Gateway routing-as-data: `describe()`-as-data wins — the open research question resolves in
   favour of a smart router, NOT a reverse proxy.** Every `#[http]` op in the tree is fully
   declarative (class (a)): the macro's `gen_decode`/`gen_encode`
   (`tools/rpc-macro/src/lib.rs:657-716`) are 100% generic over `HttpBind`
   (`core/opsapi/src/lib.rs:286-300` — `verb/path/auth/success` + `path_args` + `body_names`).
   **There is no custom decode/encode anywhere.** Match — the design's named worry case
   (`Winner`/`Loser`/`ReportId`) — reduces to serde renames = `body_names` data
   (`api/match/api/src/lib.rs:44-53`); its idempotency 202/409 lives service-side and reaches the
   gateway only as an `opsapi::Status` the generic `encode` maps. A subset tripwire ALREADY ships
   (archcheck rule 17 `tools/archcheck/src/main.rs:563` + `checkmodules` test
   `tools/checkmodules/src/tests.rs:119`).

**Consequence for phasing:** the M1 shape-proving cluster (master/agent split, SQLite, port
minting) is **largely INDEPENDENT** of replicas/round-robin/routing. Single-host replicas do NOT
need minting or SQLite (operator hand-authors distinct ports; state stays supervisor-single-writer
for spawned instances). The only Phase-A item that Phase C (round-robin) genuinely needs is
**Stub re-resolve**. This plan orders the tracks so the independence is explicit and the user can
resequence.

---

## Track map (dependencies, stated honestly)

```
Phase A  (M1 gaps — "shape-proving", mostly standalone)
  A1 rollback + sha-verify-on-read        ── standalone, cheap, first
  A2 master/agent internal role boundary  ── prereq for A3/A4 clean split
  A3 SQLite for master state              ── needed once A4 adds N writers
  A4 port minting (agent-side)            ── needs A2+A3
  A5 Stub re-resolve (resolver closure)   ── standalone; BRIDGES to Phase C

Phase B  (replica prerequisites — small)
  B0 correct stale design errata (#1,#2)  ── doc, first, prevents wasted work
  B1 accounts Epic-OAuth shared state     ── real module blocker #1 (DELETE-RETURNING)
  B1b admin reveal shared state           ── real module blocker #2 (same pattern)
  B2 fleet.toml `replicas` sugar + guard  ── convenience + fail-closed class validator
  B3 splitproof REPLICAS assertion        ── proof on split (CONTENDED delivery)

Phase C  (client-side round-robin — the real client work)
  C1 per-instance connection pool + policy ── needs A5
  C2 replace gateway-svc `exactly_one`     ── the LB authority
  C3 RetryMode × dead-instance interaction ── correctness-critical
  C4 splitproof ROUND-ROBIN assertion

Phase D  (gateway routing-as-data — unblocked by research)
  D1 `describe()` reserved op + manifest    ── per-svc, generic
  D2 runtime route-table build from resolve ── gateway start seam
  D3 convert subset tripwires → describe-cov ── keep the guard
  D4 splitproof ROUTING-AS-DATA assertion

INDEPENDENCE: A2/A3/A4 (minting cluster) share NO code with B/C/D. A user who wants
replicas+RR+routing soonest can run A5 → B → C → D and defer A2/A3/A4. A1 is free-standing.
```

**Shared seam across A5/C2/D2 — NOT independent (reviewer finding 2).** `opsapi::PeerAddr` /
`PEER_SLOT` is mutated by three steps: A5 makes it carry a late-binding (resolver) address, C2
makes it carry a SET (N instances) not one `String`, D2 contributes it from `describe()` into the
same slots `RouteTable::build` reads. These are one coherent type evolution, not three independent
edits — the target shape is **`PeerAddr { provider, addrs: <late-binding set> }`**. Land the
`PeerAddr` shape change ONCE (in A5, sized for the set even though A5 uses one element) so C2/D2
extend it rather than re-shape it; whichever of C2/D2 lands second honours the first's slot shape.
The A2/A3/A4-vs-B/C/D independence is real; this C↔D↔A5 intersection is the one exception.

**Two DISTINCT wire surfaces, two DISTINCT gate families (reviewer finding 3 — do not conflate):**
- **weles AGENT wire** (service↔weles-agent JSON `hello`/`resolve`, and any new agent verb such as
  an A4 mint-report envelope if one is added): static drift gated by `weles-wire-contract`
  (`tools/verifyctl/src/stages/weles_wire_contract.rs`, `drift_probe_*` seams + exhaustive-match
  compile guards) + a live socket proof (`weles-managed-gateway` decoy).
- **backend edge/opsapi wire** (D1 `describe()` is HERE — a reserved `opsapi` op each svc serves
  over `core/edge`, NOT an agent verb): static drift gated by the EXISTING backend contract gates —
  **public-api baseline + contract-golden + codegen-freshness** — + a live proof via splitproof
  (D4's decoy). `weles-wire-contract` does not touch `opsapi` and MUST NOT be extended for
  `describe()`.

Both families share the repo-standard law (the deleted `weles-fleet-parity`'s replacement): every
gate keyed on DATA, every asymmetric arm paid by a live proof.

---

## Context — systems mapped, per item (evidence base)

### Phase A authorities

**A1 rollback.** Generation machinery is complete: `prep::deploy` stages `deploy/gen-N/`
(`prep.rs:409-545`), `copy_and_hash` computes SHA-256 into `GenerationManifest`/`Artifact`
(`prep.rs:305-327,549-599`) at `gen-N/manifest.json`, `flip_current` atomically repoints the
`deploy/current` text file (`prep.rs:627-634`, private fn). `Layout::discover` pins once
(`prep.rs:168-188`); `pin_generation` reads `current` (`prep.rs:276-300`). **sha256 is WRITTEN
at deploy, NEVER READ BACK** — `validate_binaries`/`Layout::binary` only `path.is_file()`
(`prep.rs:349-368`), so the recorded hash is dead. `cli.rs` `Command` enum (`:8-29`) =
Up/Deploy/Status/Down/TestChild only. `deploy()` deliberately takes NO rollout lock
(`prep.rs:404-408`) and its own doc flags "a deploy-scoped guard is M1's job, tracked with
`weles rollback`" — concurrent `current`-mutators (deploy vs rollback) are the known open race.

**A2 master/agent role boundary.** Future-master surface (platform-free): `manifest.rs`
(`ServiceDef`/`Addrs`/`PeerAddrs`), `fleet_toml.rs`, `agentapi.rs` resolve/hello handler BODIES
(pure lookups over an owned `PeerAddrs`), `state.rs`. Future-agent surface (all platform I/O):
`supervisor.rs` (spawn/supervise/restart/`Reporter`/decision fns), `platform/*`, `agentapi.rs`
transport shell + tokio island, `prep.rs`, `control.rs`, `lock.rs`. **Blockers to a clean
split:** (1) `run_up` (`supervisor.rs:626-895`) interleaves both roles in one function with no
seam; (2) `_lock` drop-order is COMMENT-enforced only (`supervisor.rs:648-657` "NOTHING ENFORCES
THAT BUT THIS COMMENT"); (3) `Reporter` is `!Sync` (`Cell`/`RefCell`) single-thread-only
(`supervisor.rs:526-543`); (4) `Layout` conflates master data (pinned `Fleet`) with agent paths
(`run_dir`); (5) `PeerAddrs::from_fleet(defs)` is computed on the supervisor thread and `move`d
into the agent island (`agentapi.rs:300-303`) — the "RPC" is short-circuited to a direct value
move; (6) NO internal crate/module-visibility boundary exists (single binary crate,
`pub(crate)` modules — "master never imports platform" enforced by nobody). Tokio-island
invariants that constrain any split: the runtime may NEVER own `platform/*`, `spawn`
(`SPAWN_LOCK` `std::Mutex` across `CreateProcessW`), `lock.rs`, `state.rs`, `prep.rs`, the signal
handler, or manufacture `Observed::Exited` (`weles-design.md:571-599`, mirrored `agentapi.rs:66-98`).

**A3 SQLite.** Today `state.rs` = whole-document JSON, atomic tmp→rename (`:130-158`), **exactly
one writer** — the supervisor thread's `Reporter` (`supervisor.rs:526-607`); `ControlServer` and
`ReadinessPoller` only READ `shared` or set a bool. `hello` writes NOTHING today but the module
doc flags it as the armed mine (`agentapi.rs:95-96`). Minting + `hello`-registration add writers
FROM THE AGENT'S TOKIO THREAD → concurrent writers to one store, which whole-file-JSON cannot
arbitrate. `rusqlite` is NOT a dep anywhere (net-new); `bundled` statically links SQLite C
(consistent with one-binary-deploy / never-builds), local-pinned like `tokio`/`hyper` in
`weles/Cargo.toml` (zero-sharing). Stays OUT of SQLite: desired state (git manifest), soft
runtime state (reconcilable from agent reports). Goes IN: deploy history, dead-instance port
assignments, API-mutated desired state.

**A4 port minting.** Ports flow: `fleet.toml` literals → `fleet_toml::validate` (unique ports,
`!= AGENT_PORT` 8300) → `manifest::compose_env_with_fleet` writes own `PORT`/`EDGE_ADDR` +
Told-peer `*_EDGE_ADDR` (via `peer_addr`→`service_addr`=`127.0.0.1:{port}`) → `PeerAddrs::from_fleet`
serves the Asks/`resolve` map. Insertion seam = `supervisor.rs` boot loop where `ServiceDef`s are
walked to build `fleet: Vec<Supervised>`, BEFORE `compose_env_with_fleet`/`PeerAddrs::from_fleet`.
Minting precondition (design's consumer-first): a service X is mintable only when ZERO `Addrs::Told`
entries name X as provider — exactly the counting shape of the existing
`validate_no_told_peer_to_replicated_provider`. `validate_unique_ports` must split into
"literal-port TOML uniqueness" (unchanged) vs "minted-port agent-side bind-time uniqueness" (no
TOML value). `weles-async-island`'s `AGENT_PORT` check is about the agent's OWN fixed control
port, unaffected.

**A5 Stub re-resolve.** `Stub` holds `peer_addr: String`, parsed LAZILY at dial
(`core/remote/src/lib.rs:291`), frozen into THREE sinks at `Stub::new` (`:497-512`): `EdgeDialer`,
`probe_loop`, and the `PEER_SLOT` contribution (`opsapi::PeerAddr { provider, addr: String }`,
read by the gateway route table). `Reconnecting` caches ONE conn (`:185-189`) and re-calls
`dialer.dial()` after every `reset` on `ConnectionFatal` (`:200-221`) — so a resolver closure in
the dialer picks up new addresses on reconnect FOR FREE. Minimal change: `EdgeDialer { peer:
String }` → holds a resolver closure invoked where `parse()` is today. **Named cost:**
`resolve_peer` is documented no-retry one-shot-at-boot (`resolve.rs:98-107`) — re-invoking at
runtime is a stated contract shift, must be named, not smuggled. The awkward sink is `PEER_SLOT`
(a frozen `String` contributed in `init` before any I/O) — a re-resolving edge address is not
visible to it without changing `PeerAddr` to carry late-binding.

### Phase B/C/D authorities

**B1 accounts Epic OAuth.** `epic_oauth.rs:53` `states: Mutex<HashMap<String, OauthState>>` binds
in-flight authorization `state` → session (`new_state:105-118`, `take_state:122`, 10-min TTL,
single-redemption). Under `replicas: 2` the callback LB-routes to a replica whose map lacks the
state → `take_state`→`None`→login fails. Fix: `accounts.oauth_states` table with delete-returning
redemption (exactly-once `take_state`), OR sticky sessions for `/accounts/epic/*`. Affects ONLY
Epic web OAuth (needs `EPIC_CLIENT_SECRET`); dev/password/OIDC/sessions unaffected. Sibling JWKS
cache (`epic.rs:80`) is an idempotent read-through — already replica-safe.

**C1-C3 round-robin.** The LB authority is `cmd/gateway-svc/src/addrs.rs:378 exactly_one`, which
`bail!`s on `n>1` ("choosing between instances is load balancing… taking the first would look
healthy while sending part of the traffic nowhere"). `Reconnecting`/`EdgeDialer` hold ONE conn/one
peer. Needs (each lands somewhere specific): (1) a pool type beside `Reconnecting` mapping
instance-addr→its own conn; (2) selection state (atomic cursor/policy) — none exists today,
`Reconnecting::call` never chooses; (3) per-instance health — `probe_loop` stamps ONE verdict
(`:407-428`) feeding one `/readyz` `ReadyCheck`, needs fan-out + rethink of what the single check
reports; (4) `RetryMode` — `Reconnecting::call` (`:241-274`) replays once ONLY for
`OnceAfterReconnect`, `Never` (mutations) never replays; with a pool, a redial to a DIFFERENT
instance is safe for `#[retry_safe]` reads but a mutation must NEVER silently re-send — the
authority STAYS `RetryMode`, not a new pool knob. Also: `PEER_SLOT` carries one `String`, and the
gateway's Remote HTTP dispatch (not just capability Stub conns) must also load-balance or it stays
single-instance while only capability calls balance.

**D1-D2 routing-as-data.** Macro holds per-op `HttpBind` at glue-gen: `verb/path/auth/success`,
`path_args` (arg←`{wildcard}`), `body_names` (arg→external JSON key). `gen_decode` = default
request → `from_slice` body → inject wildcards → `to_vec`; `gen_encode` = unwrap `{status,err,value}`
envelope. Both fully generic. Route table built at `Gateway::start` (`modules/gateway/src/lib.rs:277`)
— runs AFTER every module `init`, eagerly `build_table()` with collision-`bail!` on duplicate
binding/local/peer/operation/overlap (`:660-723`) — the seam to preserve. Today providers/ops come
from the hand-listed 6 `Stub::new` in `cmd/gateway-svc/src/lib.rs:42-72`, each importing
`<name>rpc::remote_factories()` (compile-time). `Stub::start` already runs an async `RemoteBoot`
hook per stub (`core/remote/src/lib.rs:539-562`) — the natural place a `describe()` call + slot
contribution runs (I/O phase, not `init`). Two behavioral caveats: a typeless passthrough decode is
wire-JSON-equivalent but maybe not byte-identical (number/whitespace normalization); malformed-JSON
400 shifts from gateway to svc. Neither forces reverse-proxy. Optional gateway-side body validation
is describable via the macro's existing `body_shapes()` (`rpc-macro/src/lib.rs:275-289`).

### Proof surfaces (shared across phases)

Four BLOCKING weles gates (`tools/verifyctl/src/stages/mod.rs:53-158`): `weles-managed-gateway`
(the only live weles↔remote interop gate; boots `fleet.split.toml`, `swap_probe` proves resolved
address is USED via an owned fake agent, `decoy_run` proves managed-resolve has no env fallback;
borrows verifyctl's lease), `weles-async-island` (tokio feature bans + `AGENT_PORT`),
`weles-wire-contract` (pure in-memory drift gate — the template for a new verb, `drift_probe_*`
seams + exhaustive-match compile non-forgettability + `declared_variants` coverage + an explicit
NOT-pinned list with a reason per entry), `split-proof`. Splitproof extension seam: the
restart-and-reprove templates `i_gate` (`main.rs:1810`, respawn from a mutated `ServiceSpec`) and
`rdy_dead` (`:594`, kill→503→recover). A REPLICAS/ROUND-ROBIN/ROUTING assertion is a new
`ServiceSpec`-driven scenario in that harness.

---

## Ordered steps

### PHASE A — M1 gaps

#### A1 — `weles rollback` + sha-verify-on-read `[opus / core-implementer, think]`
**(a) What:** `cli.rs` add `Rollback { root, generation: Option<String> }` (copy the `Deploy`
arg-parse + `--root` pattern `:88-124`) + `USAGE` line + `main.rs` dispatch arm (`:38-43`); new
`prep::rollback(layout, target: Option<&str>)` beside `deploy`; new
`prep::verify_generation(gen_dir)` that LOADS `manifest.json`, recomputes SHA-256 per artifact,
compares — the currently-missing read path; wire `verify_generation` into `Layout::discover`
(`prep.rs:168`) so EVERY boot (up + rollback) verifies the pinned generation before spawn.
**(b) Why now / order:** standalone, cheap, closes a visible gap, and `verify_generation` is a
strict improvement independent of rollback. First so later steps boot on verified generations.
**(c) How:** `rollback(None)` = repoint `current` to the gen BEFORE the current pointer (parse
`gen-N`, pick predecessor that exists + has a parseable manifest); `rollback(Some("gen-3"))` =
explicit target. Validate target exists + `verify_generation` passes BEFORE `flip_current`. Reuse
the retention `protected` set (`live_pinned_generation` `prep.rs:650-656`) so rollback never
prunes a live-pinned gen. Rollback, like deploy, takes NO rollout lock (a live `up` pinned at
discover is immune) — BUT close deploy's own flagged race: add a small `deploy/`-scoped file lock
(distinct from `run/rollout.lock`) shared by `deploy` AND `rollback` so two `current`-mutators
can't interleave (`prep.rs:404-408`'s "M1's job"). **Prove the failing branch:** (i) a gen with a
tampered artifact byte ⇒ `verify_generation` FAILs (the dead-hash path now lives); (ii)
`rollback` repoints `current` + a subsequent `up` boots the older gen; (iii) `rollback` refuses a
gen whose manifest is missing/corrupt; (iv) two concurrent `current`-mutators serialize on the
new deploy-scoped lock. Sweep: grep for any other `path.is_file()`-only validation that should
hash.
**Commit:** `feat(weles): weles rollback + sha-verify-on-read on generation boot`

#### A2 — Draw the master/agent role boundary internally `[fable-or-opus / core-implementer, think hard]`
**(a) What:** introduce an internal module-visibility boundary WITHOUT splitting processes yet: a
`master` module (owning `manifest`, `fleet_toml`, `state`, the resolve/hello handler bodies) that
is mechanically forbidden from importing `platform`/`supervisor`/`lock`; extract `run_up`'s
master-shaped prologue (discover/validate/pin, derive `PeerAddrs`) from its agent-shaped body
(lock, signal, spawn, boot, monitor, teardown) into two named functions with an explicit typed
seam (`PeerAddrs`/`ServiceDef` passed as an owned value TODAY, shaped as the future RPC boundary).
**(b) Why now / order:** prerequisite for A3/A4 — SQLite lives master-side, minting is agent-side,
and both need the boundary to exist before they can be placed on the correct side. Before minting
so minting doesn't cement the monolithic `run_up`.
**(c) How:** do NOT move to a second process (that's machine-two). Keep one binary, Nomad's dev
shape. The mechanical boundary MUST be COMPILER-enforced, not a source-string grep (reviewer
finding 4: a `grep "use crate::platform"` test misses `use crate::platform as p`, fully-qualified
`crate::supervisor::X`, and re-exports — it IS the comment-enforced anti-pattern this very step
lists as a blocker). So a **`weles-master` path-dep sub-crate** that simply cannot name
`platform`/`supervisor`/`lock` in its `Cargo.toml` dep graph (the compiler rejects the `use`), OR
a real Rust module-privacy boundary where the platform types are unreachable from `master`. The
grep-test option is REJECTED. Do
NOT touch the tokio-island invariants: `Reporter` stays `!Sync` single-thread; `_lock` stays the
last-dropped RAII local (the split extracts functions in ONE thread, does not add a channel that
would break drop-order); `PeerAddrs::from_fleet` stays a value move for now (the RPC shape is
modelled by the function signature, the wire is deferred). **Prove:** a test that `master`-side
code referencing `platform`/`spawn` FAILS to compile (or the archcheck-analog test FAILs); the
extracted prologue/body produce byte-identical boot behavior (existing supervisor tests stay
green). **Named semantic change:** this is structural only, no behavior change — record in the
commit that it is a boundary-drawing refactor, not a process split.
**Commit:** `refactor(weles): internal master/agent role boundary (module-visibility, no process split)`

#### A3 — SQLite for master runtime state `[opus / core-implementer, think hard]`
**(a) What:** add `rusqlite` (bundled, local-pinned) to `weles/Cargo.toml`; a `master::store`
SQLite DB at `run/weles/state.db` holding deploy history + dead-instance port assignments +
(future) API-mutated desired state; KEEP `state.json` for the soft live-fleet snapshot that
`status`/`down` read (or migrate it — decide in-step, default: keep JSON for soft state, SQLite
for durable-runtime-only, matching the design's "most runtime state is soft" line).
**(b) Why now / order:** must precede A4 — minting introduces the second writer (agent thread
minting + reporting up) that whole-file JSON cannot arbitrate; SQLite's write-concurrency is the
WHOLE reason it exists (`weles-design.md:308-311`), not storage.
**(c) How:** single-connection-per-writer with WAL; the master owns writes, the agent reports
minted ports UP to the master (in-process today, over the seam A2 drew). Do NOT put soft
live-status in SQLite (reconcilable from agent reports). Schema: `deploy_history(gen, sha_root,
deployed_unix)`, `port_assignment(instance_id, provider, port, alive)`. **Prove:** two concurrent
writers (supervisor checkpoint + a simulated agent mint-report) both commit without loss — the
exact race whole-file JSON loses; a master restart rebuilds soft state from the manifest +
reconciliation while reading durable rows from SQLite.
**Commit:** `feat(weles): SQLite master store for durable runtime state (deploy history, port assignments)`

#### A4 — Agent-side port minting, consumer-first `[fable-or-opus / core-implementer, think hard]`
**(a) What:** a `Mint` port variant on `ServiceDef` port fields consumed at spawn; the agent binds
an ephemeral/free port at spawn and patches the live `ServiceDef.http_port`/`edge_port` in the
booting slice BEFORE `compose_env_with_fleet`/`PeerAddrs::from_fleet` run for consumers; split
`fleet_toml::validate_unique_ports` into literal-uniqueness (TOML-time) + minted-bind-uniqueness
(agent runtime invariant: never hand out a bound port); a new validator rule "a mintable provider
has ZERO Told consumers" (the counting shape of `validate_no_told_peer_to_replicated_provider`).
**(b) Why now / order:** last in the cluster — needs A2 (agent side) + A3 (persist minted ports).
Gateway is the design's named first mintable service (dials 6 peers, nothing dials its edge, own
port stays static as the public front door).
**(c) How:** minting spreads consumer-first — a service goes mintable only once every consumer of
it is `resolve="asks"`. Persist minted ports in A3's `port_assignment`. The `AGENT_PORT` island
check is unaffected (agent's own control port stays fixed). **Wire-gate discipline:** minting adds
no NEW agent verb (resolve already answers minted addresses via `PeerAddrs`), so it extends
`weles-managed-gateway` (a minted-port live proof) but not necessarily `weles-wire-contract`
unless a report-mint envelope is added. **Prove:** a mintable service boots on an OS-assigned port
and its Asks-consumer resolves that exact port (not a literal); a `fleet.toml` making a
Told-consumed provider mintable FAILs validation; minted-port uniqueness holds under a forced
collision.
**Commit:** `feat(weles): agent-side port minting (consumer-first, minted-uniqueness validator)`

#### A5 — Stub re-resolve via a resolver closure `[opus / core-implementer, think hard]`
**(a) What:** replace `EdgeDialer { peer: String }` with a dialer holding a resolver closure
(`Fn() -> Result<SocketAddr, Error>` or async) invoked inside `dial()` where `parse()` is today
(`core/remote/src/lib.rs:290-291`); thread the resolver through `Stub::new` the way `peer_addr` is,
updating all THREE sinks (dialer, `probe_loop`, `PEER_SLOT`); change `opsapi::PeerAddr` to the
final shape `{ provider, addrs: <late-binding set> }` NOW (reviewer finding 2 — this is the one
type C2/D2 also touch; size it for the set here even though A5 populates one element, so C2/D2
extend it rather than re-shape it).
**(b) Why now / order:** standalone within Phase A, and the BRIDGE to Phase C (round-robin's pool
sits on top of a resolver, not a frozen string). Do it before C.
**(c) How:** `Reconnecting::get` already re-dials after `reset` on `ConnectionFatal`, so
re-resolve-on-reconnect falls out for free. **Named contract shift:** `resolve_peer` becomes a
runtime call, not boot-only (`resolve.rs:98-107`) — record it, adjust the `RESOLVE_TIMEOUT`
rationale. For now keep single-address semantics (the resolver returns one addr); the LIST/LB is
Phase C. **Prove:** a Stub whose resolver returns addr A then addr B, after a forced
`ConnectionFatal`, dials B without a consumer restart (the property the design leans on); the
`PEER_SLOT`/route-table path reflects the new address.
**Commit:** `feat(remote): Stub re-resolve via resolver closure (runtime address change, no restart)`

### PHASE B — replica prerequisites

#### B0 — Correct the two stale design claims (dated errata) `[inline]`
**(a) What:** `docs/reference/weles-design.md` — prepend a dated errata (the file's own
convention) correcting: (#1) the `EVENTS_ORIGIN`/relay-advisory-lock line (`:707-709`) — the pull
plane is ALREADY replica-safe via `FOR UPDATE SKIP LOCKED`, no relay/origin exists; (#2) restate
positively that single-host replicas need only distinct loopback ports (mTLS/DNS are
multi-machine-only), and that `PeerAddrs::from_fleet` + the wire already carry the list.
**(b) Why now / order:** FIRST in Phase B — it prevents building the deleted-architecture advisory
lock, which is the single largest wasted-work risk in the whole plan.
**(c) How:** preserve stale body prose, prepend errata, per "historical docs are archives." Name
BOTH real module blockers (accounts Epic OAuth AND admin show-once reveal — the same
request-spanning in-memory redemption class, reviewer finding 1) and the two soft rate-limit
dilutions (admin/gateway) so the replica-safety picture is stated in one place, including the
fail-closed class validator (B2) that catches the next such module.
**Commit:** `docs(weles): errata — pull plane already replica-safe; single-host replicas need only distinct ports`

#### B1 — accounts Epic-OAuth shared state store `[opus / core-implementer, think]`
**(a) What:** move `epic_oauth.rs` `states: Mutex<HashMap>` into a shared `accounts.oauth_states`
table (columns: `state PK`, `session_token`, `browser_binding`, `created_at`); `new_state` =
INSERT, `take_state` = `DELETE ... RETURNING` (exactly-once single-redemption, TTL-checked), prune
expired.
**(b) Why now / order:** the ONLY correctness blocker for `replicas: 2` — before B3 can prove
replicas end-to-end.
**(c) How:** DELETE-RETURNING gives cross-replica exactly-once redemption (the callback can land
anywhere). 10-min TTL as a `created_at` predicate. No data migration (wipe strategy). Leave
dev/password/OIDC/sessions untouched. **Prove:** `new_state` on one pool, `take_state` on a SECOND
pool (simulating the other replica) succeeds exactly once; a second `take_state` returns `None`; an
expired state is not redeemable.
**Commit:** `fix(accounts): shared oauth_states table (Epic web-OAuth replica-safe)`

#### B1b — admin show-once reveal shared state store `[opus / core-implementer, think]`
**(a) What:** move `modules/admin/src/lib.rs` `RevealStore` (`Arc<Mutex<RevealStore>>` `:496`) into
a shared `admin.reveals` table (columns: `token PK`, `payload`, `created_at`); `stash_reveal`
(`:649`) = INSERT, `take_reveal` (`:660`) = `DELETE ... RETURNING` (exactly-once single-redemption,
TTL-checked); prune expired.
**(b) Why now / order:** the SECOND correctness blocker for `replicas: 2` (reviewer finding 1) —
same class as B1, same DELETE-RETURNING pattern, before B3 proves replicas end-to-end. Independent
of B1 (different module/schema), own commit.
**(c) How:** identical shape to B1 — the follow-up GET redeems on whichever replica serves it. No
data migration (wipe strategy). Fix the false in-line doc (`:179-181`, which reasons monolith-vs-
split not replicas) in the same rollout (prose-about-code discipline). **Prove:** `stash_reveal`
on one pool, `take_reveal` on a SECOND pool succeeds exactly once; a second `take_reveal` → `None`;
expired token not redeemable — the show-once secret survives a cross-replica POST→GET.
**Commit:** `fix(admin): shared reveals table (show-once secret replica-safe)`

#### B2 — `fleet.toml` `replicas: N` sugar + fail-closed class validator `[opus / core-implementer, think]`
**(a) What:** optional `replicas: Option<u32>` on `[[service]]`; validator expands it to N
`ServiceDef`s with distinct `name`s (`<name>#1..N`) and distinct ports (minted if A4 landed, else
require a port list); PLUS a **fail-closed class validator**: `replicas > 1` is REJECTED for any
service whose module is declared to hold request-spanning in-memory redemption state, until that
module is on a shared store. The existing `validate_no_told_peer_to_replicated_provider` already
forces consumers onto `asks`.
**(b) Why now / order:** the class validator is the AUTHORITY that catches this bug class for
FUTURE modules (reviewer finding 1's Fix-the-Authority half — fixing B1/B1b closes today's two
breakers; the validator stops the next one shipping silently). After B1/B1b so the two known
modules pass. Not `[sonnet]` any more — the validator is a real fail-closed decision, not
scaffolding.
**(c) How:** the "holds in-memory redemption state" fact is a small explicit allow/deny list keyed
by provider (the modules are known; a hand-list here is legitimate BUT must itself be diffed — a
new module is presumed-unsafe until it declares itself redemption-free or shared-store-backed,
fail-closed, so the list can't silently rot). If A4 (minting) is NOT done, `replicas` requires an
explicit per-replica port list (fail-closed, no silent port guessing — anti-magic). **Prove:**
`replicas: 2` expands to two validated `ServiceDef`s; `replicas: 2` on a redemption-state module
NOT yet on a shared store FAILs closed; a Told consumer of a replicated provider FAILs.
**Commit:** `feat(weles): fleet.toml replicas sugar + fail-closed redemption-state validator`

#### B3 — splitproof REPLICAS assertion (CONTENDED delivery) `[opus / core-implementer, think]`
**(a) What:** a new `tools/splitproof` scenario: spawn TWO instances of one durable-consumer
service from two `ServiceSpec`s with distinct ports, enqueue a BACKLOG with BOTH workers already
polling (forced contention on one subscription row), DB-assert each event's effect applied EXACTLY
once, and assert both serve.
**(b) Why now / order:** proof on the AT-RISK topology (split) — after B1/B1b/B2 make replicas
real. **Framed honestly (reviewer finding 5): this is a REGRESSION GUARD, not a fix's failing
branch** — the plane is already safe; B3 pins that it stays safe.
**(c) How:** reuse `i_gate`/`rdy_dead` (`main.rs:1810`,`:594`); build a second `ServiceSpec`. The
proof must force the row-lock to be CONTENDED: enqueue a batch AFTER both workers are up and
polling (not spawn-drain-then-spawn, which passes by sequencing even with no lock — the finding-5
failure mode). DB-assert an exact aggregate (row count / exact MMR delta), not "no error". **Prove
the failing branch:** constructed so that if `FOR UPDATE SKIP LOCKED` were absent, the contended
batch would double-apply and the aggregate assertion would FAIL.
**Commit:** `test(splitproof): replicas assertion — contended consumer-group exactly-once`

### PHASE C — client-side round-robin

#### C1 — Per-instance connection pool + selection policy `[fable-or-opus / core-implementer, think hard]`
**(a) What:** a new pool type in `core/remote` beside `Reconnecting` holding N instance dialers
(each its own conn), a selection cursor (atomic round-robin), fed by a resolver that returns the
LIST (A5's resolver generalized from one addr to many).
**(b) Why now / order:** needs A5 (resolver closure). First in Phase C — C2/C3 build on the pool.
**(c) How:** the pool wraps N `EdgeDialer`s; `probe_loop` fans out to a per-instance verdict so
selection skips a dead instance; the single `/readyz` `ReadyCheck` becomes all-down vs some-down.
**Prove:** with a resolver returning [A,B], calls distribute across A and B (assert both receive
traffic); a dead instance is skipped by selection.
**Commit:** `feat(remote): per-instance connection pool + round-robin selection`

#### C2 — Replace gateway-svc `exactly_one` with LB `[opus / core-implementer, think]`
**(a) What:** `cmd/gateway-svc/src/addrs.rs` — the `exactly_one` `bail!`-on-`n>1` authority
(`:378-409`) becomes "accept the list, hand it to the pool"; `PEER_SLOT` (`opsapi::PeerAddr`) and
the gateway's Remote HTTP dispatch carry a set, not one `String`.
**(b) Why now / order:** the LB decision authority — after C1's pool exists to receive the list.
**(c) How:** preserve the `0`/empty bail (still un-actionable). The gateway's own Remote HTTP
dispatch must also balance or capability calls balance while HTTP stays single-instance — do both.
**Prove:** gateway-svc with a 2-instance resolve answer routes across both; empty still bails.
**Commit:** `feat(gateway): client-side load-balance resolved instances (replace exactly_one)`

#### C3 — `RetryMode` × dead-instance interaction `[opus / core-implementer, think hard]`
**(a) What:** the pool's `call` retry-to-another-instance decision stays gated on
`opsapi::RetryMode` EXACTLY as the single-conn path: `OnceAfterReconnect` (reads/`#[retry_safe]`)
may redial a DIFFERENT instance and replay once; `Never` (mutations) must NEVER silently re-send.
**(b) Why now / order:** correctness-critical, the sharp edge the design names — after C1/C2.
**(c) How:** `RetryMode` stays the WHETHER-to-retry authority (matches `Reconnecting::call`
`core/remote/src/lib.rs:256`), not a new pool knob. The pool's selection cursor is a legitimate
SECOND, orthogonal input deciding WHERE a permitted (`OnceAfterReconnect`) retry lands — it must
advance OFF the dead instance or the replay hits the same corpse (reviewer finding 7: name this
so it doesn't read as a smuggled knob). A mid-request instance death on a mutation (`Never`)
returns the error, no cross-instance replay (double-execute hazard). **Prove the failing branch:**
a mutation whose instance dies mid-call does NOT re-execute on another instance (assert
exactly-once side effect); a `#[retry_safe]` read DOES transparently retry on a DIFFERENT
(cursor-advanced) instance.
**Commit:** `feat(remote): RetryMode-gated cross-instance retry (mutations never double-send)`

#### C4 — splitproof ROUND-ROBIN assertion `[sonnet]`
**(a) What:** drive N requests through gateway-svc against a 2-instance provider, assert spread
(distinct per-instance markers in responses or per-instance DB/log rows).
**(b) Why now / order:** proof on split — after C1-C3.
**(c) How:** extend the P3/K-series HTTP-through-gateway assertions (`main.rs:894-931`); assert
distribution, not just success. **Prove:** the assertion FAILs if all traffic hits one instance.
**Commit:** `test(splitproof): round-robin distribution across two instances`

### PHASE D — gateway routing-as-data

#### D1 — Reserved `describe()` op + manifest `[fable-or-opus / core-implementer, think hard]`
**(a) What:** a reserved `describe()` edge op every svc serves, returning its `#[http]` op
manifest as DATA — per op `{verb, path, auth, success}` + per-arg `{wire_key, source: body |
path(wildcard)}` (exactly `HttpBind`). Generated once by the macro (it already holds this at
glue-gen).
**(b) Why now / order:** first in D — D2 consumes the manifest. Research resolved the fork:
describe-as-data, NOT reverse-proxy.
**(c) How:** the macro already emits the shape data (`HttpBind`, `body_shapes()`); expose it as a
reserved `opsapi` op. **Gate discipline (MANDATORY, correct family — reviewer finding 3):**
`describe()` is a BACKEND edge/opsapi op, NOT a weles agent verb — its static drift is caught by
the EXISTING backend contract gates (**public-api baseline + contract-golden + codegen-freshness**,
re-blessed intentionally), NOT `weles-wire-contract` (which gates only the orchestrator agent wire
and does not touch `opsapi`). The LIVE proof is splitproof (D4's decoy). Do NOT extend
`weles-wire-contract` for `describe()`. **Prove:** describe round-trips every op's `HttpBind`
byte-identically; a new `#[http]` op appears in describe without gateway changes.
**Commit:** `feat(opsapi,rpc): reserved describe() op — #[http] manifest as data`

#### D2 — Runtime route-table build from resolve + describe `[fable-or-opus / core-implementer, think hard]`
**(a) What:** replace the compile-time `<name>rpc` import set in `cmd/gateway-svc/src/lib.rs:42-72`
with a runtime pass: after `resolve` returns the peer list, call each peer's `describe()`,
reconstruct the generic decode (raw body passthrough + inject wildcards under wire-keys) / encode
(unwrap `{status,err,value}`), contribute `Operation`/`OpBinding`/`PEER_SLOT` into the SAME slots
`RouteTable::build` reads — preserving its collision-`bail!`.
**(b) Why now / order:** the payoff — adding a module needs ZERO gateway changes. After D1.
**(c) How:** the contribution pass runs in `Stub::start` (the async `RemoteBoot` hook,
`core/remote/src/lib.rs:539-562`), BEFORE `Gateway::start`'s eager `build_table()`. Keep
LOCAL_SLOT empty for stub-fronted ops (always Remote). **Record THREE behavioral caveats:**
(i) typeless passthrough decode is wire-JSON-equivalent not necessarily byte-identical
(number/whitespace); (ii) malformed-JSON 400 shifts gateway→svc; (iii) empty/absent-body POST —
today `gen_decode` synthesizes `Request::default()` then re-serializes (`rpc-macro/src/lib.rs:682`),
so the svc receives a valid default-struct JSON; a raw passthrough forwarding empty bytes changes
that decode outcome (reviewer finding 6) — the data-driven decode must replicate the default
synthesis, not forward empty bytes. **Prove:** a route resolved purely
from describe-data reaches the right peer with the right verb/path/auth/status (match's
`Winner`/`Loser` included); the collision-`bail!` still fires on a duplicated method.
**Commit:** `feat(gateway): runtime route table from resolve + describe() (routing-as-data)`

#### D3 — Convert the subset tripwires to describe-coverage `[sonnet]`
**(a) What:** archcheck rule 17 (`tools/archcheck/src/main.rs:563`) + checkmodules
`gateway_stubs_every_http_domain` (`tools/checkmodules/src/tests.rs:119`) currently assert
stub-list ⊇ `#[http]` domains; once routing is data-driven they become "every `#[http]` domain is
reachable via describe-coverage."
**(b) Why now / order:** keep the guard alive across the mechanism change — after D2.
**(c) How:** the guard's SUBJECT changes from the hand-listed stub set to the describe manifest;
the invariant (no `#[http]` domain silently unreachable) is preserved. **Prove:** removing a
domain from the resolve/describe set FAILs the converted guard.
**Commit:** `refactor(archcheck,checkmodules): stub-list guards become describe-coverage guards`

#### D4 — splitproof ROUTING-AS-DATA assertion `[opus / core-implementer, think]`
**(a) What:** assert an op routed purely from runtime describe-data (no compile-time `<name>rpc`
import) reaches the right peer through gateway-svc.
**(b) Why now / order:** proof on split — last.
**(c) How:** plug into the existing HTTP-through-gateway assertions. The falsifiable form is a
decoy peer whose `describe()` OMITS an op ⇒ the gateway must 404 that op (reviewer finding 8: this
is the real check — the "route came from a compile-time import" phrasing is circular once D2
removes the imports, drop it).
**Prove:** the decoy-omits-op assertion FAILs (op wrongly served) if the gateway fell back to any
non-describe route source.
**Commit:** `test(splitproof): routing-as-data — op reachable only via runtime describe`

---

## Verification (per phase, one rollout at a time — `pgrep -x cargo`/`rustc` first)
- After each Phase-A weles step: `cargo test -p weles` (self-contained, safe alone).
- After A5/C*: `cargo test -p remote -p gateway`.
- After B*/D*: `cargo run -p verifyctl -- --fast` (build, fortress, split-proof, the weles gates).
- New wire verbs (A4 report-mint if added, D1 describe): confirm `weles-wire-contract` +
  `weles-managed-gateway` both extended and both can FAIL (decoy).
- Trailer audit after each rollout: `git log -<N> --format="%h %B" | grep "Co-Authored"` — match
  each lane (`[fable]`→Fable 5, `[opus]`→Opus 4.8, `[sonnet]`→Sonnet 4.6).

## Out of scope (named, not dropped)
- All multi-machine work: agent↔master mTLS hop, the orchestrator's own CA, real host:port from
  agent-observed IP, `EdgeDialer` DNS, agent-side cache for remote peers, master migration by
  repoint, cross-host artifact distribution. (Research confirmed NONE are single-host-replica
  blockers.)
- Liveness-tracked `resolve` (`200 {addrs:[]}` semantics) — deferred to M2; single-host RR uses
  the existing `ConnectionFatal` mapping as a first cut.
- Master election / Raft / HA — explicitly rejected in the design.
- Gateway-side body validation (optional, describable via `body_shapes()` if wanted later).

## Dispatch tags — user to approve at ExitPlanMode
Phase A: A1 [opus], A2 [fable-or-opus], A3 [opus], A4 [fable-or-opus], A5 [opus] — all
core-implementer. Phase B: B0 [inline], B1 [opus], B1b [opus], B2 [opus], B3 [opus]. Phase C: C1
[fable-or-opus], C2 [opus], C3 [opus], C4 [sonnet]. Phase D: D1 [fable-or-opus], D2
[fable-or-opus], D3 [sonnet], D4 [opus]. ("[fable-or-opus]" = session-model top tier; resolve to
a concrete model at dispatch per the trailer rule.) All code lanes are `core-implementer`; only
B0 [inline] and D3/C4 [sonnet] mechanical.
