# Plan: weles fleet from a hand-authored `fleet.toml` (kill the hardcoded manifest)

**Date:** 2026-07-18 10:48
**Scope:** `weles` crate + `tools/verifyctl` (2 stages). No other crate is affected.
**Status:** DRAFT v3 — two grumpy-reviewer passes done. v3 resolves the second pass:
B1 (CA idempotency premise was false), M1–M4, and minors. Pending user approval.

---

## Context & goal

`weles up split` / `weles up monolith` today dispatch to two **hardcoded Rust
functions** (`weles/src/manifest.rs::split_fleet()` / `monolith()`) that bake the
whole fleet — service names, packages, ports, peer wiring, env, PG budgets — into
weles's source. That is repo-specific topology knowledge living inside a crate whose
entire identity is "generic, zero-sharing, extractable orchestrator." A C#-authored
service, or anyone deploying without `tools/`, cannot use it.

**The decided end-shape (settled with the user across this session):**

1. **weles has NO concept of monolith/split.** It supervises *a fleet*. Monolith is
   a fleet of one process; split is a fleet of twelve. Both are just different
   `fleet.toml` files. `enum Topology` and the `up [split|monolith]` argument are
   deleted; `weles up` reads whatever fleet was deployed.
2. **The fleet is a hand-authored strict `fleet.toml`** (operator/CI writes it —
   `weles deploy … --fleet <path>` stamps it into the generation, `weles up` reads it
   from `deploy/current`). Strict data config per the repo's anti-magic rule:
   `#[serde(deny_unknown_fields)]`, no layering, no templating, `--dry-run` validated.
   A deliberate, recorded exception to "prefer typed Rust config" (memory
   `config-as-code-anti-magic`): the fleet definition MUST be readable without
   recompiling weles at a deploy site weles does not control.
3. **All composition/wiring logic STAYS in Rust**, now operating on owned data parsed
   from TOML: `compose_env_with_fleet`, `PeerAddrs`, `peer_addr`, `service_addr`,
   `Addrs::Told`/`Asks`, `AddrKind`, the env-forwarding logic. Only the *source of the
   data* changes (Rust literals → TOML), never the wiring.
4. **weles loses its domain KNOWLEDGE — but keeps its generic CAPABILITIES.** This is
   the distinction the earlier draft blurred:
   - **Removed (domain knowledge):** the pg-session-budget machinery
     (`PG_SESSION_BUDGET`, `PLANE_DEDICATED_SESSIONS`, `SCHEDULER_FIRE_SESSIONS`,
     `HARNESS_RESERVE`, `service_pg_budget`, `fleet_pg_budget`, `validate_pg_budget`),
     `has_db`, `pool_max`, `RuntimeInputs`, `prep::database_url()` + `DEFAULT_DATABASE_URL`
     (`prep.rs:72-73,232-233`), the *meaning* of `DATABASE_URL`, the DB/CA injection
     block in `compose_env_with_fleet` (`manifest.rs:617-634`), the hardcoded
     `svc.name == "gateway-svc"` special-case, and the typed `CaPaths` return.
   - **KEPT, generalized (capability):** the ability to *run a declared command before
     the fleet starts*. `prep::mint_ca`/`seed_admin` are NOT deleted — today they only
     `Command`-launch opaque binaries (`edgeca`, `adminctl`) with a deadline + logging +
     exit-check. They collapse into one generic, manifest-driven `run_prepare` (see
     **D-PREPARE**). weles keeps "spawn a process I was told to," which is exactly its
     job; it just stops knowing that one of them mints a CA.
5. **Opaque operator env replaces every domain injection.** DSN, CA paths, pool caps,
   secrets — none are weles concepts. They reach a service two domain-blind ways: (a)
   the service's `[service.env]` table in `fleet.toml`, or (b) a **passthrough list** —
   `fleet.toml` names env KEYS that weles forwards from its OWN environment. Passthrough
   replaces the hardcoded `SERVICE_ENV_ALLOWLIST` const (`manifest.rs:52-65`) — weles
   knows only the key NAME, never the meaning. `DATABASE_URL`/`EDGE_CA_*`/
   `DATABASE_POOL_MAX_CONNECTIONS` are just ordinary keys. NO fleet-wide `[env]` layer
   (that is the layering the anti-magic rule bans); shared values via passthrough,
   per-service values via `[service.env]`.

   **No manual operator pre-step.** Provisioning is declarative in `fleet.toml`
   (`[[prepare]]`) and run by weles on EVERY `weles up`, before spawn. `devctl` keeps
   its own CA+admin path; the everyday `devctl up` flow is unchanged. The prepare
   commands must tolerate re-running (see D-PREPARE): `adminctl create-user` is an
   upsert; `edgeca` **regenerates** the CA each `up` — deliberate, because the dev CA is
   ephemeral per `weles up` session (it should not survive teardown) and `rollout.lock`
   forbids a second `up` under a running fleet.

