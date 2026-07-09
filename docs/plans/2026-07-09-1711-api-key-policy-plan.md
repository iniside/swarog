# API key policy — plan

Feature: every request through the gateway carries an **API key** identifying the
client class; each key has a **policy** (allowed method list or `full`) stored in
DB and editable at runtime. Untrusted clients (the game build) ship a restricted
key; trusted servers get a `full` key. No per-user RBAC. Session bearer auth is
unchanged and orthogonal (player ops still require it *in addition to* the key).

Decision history: `docs/design/2026-07-09-1652-api-access-control-options-summary.md`
(options + user decisions). Research: 3 + 6 parallel subagents (gateway/edge
enforcement, accounts identity, config/admin patterns; then rpc-macro/ops
inventory, gateway dispatch detail, accounts/split templates, C# client, dev-flow
blast radius, admin page contract).

## Context — what exists, and why a new module

- **Enforcement point exists and is single**: both planes funnel through the
  gateway — `dispatch_matched_op` (HTTP, `modules/gateway/src/lib.rs:572`, has
  `HeaderMap` + `Operation` in scope) and `handle_player` (player-QUIC, `:280`,
  has `route.op` in scope). Error helpers exist: `error_response(StatusCode, msg)`
  (HTTP, plain-text body) and `front_envelope(Status, msg)` (player plane;
  `opsapi::Status::Forbidden` already exists → 403).
- **No opsapi/rpc-macro changes needed.** The user's model puts ALL policy on the
  key (method allow-list or `full`); ops declare nothing new. `AuthReq::{None,Player}`
  and `Identity` stay as-is — a key allows a *method*, a session still authorizes
  the *player* where `AuthReq::Player` demands it. (The earlier "audience field on
  Operation" idea is dropped: superseded by the per-key policy decision. A new op
  is safe by default because it's absent from every restricted key's list.)
- **Why not extend `accounts`**: keys are app/class credentials, not player
  identity; accounts' schema is player-focused (players/identities/sessions) and
  its module doc pins it to "own schema, player identity only". A sibling fortress
  keeps accounts untouched and lets the key store be deployed/split independently
  — exactly how `config`/`audit` relate to `accounts` today.
- **Why not extend `config`**: `config.settings` is scalar string KV; a per-key
  policy is a keyed record with lifecycle (create/revoke), not a knob. We copy
  config's *shape* (module + api crate + admin item), not its table.
- **Why not `httpmw::LAYER_SLOT`**: sits outside route→op resolution (gateway
  dispatches via fallback; no `Operation` in scope) and never touches player-QUIC.
- **Split reality check**: gateway-svc has **no DB ⇒ no durable-events plane ⇒
  `on_tx` cache invalidation is impossible there**. So no `apikeysevents` crate,
  no LISTEN/NOTIFY, no CachedX snapshot. Instead: per-request capability call
  (`apikeysapi::Keys`, same seam as `accountsapi::Sessions`) behind a **small TTL
  cache in the gateway module** (5 s). Revocation/policy edits propagate within
  ≤5 s in both topologies. This is the sessions-verifier template, not the
  config-cache template.

## Decisions (fixed for this plan)

1. **Transport**: HTTP → `X-Api-Key` header; player-QUIC → new optional
   `api_key` field on the `PlayerRequest` envelope (serde `default` +
   `skip_serializing_if`). NOTE the honest framing: serde-compat only means a
   pre-change client gets a clean **401**, not a parse error — every old caller
   is *functionally* broken by design until it sends a key. That's the point of
   the feature, but don't read "additive" as "old game builds keep working".
2. **Scope of enforcement**: only op-dispatched requests (RouteTable hits), on
   both planes. NOT key-gated (documented non-goals): `/healthz`, `/readyz`,
   `/metrics`, `POST /events`, webui static `/`, and the reverse-proxy
   passthroughs (`/admin`, `/accounts/epic` — they never reach `dispatch_matched_op`;
   admin keeps its Basic auth). `wait_healthy` in the harness keeps working.
3. **Storage**: new schema `apikeys`, table
   `apikeys.keys(name text PRIMARY KEY, key text UNIQUE NOT NULL, policy text NOT NULL,
   created_at timestamptz NOT NULL DEFAULT now(), revoked_at timestamptz)`.
   **Plaintext key storage** — same trust model as `accounts.sessions.token`
   (plaintext bearer, equality lookup); lets the admin page display keys. Hashing
   at rest = future hardening, out of scope.
