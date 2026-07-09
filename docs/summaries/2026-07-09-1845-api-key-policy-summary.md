# API key policy — rollout summary

Plan: `docs/plans/2026-07-09-1711-api-key-policy-plan.md` (all 7 steps executed;
grumpy-review punch list — 1 blocker, 4 major, 5 minor — was folded into the plan
before implementation).

## What shipped (commits, in order)

1. `5e24a24` **feat(apikeys)** [opus] — new fortress `modules/apikeys` +
   contracts `api/apikeys/{api,rpc}`. Schema `apikeys.keys(name PK, key UNIQUE,
   policy, created_at, revoked_at)`, plaintext keys (sessions-token trust model).
   `apikeysapi::Keys::lookup_key` capability under `registry::key("apikeys","keys")`;
   explicit-only `APIKEYS_DEV_SEED` self-healing dev seed (`dev-key-client` =
   player-facing method list WITHOUT `match.report`; `dev-key-server` = `full`).
2. `e703d0a` **feat(gateway,edge)** [fable] — enforcement on both planes, post
   route/method match, pre session-auth: HTTP `X-Api-Key` header, player-QUIC
   `api_key` envelope field (additive serde; old clients get a clean 401).
   401 missing / 401 invalid / 403 policy-denied. `RealKeyVerifier` with 5 s TTL
   cache (caches Ok(Some)+Ok(None), NEVER caches Err, clears at 10k entries);
   `APIKEYS_DEV_ALLOW` explicit-only allow-all fallback else startup failure.
   `PlayerHandler`/`PlayerClient::call` widened; playercli `--api-key`.
   Gateway tests 24 → 46.
3. `793884a` **feat(cmd)** [sonnet] — `cmd/apikeys-svc` (metrics + apikeys, ports
   env-driven), gateway-svc `remote::Stub("apikeys", APIKEYS_EDGE_ADDR
   default :9009)`, fortress build lists in verify.sh/.ps1.
4. `4304010` **test(split-proof,run,webui,docs)** [sonnet] — 12th process boots in
   split-proof (:8091/:9009, distinct EVENTS_ORIGIN), `X-Api-Key: dev-key-client`
   on every op curl (`dev-key-server` on match/report), playercli `--api-key`,
   new `[K1]–[K4]` (401/401/403/202), monolith parity keyed, run.sh boots
   apikeys-svc + prints dev keys, webui `api()` sends the dev client key,
   CLAUDE.md smoke updated. Live `./split-proof.ps1`: PASS.
5. `749b7cb` **feat(clients/csharp)** [sonnet] — connection-scoped `--api-key` in
   gbclient (envelope `api_key`; no generator/Generated changes), verify csharp
   stage C1–C4 keyed + new C5 (`dev-key-client` on `match.report` → Forbidden) and
   C6 (`dev-key-server` → Ok). All six verified live against a monolith boot.
6. `6b0825a` **feat(apikeys,admin-svc)** [opus] — "API Keys" admin page
   (list/edit policy/add/revoke via flat-text form, config-item pattern),
   `AdminData` fan-out + `admin_stub("apikeys", …)` in admin-svc, split-proof
   `[K5]` (`GET /admin/api-keys` → 200 + `dev-client` — note: slug derives from
   the label, hence `api-keys` not `apikeys`). Live split-proof: PASS.
7. (this commit) **docs** [inline] — CLAUDE.md: 12 fortresses, apikeys blurb,
   split-proof port/scenario lists; this summary. Full `./verify.ps1 --fast` run.

## The model (recap)

Every request through the gateway carries an API key identifying the CLIENT
CLASS; the key's policy (DB, editable at runtime — admin page or SQL, ≤5 s
propagation via TTL cache) decides which wire methods it may call. Player
session bearer is unchanged and orthogonal (`AuthReq::Player` ops need both).
The client key ships inside the game build and is NOT a secret — it classifies,
the session authorizes; the server key IS a secret and is `full`.

## Non-goals / follow-ups (unchanged from the plan)

- Key hashing at rest; key expiry; per-key rate limits.
- `POST /events` has no auth (network-topology trust) — separate task.
- Per-peer mTLS identity on the internal edge — separate task.
- `Identity::Service` / impersonation (server calling Player ops) — only if a
  real need appears.

## Verification

- Trailer audit: all 6 commits match their dispatch lanes (opus/fable/sonnet ✓).
- `./split-proof.ps1` PASS twice (Steps 4 and 6) — 12-process split + monolith
  parity, including `[K1]–[K5]`, P1–P5 (NotFound preserved), C# C5/C6.
- `./verify.ps1 --fast` (build, clippy -D warnings, tests, cargo audit,
  fortress+archcheck, split-proof): result recorded in the Step 7 commit.