### Overlap analysis — why not extend an existing seam

- **`processctl::fleet` (the "other world").** `tools/processctl/src/fleet.rs` is the
  typed fleet consumed by devctl/verifyctl/splitproof. weles's manifest is a *hand
  copy* of it. The obvious "extend" is: import it. **Rejected — it is the load-bearing
  zero-sharing violation.** weles must never import a workspace crate; the two are "two
  different worlds" by design (user, this session). We remove weles's copy in favour of
  an operator-authored file. Confirmed: the only compiled cross-edge is `tools/verifyctl`
  dep'ing weles, nothing else.
- **`weles::agentapi` hello/resolve (managed mode / M1).** Could the fleet come from
  service self-registration instead of a file? **No — bootstrap ordering.** weles must
  know *what to spawn before anything runs*; `hello` is post-spawn. Self-describe
  (`--describe`) was rejected: a Rust-centric extra contract a C# service may not
  implement. The irreducible deploy-time datum is the launch list + prepare hooks +
  non-derivable operator env — which is exactly what `fleet.toml` carries. Managed mode
  (M1) is orthogonal and NOT required; `resolve`'s `unknown_peer` is itself "derived
  from the manifest," so this file *is* what M1 would resolve against later.
- **`weles-fleet-parity` verify stage.** Its whole purpose is guarding the hand-copy
  from drift. Once the copy is gone, its premise evaporates — see D-PARITY.

### Decision record (for the reviewer + errata)