4. **Policy format**: the string `full`, or a comma-separated list of wire method
   names (e.g. `accounts.login,characters.create`). Gateway evaluates:
   `policy == "full" || policy.split(',').any(|m| m.trim() == op.method)`.
   Revoked key (`revoked_at IS NOT NULL`) behaves as unknown.
5. **Error shapes**: HTTP — missing header → 401 `"missing api key"`; unknown/
   revoked → 401 `"invalid api key"`; policy denies → **403** `"api key policy
   forbids this operation"` (via existing `error_response`). Player plane — same
   three cases as `front_envelope(Status::Unauthorized, …)` ×2 and
   `front_envelope(Status::Forbidden, …)`. Key check runs AFTER route/method
   match and BEFORE session auth — on BOTH planes. (Player plane: after
   `find_by_method`, so an unknown method stays `NotFound` — split-proof P5 and
   verify C3 assert NotFound for `characters.ownerOf` and must keep passing;
   HTTP is post-match by construction inside `dispatch_matched_op`.)
6. **Capability + fallback**: gateway resolves
   `registry::key("apikeys", "keys")` at `init` (mirroring
   `resolve_verifier`); if absent, `APIKEYS_DEV_ALLOW` **explicitly** truthy →
   allow-all verifier with loud warn; else startup failure with a fix-it message.
   No `requires("apikeys")` on gateway (same posture as accounts today: resolved
   with dev escape hatch, not manifest-declared).
7. **Dev seed**: `modules/apikeys::migrate` seeds ONLY when `APIKEYS_DEV_SEED`
   is **explicitly** truthy (default OFF — a well-known `full` key is a trust
   artifact, so it follows the gateway's explicit-only convention
   (`dev_auth_explicitly_on`, `verifier.rs:133-141`), NOT the module-convenience
   default-ON one; loud warn when seeding). The seed is **self-healing**:
   `INSERT … ON CONFLICT (name) DO UPDATE SET key = EXCLUDED.key,
   policy = EXCLUDED.policy, revoked_at = NULL` — so a stray revoke on a shared
   dev DB can't permanently poison the harness. `run.sh`, `split-proof.*` and
   the verify csharp stage export `APIKEYS_DEV_SEED=1` themselves. Seeded keys:
   - `dev-client` / key `dev-key-client` / policy = the player-facing list:
     `accounts.register,accounts.login,accounts.loginEpic,accounts.me,`
     `characters.create,characters.list,characters.delete,`
     `inventory.grant,inventory.listMine,inventory.listCharacter,`
     `leaderboard.topScores` (exact strings taken from the generated METHOD
     consts at implementation time — the list above matches the C# generated
     client's wire names).
   - `dev-server` / key `dev-key-server` / policy = `full`.
   `match.report` is deliberately NOT in `dev-client` — it's the trusted-server
   op, which gives the harness a real negative case.
8. **Ports**: apikeys-svc HTTP `:8091`, edge `:9009`
   (`APIKEYS_EDGE_ADDR=127.0.0.1:9009` consumed by gateway-svc + admin-svc).
9. **TTL cache**: in the gateway module, wrapping whichever `Keys` impl was
   resolved: `Mutex<HashMap<String, (Option<KeyRecord>, Instant)>>`, TTL 5 s.
   Caches `Ok(Some)` AND `Ok(None)` (bounds DB/edge chatter under bad-key spam)
   but **never caches on `Err`** — an apikeys-svc blip must not poison a valid
   key as 401 for a whole TTL (the `Err→None` response collapse still applies
   per-request, logged `error!`, just not cached). **Bounded**: on insert when
   `len() >= 10_000`, clear the map (crude, O(1) amortized, immune to
   distinct-garbage-key memory growth).

## Steps

### Step 1 — `api/apikeys` contracts + `modules/apikeys` fortress  `[opus]` (effort: medium)

**What**: new crates `api/apikeys/api` (`apikeysapi`), `api/apikeys/rpc`
(`apikeysrpc`), `modules/apikeys`; workspace `Cargo.toml` members.

**Why first**: everything else (gateway, cmd roots, admin) imports these.

**How**:
- `apikeysapi`: `KeyRecord { pub name: String, pub policy: String }`
  (Clone/Debug/Serialize/Deserialize/PartialEq/Eq);
  `#[rpc(prefix = "apikeys")] #[async_trait] pub trait Keys: Send + Sync {
  async fn lookup_key(&self, key: String) -> Result<Option<KeyRecord>, Error>; }`
  — wire-only (no `#[http]`), mirrors `accountsapi::Sessions::verify_session`
  exactly (returns `Ok(None)` for unknown/revoked, `Err` only for infra).
- `apikeysrpc`: `apikeysapi::apikeys_keys_meta!(rpc_macro::generate_glue);`
  `pub use adminrpc::register_admin;` and
  `pub fn remote_factories() -> Vec<remote::RemoteFactory>` providing
  `keys_rpc::provide_remote(ctx.registry(), caller)` (copy the sessions arm of
  `accountsrpc::remote_factories`, `api/accounts/rpc/src/lib.rs:43-54`).
- `modules/apikeys`: `lifecycle::Module` named `apikeys`; `SCHEMA_DDL` const with
  `CREATE SCHEMA IF NOT EXISTS apikeys;` + the `keys` table (Decision 3),
  idempotent like `modules/accounts/src/lib.rs:45-70`; `store.rs`
  (`Store { pool }`, `lookup(key) -> Option<KeyRecord>` via
  `SELECT name, policy FROM apikeys.keys WHERE key = $1 AND revoked_at IS NULL`,
  `list()`, `insert(name, key, policy)`, `set_policy(name, policy)`,
  `revoke(name)`); `register` provides `Arc<Service>` under
  `registry::key("apikeys", "keys")`; `migrate` runs DDL + dev seed (Decision 7,
  private `env_bool` helper copied from accounts, `tracing::warn!` when seeding);
  `init` contributes `edge::EDGE_SLOT` → `apikeysrpc::keys_rpc::register_server`
  (admin face added in Step 6). Tests in `src/tests.rs` + `src/store_tests.rs`
  against local Postgres (existing convention): seed-idempotency, lookup
  known/unknown/revoked, policy CRUD, seed self-heal (revoke `dev-client`, re-run
  migrate with seed on, assert un-revoked). **Test fixtures use test-only key
  names (`test-…` prefix), never `dev-client`/`dev-server`** — the shared local
  Postgres must not have the harness's dev rows poisoned by tests.
- Add `apikeysapi` to `PUBLIC_API_CRATES` in `verify.sh`/`verify.ps1` (line ~84).

### Step 2 — envelope + gateway enforcement  `[fable]` (effort: high)

**What**: `core/edge/src/player.rs`, `modules/gateway/src/{lib.rs,verifier.rs or
new keys.rs}`, `modules/gateway/src/tests.rs`, `tools/playercli/src/main.rs`,
PLUS — so the workspace stays bootable at every step — `cmd/server/src/main.rs`
(add `Box::new(apikeys::ApiKeys::new())`) and
`tools/checkmodules::monolith_modules()` (add `apikeys`): once `Gateway::init`
bails without the capability, the monolith AND the `topiccheck`/`requirecheck`
harnesses (which build the monolith module list) would otherwise be red until
Step 3.

**Why now**: the correctness-critical seam; depends on Step 1.

**How**:
- `core/edge/player.rs`: add `api_key: Option<String>` to `PlayerRequest`
  (serde `default` + `skip_serializing_if = "Option::is_none"`, matching `token`);
  extend `PlayerHandler` from `Fn(String, Option<String>, Vec<u8>)` to also pass
  `api_key: Option<String>`; extend `PlayerClient::call(method, token, api_key,
  payload)`; update `player_tests.rs` construction/roundtrip tests.
- `tools/playercli`: `--api-key KEY` flag beside `--token`
  (`main.rs:37-67`), threaded into `client.call`.
- Gateway: new `keys.rs` module — `trait KeyVerifier { async fn lookup(&self,
  key: &str) -> Option<apikeysapi::KeyRecord>; }`; `RealKeyVerifier` wrapping
  `Arc<dyn apikeysapi::Keys>` + the 5 s TTL cache (Decision 9; `Err` from the
  capability logs `error!` and maps to `None`, same collapse as
  `SessionsVerifier`); `AllowAllKeyVerifier` (dev, `Once`-guarded loud warn);
  `resolve_key_verifier(ctx)` mirroring `resolve_verifier`
  (`verifier.rs:104-131`) with `APIKEYS_DEV_ALLOW` explicit-only escape and a
  fix-it bail message naming `remote::Stub::new("apikeys", …,
  apikeysrpc::remote_factories())`.
- Enforcement: in `dispatch_matched_op` before the `match op.auth` block — read
  `X-Api-Key` from `headers`, run the three-way check (Decision 5); in
  `handle_player` **after `find_by_method` succeeds** (unknown method stays
  `NotFound` — keeps split-proof P5 / verify C3 green) and before the auth
  block — same check on the new `api_key` param. Policy evaluation helper `policy_allows(policy: &str, method:
  &str) -> bool` (Decision 4) with unit tests (full / exact / trimmed / empty /
  unknown).
- Tests (`modules/gateway/src/tests.rs`): extend `demo_front_door` scaffold with
  a fake `KeyVerifier`; update ALL existing `.oneshot` requests (8 sites) and
  `call_player` sites to carry a valid demo key; new cases: HTTP missing key →
  401, unknown key → 401, denied method → 403, allowed passes; player-plane
  envelope equivalents (`{"status":"Forbidden", …}`); TTL-cache behavior
  (hit cached, expiry re-queries — inject a fake clock or accept a sleep-free
  design by testing the cache struct directly).

### Step 3 — composition roots + harness module lists  `[sonnet]` (effort: low)

**What**: `cmd/apikeys-svc/` (new), `cmd/gateway-svc/src/main.rs`,
`verify.sh` + `verify.ps1` fortress build list. (`cmd/server` + `checkmodules`
already done in Step 2 to keep the workspace bootable.)

**Why now**: the split topology must compile+boot before the harness sweep.

**How**: copy `cmd/config-svc` shape for `cmd/apikeys-svc`
(metrics + `apikeys::ApiKeys::new()`, HTTP `:8091`, edge `:9009`, no stubs, no
FrontDoor); add `Box::new(remote::Stub::new("apikeys", &env_addr("APIKEYS_EDGE_ADDR",
"127.0.0.1:9009"), apikeysrpc::remote_factories()))` to `cmd/gateway-svc`; add
`-p apikeys-svc` to the `fortress()` build list in **both** `verify.sh`
(~line 138-143) and `verify.ps1` (there is no "port list" in the fortress
stage — it's a `cargo build -p …` package list; ports live only in split-proof).

### Step 4 — harness + dev-surface sweep  `[sonnet]` (effort: medium)

**What**: `split-proof.sh` + `split-proof.ps1` (lockstep), `run.sh`,
`modules/webui/src/index.html`, `CLAUDE.md` smoke-test block.

**Why now**: Steps 2–3 made every existing HTTP/QUIC call fail without a key;
this step restores green and adds the named proof.

**How**:
- split-proof both variants: boot `apikeys-svc` (`:8091`/`:9009`, distinct
  `EVENTS_ORIGIN=apikeys-svc` — it has a DB so the durable plane boots, and the
  harness mandates distinct origins per process; export `APIKEYS_EDGE_ADDR` to
  gateway-svc AND to admin-svc's env block; export `APIKEYS_DEV_SEED=1` to
  apikeys-svc and the monolith run); add `-H "X-Api-Key: dev-key-client"` to
  every existing curl EXCEPT `match/report` calls which switch to
  `dev-key-server` (the client key must not carry `match.report`); playercli
  calls (P1–P5) gain `--api-key dev-key-client` (P5 stays `NotFound` — key
  check is post-match); monolith-parity section same treatment. New
  `[K1]–[K4]` block right after `[A5]`: K1 no key → 401, K2 bogus key → 401,
  K3 `dev-key-client` on `POST /match/report` → 403, K4 `dev-key-server` on
  the same → **202** (the op's real success code, per MT1).
  `/healthz`+`/metrics`+`/admin` assertions stay keyless (proves the non-goal
  carve-outs).
- `run.sh`: set `APIKEYS_DEV_SEED=1` (it's the dev-boot script; still
  overridable), boot apikeys-svc in split mode with distinct `EVENTS_ORIGIN`,
  print a note with the two dev keys.
- webui `index.html` `api()` helper (line ~98): add
  `opts.headers["X-Api-Key"] = "dev-key-client";` beside the bearer line
  (webui only calls accounts ops — covered by the client policy; `/accounts/epic/start`
  is passthrough, header harmless).
- `CLAUDE.md` smoke block: add `-H "X-Api-Key: dev-key-server"` to the
  `match/report` curl and `-H "X-Api-Key: dev-key-client"` to `leaderboard`.

### Step 5 — C# client + verify C-stage  `[sonnet]` (effort: medium)

**What**: `clients/csharp/Transport/{IPlayerTransport.cs,QuicPlayerClient.cs}`,
`clients/csharp/Program.cs`, `verify.sh` `csharp_stage()` AND `verify.ps1`'s
csharp stage (~line 292 onward — it fully mirrors; keep lockstep). Both stages
export `APIKEYS_DEV_SEED=1` for their self-contained monolith boot. Known
window: the csharp stage (advisory; blocking under `--strict`) is red from
Step 2 until this step lands.

**Why now**: this is the user's chosen live proof; needs Steps 2–4 semantics.

**How**: key is **connection-scoped** — add `string? apiKey` to
`QuicPlayerClient` construction/`ConnectAsync`, stamp `env["api_key"]` in
`CallAsync`'s envelope build (`QuicPlayerClient.cs:129-135`) when set; no
`IPlayerTransport.CallAsync` signature change and **no `csharp-client-gen`
change** (transport-level, generated typed client untouched). `Program.cs`:
`--api-key KEY` flag (pattern of `--token`, lines 229-231), used by both `raw`
and `flow` modes. `verify.sh` C-stage: thread `--api-key dev-key-client` into
C1–C4; new **C5**: `gbclient raw` with `dev-key-client` calling `match.report`
→ expect exit 1 + `Forbidden`; **C6**: same call with `dev-key-server` → Ok.
(This is the "test policy on the C# client" the user asked for.)

### Step 6 — admin page "API Keys"  `[opus]` (effort: medium)

**What**: `modules/apikeys/src/admin.rs` (+ `lib.rs` init wiring),
`cmd/admin-svc/src/main.rs`.

**Why now**: runtime configurability surface; independent of the harness, last
so review focuses on it separately.

**How**: mirror config's admin item (`modules/config/src/lib.rs:407-548`):
`adminapi::Item::local("apikeys", "Platform", "API Keys", render)`; render
builds KPIs (total, active), a Table (Name, Key `Cell::mono`, Policy, Created,
Status badge green/`revoked` red) and a Form — per-key policy `Field`
(`name: key-name`, `value: policy` — flat text, the contract has no richer
widget), plus `_new_name`/`_new_key`/`_new_policy` add-row triple (operator
supplies the key string; table displays it afterwards) and a `_revoke_name`
field (type a name to revoke). Submit closure diffs posted values → `Store`
calls, first-error-wins (config's `apply_edit` pattern). Implement
`adminapi::AdminData` on the service (read-only `ItemData`, `form: None` by
construction over the wire); add `apikeysrpc::register_admin(server, svc)` to
the existing `EDGE_SLOT` contribution; add
`admin_stub("apikeys", "APIKEYS_EDGE_ADDR", "127.0.0.1:9009")` to
`cmd/admin-svc`. Tests: render snapshot over a seeded store; submit
policy-edit + revoke round-trip. PLUS a live split-proof assertion (recipe rule:
named assertion per new cross-process flow): `[K5]` — `GET /admin/apikeys`
through the gateway passthrough with Basic auth (pattern of `[AD2]`) asserts the
remote apikeys admin page renders and contains `dev-client` (proves the
`admin.adminData` fan-out over the edge); mirrored in `.ps1`.

### Step 7 — full verification + docs  `[inline]`

**What**: run `./verify.ps1` (blocking tiers) and `./split-proof.ps1`; fix
fallout; update `CLAUDE.md` (module blurb under "Domain modules": apikeys
fortress — key store, policy-per-key, dev seed env vars; env var docs
`APIKEYS_DEV_SEED`/`APIKEYS_DEV_ALLOW`/`APIKEYS_EDGE_ADDR`); write
`docs/<subdir>/…-summary.md` per repo convention.

**Why last**: the safety net over the whole rollout; inline because it's
mid-rollout judgment (triage, small fixes), not delegable work.

## Non-goals (explicit)

- No hashing of keys at rest (plaintext, sessions-token precedent) — future.
- No per-peer mTLS identity on the internal edge; no `/events` auth — separate
  task (flagged in the design doc).
- No `Identity::Service` widening, no audience field on `Operation` — the key
  policy fully covers the requirement; revisit only if a trusted server must
  call `AuthReq::Player` ops without a session (impersonation — out of scope).
- Passthrough surfaces (`/admin`, `/accounts/epic`) stay key-free.
- Rate-limiting per key, key expiry (`expires_at`) — future.

## Verification map

- Unit: policy evaluator, TTL cache, store CRUD, seed idempotency, envelope
  serde compat (old envelope without `api_key` still parses).
- Gateway tests: 401/403 matrices on both planes.
- split-proof `[K1]–[K4]` + all existing assertions with keys (split AND
  monolith parity) — proves the at-risk path, both topologies.
- C# `C5`/`C6` — the user's requested external-client proof.
- `verify` blocking tiers green; `public-api` additive (new crate only).