- **D-PREPARE:** `prep::mint_ca`/`seed_admin` (`prep.rs:622,690`) — today `Command`-launch
  the binaries (`edgeca`, `adminctl`) with deadline + `run_dir` logging + exit-check —
  collapse into one generic `run_prepare(&[PrepareCmd], &Layout) -> Result<()>`: each
  `PrepareCmd` = `{ name, run, args, env, passthrough, timeout_secs }`, from the manifest;
  runs with **`cwd = layout.root`** (like `mint_ca` today, `prep.rs:709`), before fleet
  spawn, logs to `run_dir/<name>.{out,err}.log`, nonzero exit (or `timeout_secs` elapse,
  default 30 — from `MINT_CA_TIMEOUT`/`SEED_ADMIN_TIMEOUT`, both 30 today) aborts the
  whole `up`. weles knows the command NAME, not its meaning — the same philosophy as
  passthrough. `run_prepare` carries **NO idempotency logic** (that was the reviewer's B1
  correction).

  **B1 — the dropped guard, named (not smuggled).** Today `prep::mint_ca` (`prep.rs:626`)
  short-circuits `if cert.is_file() && key.is_file() { return }` — an invariant documented
  at `prep.rs:618-619` ("a second `weles up` must not rotate the CA under a running
  fleet"). `edgeca` itself is NOT mint-if-absent — `edgeca/src/lib.rs:6-31` always
  `DevCA::generate()` + `atomic_replace`. So a generic runner CANNOT reproduce the Rust
  short-circuit. **Decision (user): drop it — regenerate the CA every `up`.** Safe and
  in fact more correct: the dev CA is ephemeral per `weles up` session (should not
  survive teardown), `rollout.lock` serialises ups (no second `up` under a running
  fleet), and `run_prepare` runs once per `run_up` (not per crash-restart). This is a
  deliberate reversal of the `prep.rs:618-619` invariant — recorded here and in the
  `weles-design.md` errata (Fix-the-Authority rule 4). No `edgeca` change needed;
  `tools/edgeca` is untouched, so the "no other crate affected" scope holds.

  **CA path contract (M3 — written down, not left to "operator will match").** The
  `edgeca` `[[prepare]]` hook writes `run/weles/edge-ca.crt` / `.key` (relative to
  `cwd=layout.root`); every service that dials the edge sets `EDGE_CA_CERT` /
  `EDGE_CA_KEY` = `run/weles/edge-ca.crt` / `.key` in its **`[service.env]`** (a literal
  RELATIVE path — NOT passthrough: weles's own env does not contain these keys, the CA is
  minted at runtime); services spawn with `cwd = layout.root` (`supervisor.rs:1308`) so
  the relative path resolves. `weles-managed-gateway`'s `swap_probe` already reads
  `ctx.root.join("run/weles/edge-ca.{crt,key}")` (`weles_managed_gateway.rs:241-242`), so
  all three coincide on exactly that path. This removes the `gateway-svc` special-case.
- **D-PARITY:** DELETE `weles-fleet-parity`. Fold its *topology-generic* value (unique
  ports, peers reference a declared provider, boot-order sanity) into weles's own
  `fleet.toml` validation. Do NOT fold its pg-budget check (dropped, per #4). Do NOT
  repoint it to compare against processctl (re-enforces the cross-world consistency the
  user rejected; the comparator is incoherent against TOML data).
- **D-GATEWAY:** KEEP `weles-managed-gateway` (the managed-mode interop proof — boots
  the real weles fleet, proves resolved addresses are actually dialed). REPOINT its
  three `split_fleet()` call sites to read ports/peers from `weles/fleet.split.toml`.
  With D-PREPARE, the stage's `weles up` self-provisions CA/admin via the hooks, so NO
  verifyctl pre-step is needed.
- **D-FLAVOR:** weles ships ONE dev `fleet.toml` per topology (split, monolith) — two
  complete files, no layering. The `FleetFlavor::Proof` variation (`SCHEDULER_ENABLED`)
  is splitproof/processctl's concern; weles's split fleet is the Development flavor and
  already omits it (`manifest.rs:303-306`), so weles needs no flavor logic.

### Commit & green boundaries (reviewer M2 — no fiction of N independent green commits)

`tools/verifyctl` depends on the `weles` crate, and the owned-`ServiceDef` +
domain-strip changes ripple into `weles_fleet_parity.rs` (which constructs `ServiceDef`
and calls the deleted symbols) and into weles's own tests. Therefore:

- **Step 5a (delete `weles-fleet-parity`) FIRST** — green on its own (weles untouched);
  removes the biggest external consumer of the doomed symbols. Own commit.
- **Step 2 (`fleet_toml.rs`) can be its OWN commit** (reviewer m1): once Step 1's owned
  `ServiceDef` lands, the new module + `toml` dep + parser/validator + its own unit tests
  build and test green while nothing references it yet.
- **Step 5b (managed-gateway repoint) is its own commit**, after Step 4.
- **The irreducible atomic core is Steps 1 + 3 + 4** — `cargo test -p weles` is red
  between them (weles's own tests reference `split_fleet`/`RuntimeInputs`/`compose_env`
  until Step 4). Land them as one verified unit; Steps 2, 5a, 5b, 6 are separate commits.
- **Step 6 (docs)** is a separate green commit.

---

## Research basis (facts cited from source, this session)

- `ServiceDef` (`manifest.rs:196-233`) — `&'static str` fields; `name, pkg, provider:
  Option, http_port, edge_port: Option, player_port: Option, has_db, pool_max, addrs:
  Addrs, env_extra: &'static [(…)]`.
- `Addrs` (`:159-179`) — `Told(&'static [(env_key, provider, AddrKind)])` | `Asks`
  (gateway-svc only, `:401`). Derives `Copy` today — CANNOT after `Told` becomes `Vec`
  (reviewer m1). `AddrKind` (`:134-142`) — `Edge`|`Http`, already `Serialize/Deserialize`.
- `RuntimeInputs` (`:237-241`). Composition: `compose_env_with_fleet` (`:599-663`),
  `compose_env`+`home_fleet` (`:684-703`, goldens-only panic path — drop), `service_addr`
  (`:469-475`), `peer_addr` (`:485-501`), `PeerAddrs` (`:551-585`, `.entries:
  Vec<(&'static str,…)>` at `:538` ripples to owned).
- DB/CA injection lives at `manifest.rs:617-634` (has_db block `:617-624`, gateway-CA
  else-branch `:625-634`). PG budget: consts `:793,797,803,809`; `service_pg_budget`
  (`:815-828`), `fleet_pg_budget` (`:837-856`), `validate_pg_budget` (`:858-864`).
- `prep::mint_ca` (`:622`, launches `edgeca`), `prep::seed_admin` (`:690`, launches
  `adminctl`), `SEED_ADMIN_TIMEOUT` (`:78`); `deploy_packages()` (`:240-251`,
  split∪monolith∪edgeca∪adminctl); `DEFAULT_DATABASE_URL` (`:72-73`), `database_url()`
  (`:232-233`).
- `Topology` enum: `cli.rs:6-10`; `up` arm `cli.rs:48-81` (preserve `BORROWED_LEASE_ARG`
  at `:65`). Fleet flow: `run_up(topology)` (`supervisor.rs:629`), the mint/db/seed +
  `RuntimeInputs` block (`:735-745`), `match topology` at `:725-728`/`:746-749`,
  `SpawnCtx.defs` (`:924-928`), `spawn_service`→`compose_env_with_fleet` (`:1302-1309`).
  Display name `:678-681`; `FleetState.topology: String` (`state.rs:104`; also read in
  the pre-bind message `main.rs:135`); `control.rs` messages `:164,172,238,245,261`.
- Deploy: `deploy()` (`prep.rs:316-417`), copy loop `:351-375`, `GenerationManifest`/
  `Artifact` (`:214-228`), manifest write `:393-398`, `flip_current` `:400`.
  `Layout::discover` pins `active_bin_dir` once (`:115-124`); read `fleet.toml` right
  after `pin_generation` (`:117`).
- `core/edge/src/tls.rs:353-368` — `dev_ca_from_env()` does NOT fail on absent
  `EDGE_CA_CERT/KEY`; it silently generates an *ephemeral, unshared* CA → cross-process
  mTLS is REJECTED. **This is why the CA must actually be provisioned (D-PREPARE), not
  merely optional.**
- Serde: no `toml` dep yet; `deny_unknown_fields` pattern in `agentapi.rs:605-629`.
- Tests: `manifest_tests.rs` (~19 keyed on `split_fleet()`/`monolith()`), `cli_tests.rs`
  (5 `up split/monolith`), `agentapi_tests.rs:22` (imports `split_fleet`/`monolith`),
  `manifest_tests.rs:5-6` (imports `RuntimeInputs`/`compose_env_with_fleet`). Per-file
  `*_tests.rs`, `OnceLock<Mutex>` env guards.
- External blast radius: ONLY `tools/verifyctl` (`Cargo.toml:13`). Break sites:
  `weles_fleet_parity.rs:374,390` + tests `:201,207`; `weles_managed_gateway.rs:219,697,
  952` + `up split` at `:250` + tests `:277`. Stage registration: `model.rs:22-25,49-52`;
  `stages/mod.rs:130-163` (all four BLOCKING); frozen-manifest test `mod.rs:361-373`
  includes the name list, the `[16..]` advisory-slice index (`:369`), and `len()==21`
  (`:373`). `weles-async-island`/`weles-wire-contract` untouched.

---

## Ordered steps

### Step 5a (done FIRST) — verifyctl: delete `weles-fleet-parity`. `[opus]` (core-implementer)

**(a) What:** files `stages/weles_fleet_parity.rs` + `_tests.rs`; `StageId::WelesFleetParity`
variant + `name()` arm (`model.rs:22,49`); `pub mod` decl + `Stage{…}` registration
(`stages/mod.rs:130-135`); in the frozen-manifest test (`mod.rs:361-373`) remove the
`"weles-fleet-parity"` name entry, shift the advisory slice index `[16..]`→`[15..]`
(`:369`), and drop `len()==21`→`20` (`:373`); fix the two prose refs
(`weles_async_island.rs:6-10`, `weles_managed_gateway.rs:73`). Keep the `weles` crate dep
(3 other stages use it).

**(b) Why FIRST / order:** it is green on its own (weles untouched) and removes the
biggest external consumer of the symbols Steps 1–4 delete, shrinking the atomic red
window to just weles's own tests + `weles-managed-gateway`.

**(c) How:** pure deletion + one slice-index arithmetic fix; verify `cargo test -p
verifyctl` green after.

**(d) Dispatch:** `[opus]` — core-implementer (touches a verify gate; `proof-auditor`
in Step 6 audits that no coverage silently vanishes).

---

### Step 1 — Strip domain KNOWLEDGE; generalize prepare hooks; owned `ServiceDef`/`Addrs`; passthrough env. `[opus]` (core-implementer)

**(a) What:** `weles/src/manifest.rs`, `weles/src/prep.rs`, `weles/src/supervisor.rs`.
- Convert `ServiceDef` to owned and **drop the domain fields**: keep `name/pkg: String`,
  `provider: Option<String>`, `http_port`, `edge_port/player_port: Option`, `addrs:
  Addrs`, `env: BTreeMap<String,String>` (was `env_extra`); **remove `has_db`/`pool_max`**.
  `Addrs::Told(Vec<(String,String,AddrKind)>)` — `Addrs` LOSES `Copy` (ripples to the
  `match svc.addrs` at `:644`, `.told()`'s `&'static` return `:185-190`, `PeerAddrs.entries`
  `:538` → owned/by-ref, AND the `PeerAddrs` consumers in `agentapi.rs:304,316,424,635`
  incl. `Arc<PeerAddrs>` — verify the `lookup`/`resolve` paths still compile against `&str`).
- DELETE domain knowledge: PG-budget consts + `service_pg_budget`/`fleet_pg_budget`/
  `validate_pg_budget` (`:793-864`); `prep::database_url()` + `DEFAULT_DATABASE_URL`
  (`:72-73,232-233`); `RuntimeInputs` entirely (`:237-241`); the DB/CA injection block
  incl. the `gateway-svc` special-case (`:617-634`); `compose_env`+`home_fleet` (goldens
  panic path, `:684-703`). `compose_env_with_fleet` loses its `inputs` param and reduces
  to: passthrough → `PORT`/`EDGE_ADDR` → peers/`ORCHESTRATOR_URL` → service `env` (last).
- **REFACTOR `mint_ca`+`seed_admin` into `run_prepare(&[PrepareCmd], &Layout)`** (D-PREPARE)
  — do NOT delete the capability. Remove: the typed `CaPaths` return, the `database_url`
  argument, the HARDCODED binary names/argv (→ manifest `[[prepare]]`), AND the
  file-existence short-circuit in `mint_ca` (`prep.rs:626` — the dropped guard, B1). Keep
  the generic machinery: `cwd = layout.root` (`prep.rs:709`), per-command deadline
  (`timeout_secs`, default 30), `run_dir/<name>.{out,err}.log` logging, nonzero-exit/
  timeout = abort. `run_prepare` applies each `PrepareCmd`'s `passthrough` (env keys
  forwarded from weles's own env) + `env` — so the `adminctl` hook can receive
  `DATABASE_URL` via passthrough (reviewer M2).
- REPLACE `SERVICE_ENV_ALLOWLIST` const (`:52-65`) with a per-fleet **passthrough list**
  (env KEYS forwarded from weles's own environment), sourced from `fleet.toml` (Step 2);
  keep a minimal always-on floor (PATH/HOME/…) needed to exec at all.
- `supervisor.rs`: the `mint_ca`/`database_url`/`seed_admin`/`RuntimeInputs` block
  (`:735-745`) is replaced by a `run_prepare(fleet.prepare)` call in Step 3;
  `validate_pg_budget` (`:735`) is removed here.

**(b) Why now / order:** downstream depends on the FINAL owned, domain-free shape and on
the generalized `run_prepare` signature. `split_fleet()`/`monolith()` are TEMPORARILY
kept (rewritten to owned, `env`-map form) so `cargo build -p weles` is green at step end
(its TESTS stay red until Step 4 — see Commit boundaries). The two real peer-list
literals become `vec![(...to_string()...)]`; former injected env folds into `env`/passthrough.

**(c) How:** env-composition ORDER (passthrough → ports → peers → service `env` last, so
operator env wins) is invariant. `AddrKind` stays `Copy`; `Addrs` cannot. Hand-format to
house style (no `cargo fmt`, memory `cargo-fmt-is-not-safe-here`).

**(d) Dispatch:** `[opus]` — core-implementer (deletes several decision authorities +
generalizes one; authority-first. Prove: `compose_env_with_fleet` injects NO
`DATABASE_URL`/`EDGE_CA_*`/`DATABASE_POOL_*` unless the operator put them in
`env`/passthrough — the failing branch of the old injection.)

---

### Step 2 — `fleet.toml` schema + strict parser + validation. `[opus]` (core-implementer)

**(a) What:** new `weles/src/fleet_toml.rs` (+ `fleet_toml_tests.rs`); add `toml` dep.
- `#[serde(deny_unknown_fields)]` structs:
  `FleetToml { passthrough: Vec<String> (default []), prepare: Vec<PrepareCmd> (default
  []), service: Vec<ServiceEntry> }`;
  `PrepareCmd { name: String, run: String, args: Vec<String> (default []), passthrough:
  Vec<String> (default []), env: BTreeMap<String,String> (default {}), timeout_secs: u64
  (default 30) }`;
  `ServiceEntry { name, pkg, provider: Option, http_port, edge_port: Option, player_port:
  Option, resolve: Option<"asks">, peer: Vec<PeerEntry> (default []), passthrough:
  Vec<String> (default []), env: BTreeMap (default {}) }`;
  `PeerEntry { env_key, provider, kind: AddrKind }`. `resolve="asks"` ⇒ `Addrs::Asks`;
  else `Addrs::Told(peer…)`. NO `has_db`/`pool_max`.
- `fn load(path) -> Result<(Vec<PrepareCmd>, Vec<String>, Vec<ServiceDef>)>` (or a small
  `Fleet` struct): parse + convert to owned.
- `fn validate(fleet) -> Result<()>` — the folded topology-generic value: (i) http/edge/
  player ports unique across the fleet AND distinct from `AGENT_PORT`; (ii) every
  `Addrs::Told` peer's `provider` names a service that exists AND serves that `AddrKind`
  (reuse `service_addr` returning `None` = fail); (iii) boot-order: every `Edge` peer's
  provider appears earlier in the Vec than its consumer (`manifest.rs:12-17`). NO
  pg-budget. `[[prepare]]` order = run order (CA before admin/gateway). **No parse-time
  "`prepare.run` is a staged pkg" check** (reviewer M1: it would be circular —
  `deploy_packages` is *defined* to include `prepare.run` — and unverifiable at parse,
  since staging happens at `weles deploy`; the real guard is the deploy-time
  missing-source error `prep.rs:355-359` + `validate_binaries` `prep.rs:259`).

**(b) Why now / order:** the parser targets the Step-1 owned shape; self-contained, so
built+unit-tested before the supervisor is rewired (Step 3).

**(c) How:** mirror `agentapi.rs:605-629`'s `deny_unknown_fields`. `validate` reuses
`service_addr`/`PeerAddrs` — no duplicated resolution. Failing-branch tests: dup port,
peer→absent provider, `Edge` on `edge_port=None`, out-of-order edge peer, `prepare.run`
naming a non-staged pkg — each a distinct `Err`.

**(d) Dispatch:** `[opus]` — core-implementer (new parse+validate authority replacing a
deleted blocking gate; prove each failing branch).

---

### Step 3 — Wire `deploy --fleet` / `up` + `run_prepare`; delete `Topology`; `deploy_packages` from the fleet. `[opus]` (core-implementer)

**(a) What:** `weles/src/prep.rs`, `supervisor.rs`, `cli.rs`, `main.rs`, `state.rs`.
- `cli.rs`: delete `enum Topology` (`:6-10`); `Command::Up { topology }` →
  `Command::Up { dry_run: bool }`; `up` arm parses `--dry-run` (keep `BORROWED_LEASE_ARG`
  at `:65`); add `--fleet <path>` to the `deploy` arm; update `USAGE`.
- `prep.rs`: `deploy(src, fleet_path)` copies the chosen `fleet_path` into `gen-N/` as
  `fleet.toml` (hash it into `GenerationManifest` — extend `Artifact`/add a field) BEFORE
  `flip_current`, so a missing/failed fleet aborts the flip like a missing binary.
  `deploy_packages()` (`:240-251`): staged set = the `[[service]]` `pkg`s **∪** the
  `[[prepare]]` `run`s (still derived from `fleet.toml`, not hardcoded — `edgeca`/`adminctl`
  stay staged because a hook references them). `Layout::discover` reads+pins
  `deploy/current/fleet.toml` once after `pin_generation` (`:117`) into a new `Layout`
  field (PIN-AT-DISCOVER).
- `supervisor.rs`: `run_up()` drops `topology`; `defs` + `prepare` + `passthrough` come
  from `Layout`'s pinned fleet. **After the fleet pin, before the spawn loop, call
  `run_prepare(fleet.prepare, &layout)` sequentially; abort on the first nonzero exit
  (before taking anything to spawn)** — this occupies the slot of the old
  `mint_ca`/`seed_admin` block (`:735-745`). Replace both `match topology` blocks
  (`:725-728` packages, `:746-749` defs). Display name (`:678-681`) → derived from the
  fleet (process count / file label); `Reporter.topology`/`FleetState.topology`
  (`state.rs:104`, `main.rs:135`) → a plain fleet label.
- `main.rs`: `up()` no longer takes `Topology`; `--dry-run` parses+validates the deployed
  `fleet.toml` (incl. `[[prepare]]` names/`run` existence) and returns BEFORE
  `supervisor::run_up` — **must NOT take the rollout lock, run hooks, or spawn**
  (side-effect-free). `control.rs`: reword split/monolith status messages.

**(b) Why now / order:** needs Step-2 loader/validator. Flips weles onto the file.

**(c) How:** `run_prepare` runs at every `up` (idempotent hooks). `--dry-run` validates
`[[prepare]]` but does NOT run it. Preserve the `defs`-is-the-booting-fleet invariant
(`supervisor.rs:750-753`): peers resolve against the loaded fleet, never a re-loaded one.

**(d) Dispatch:** `[opus]` — core-implementer (deploy↔up + prepare-before-spawn ordering
— core-failure-taxonomy surface). Prove: a `PrepareCmd` with nonzero exit aborts `up`
BEFORE any service is spawned (failing branch, on a real 2-service fleet).

---

### Step 4 — Ship `fleet.toml` fixtures; delete `split_fleet()`/`monolith()`; rebase weles tests. `[opus]` + `[sonnet]`

**(a) What:**
- Add `weles/fleet.split.toml` (12 services) and `weles/fleet.monolith.toml` (single
  `server`). Transcribe from the current `split_fleet()`/`monolith()` tables, and
  **explicitly materialize the formerly-INJECTED env** (reviewer M4): per-service
  `DATABASE_POOL_MAX_CONNECTIONS` = `3` (split) / `20` (monolith) in `[service.env]`;
  `DATABASE_URL` via fleet-level `passthrough` (operator/verifyctl sets it in weles's env);
  `EDGE_CA_CERT`/`EDGE_CA_KEY` = the RELATIVE `run/weles/edge-ca.crt`/`.key` in
  `[service.env]` — NOT passthrough (per the D-PREPARE CA-path contract: weles's env has no
  CA key, the hook mints it at runtime). Keep existing dev-flags (`ACCOUNTS_DEV_AUTH`,
  `INVENTORY_DEV_GRANT`, `APIKEYS_DEV_SEED`, `ADMIN_COOKIE_SECURE`, `TRUSTED_PROXY_CIDRS`,
  gateway's `PLAYER_EDGE_ADDR`/`TLS_MODE`).
  Add `[[prepare]]`, CA-first: `edgeca` (writes `run/weles/edge-ca.{crt,key}`), then, since
  the split hosts admin-svc, `adminctl` with `passthrough=["DATABASE_URL"]` +
  `env={ADMINCTL_PASSWORD="admin"}` (reviewer M2 — the DSN the seed needs) and its argv in
  `args`. Monolith fixture likewise.
- DELETE `split_fleet()`/`monolith()`. **DROP `validate_disk`/`validate_names`**
  (`:753-785`) and their tests (decided, reviewer m3): a `cmd/*-svc`-vs-canonical drift
  check is meaningless once the fleet is an arbitrary operator file.
- Rebase `manifest_tests.rs` (~19), `cli_tests.rs` (5), `agentapi_tests.rs` (`:22`): load
  the fixtures via `fleet_toml::load` instead of the deleted fns. Delete the now-meaningless
  `compose_env_refuses_a_def_from_no_real_manifest`, the real-fleet pg-budget tests, and the
  `validate_disk` tests. Keep synthetic-fleet unit tests (adjust `ServiceDef` literals to owned).
- **`deploy_packages()` signature change ripples into ~9 test call sites** (reviewer M4):
  `prep_tests.rs:168,192,258,307,428,439` and `tests/prep.rs:35,59,199` call it with no
  fleet arg today; rebase them to the fleet-derived form (and `manifest_records_the_sha256_
  of_each_staged_artifact`'s count assumption, `tests/prep.rs:199`).

**(b) Why now / order:** fixtures must exist before the fns are deleted (tests + Step-5b
repoint onto them). This is the last weles-internal removal.

**(c) How:** the fixture is the single transcription of the `split_fleet()` table (Step-1
research holds the 12-row port/peer/env data); cross-check against the deleted fn in the
same diff. Accept NO runtime pg-sum-check (dropped): the per-service caps in the fixture
bound the pools; processctl/split-proof guard the dev split independently. Test rebase is
mechanical once `load_split_fixture()`/`load_monolith_fixture()` helpers exist.

**(d) Dispatch:** fixtures + `validate_disk` + `split_fleet` deletion `[opus]`; the
~30-site test rebase `[sonnet]` from the helper pattern.

---

### Step 5b — verifyctl: repoint `weles-managed-gateway` to the fixture. `[opus]` (core-implementer)

**(a) What:** replace the three `split_fleet()` calls (`weles_managed_gateway.rs:219,697,
952`) and the test at `:277` with `weles::fleet_toml::load("weles/fleet.split.toml")` (now
public). **Change the spawn args `["up","split"]` → `["up"]`** (`:250`, reviewer M1) — the
new parser has no topology token; the deployed fleet is selected at `weles deploy --fleet`.
The stage already runs `weles up`, which now runs the `[[prepare]]` CA/admin hooks, so the
shared CA + dev admin exist WITHOUT any verifyctl pre-step (D-PREPARE closes old open-item #2).

**(b) Why now / order:** AFTER Step 4 (fixture + `fleet_toml::load` exist). Part of the
atomic rollout (it's red until here).

**(c) How:** `managed-gateway` already builds owned "views"; a `fleet_toml::load` `Vec`
is a drop-in for `split_fleet()`. Confirm the `weles deploy` the stage runs
(`:202-214`) is given `--fleet weles/fleet.split.toml`, and that `swap_probe`'s CA path
(`:241-242,736-743`) matches where the `edgeca` `[[prepare]]` hook writes. Keep the
stage's value (resolved-address-is-dialed proof); no processctl comparison.

**(d) Dispatch:** `[opus]` — core-implementer (verify gate; `proof-auditor` in Step 6).

---

### Step 6 — Docs + review. `[sonnet]` docs, session review.

**(a) What:** `docs/reference/weles-design.md` (errata: manifest → operator `fleet.toml`
+ `[[prepare]]` hooks; refine the `:79` "spawns from a manifest" claim; domain knowledge
removed but prepare-capability kept; `up` takes no topology arg);
`docs/reference/weles-fleet-parity.md` (stage removed, premise gone); `README.md` weles
section + `weles/README.md` (`weles up`, `--dry-run`, `deploy --fleet`, the `fleet.toml`
+ `[[prepare]]` model); `CLAUDE.md` weles paragraph if it names `split_fleet`/topology.

**(b) Why now:** docs-current is a blocking verify stage.

**(c) Review:** one `core-reviewer` pass over the whole rollout, class-keyed; ADD
`proof-auditor` (Step 5a/5b touch a verify gate; Steps 2/4 replace a blocking gate) —
audit that `fleet_toml::validate` failing branches actually execute, that `run_prepare`'s
abort branch is proven, and that deleting `weles-fleet-parity` drops no coverage nothing
else has.

**(d) Dispatch:** docs `[sonnet]`; review `core-reviewer` + `proof-auditor` at `model:` ≥
implementer tier.

---

## Verification

Per `safe-verification` (one rollout at a time, shared Postgres): after the atomic rollout,
`cargo test -p weles` then `cargo test -p verifyctl` (sequential). Full gate:
`cargo run -p verifyctl -- --fast` (ONLY rollout running) — expects **15/16** blocking
stages (fleet-parity removed), `weles-managed-gateway` green booting the file-driven fleet
(its own `[[prepare]]` hooks minting the CA + seeding admin), split-proof unaffected (runs
processctl, not weles). `run_prepare` carries a failing-branch test: a hook exiting nonzero
aborts `up` before any service spawns.

## Open items (remaining after two review passes)

1. `fleet.toml` schema: `resolve="asks"` for `Addrs::Asks` vs inferring it — but the
   `kind = "edge"|"http"` field MUST stay typed, never parsed from an env-key name.
2. Is a fleet with no DB-backed service (no `DATABASE_URL`) actually reachable/handled,
   or does something still assume a DB exists? (Still relevant after budget removal.)

**Resolved in v3:** anti-magic of `[[prepare]]` (accepted — opaque commands, same class as
`[service.env]`, zero layering); `validate_disk` (DROPPED, Step 4); CA-path consistency
(written down — D-PREPARE contract: `run/weles/edge-ca.{crt,key}`, cwd=root, `[service.env]`
literal relative, swap_probe matches).
