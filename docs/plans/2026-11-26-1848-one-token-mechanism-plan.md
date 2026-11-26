# One token mechanism: player access tokens become ES256 JWTs

Date: 2026-11-26
Parent: docs/plans/2026-11-24-0004-lore-integration-roadmap-plan.md, Phase A, between Step 2 (done) and Step 3.
Owner answers (2026-11-26): Q1 yes, an explicit-only dev seed; no separate `federated` grant, because an external provider (Epic, Google) only proves who the user is and every client may use every configured provider, as in Keycloak.
Plan review: core-reviewer (opus, high), approve with fixes; all 17 items applied (steps re-cut, Epic start client, client revocation rows, clock_timestamp, missing gates and launchers).

## Context

Owner (2026-11-26): "mysle claude ze lepiej to zrobic teraz inaczej bedziemy mieli dwa systemy tokenow";
"kazdy tego typu system robi to w ten sposob, jedna sciezka tokenow, roznie wydawane dla poszczegolnyhc
klientow z rozna zawartoscia"; "nie bedziemy robic hardcode" (clients are created in the admin portal).
Rulings the same day:
- Refresh families stay in the DB. Every family has a client.
- A client limits what a program may ask for. User permissions come from authz, not from the client.
- Sizing answers ("ok mozesz zrobic zgodnie z rekomendacjami"):
  (1) the gateway pulls revocations from a new accounts capability every 5 s and refuses tokens once its
  copy is 60 s stale, in both topologies;
  (2) the game is a portal-created client, and the login ops take a `client_id`, with the SDK regenerated;
  (3) access JWTs live 15 minutes.

Requested outcome: one token path. The player access token minted by register, login, federated login,
guest and refresh is an ES256 JWT signed with the A2 keys. It carries the client it was issued to and is
verified by the gateway without a database round trip. A revocation reaches the gateway within one
5 s pull. The opaque `accounts.sessions` table and the per-request `verify_session` call go away.

Not in this plan, because roadmap Step 3 (A3) owns them: authorization code + PKCE, redirect URIs, client
secrets, `root_domains`, scopes, the `/token` endpoint, `id_token`, and Lore-audience tokens. The client
table is created here under the roadmap's name `oidc_clients`, so A3 adds columns to it rather than a
second table.

### Overlap (why not extend what exists)

- **`accounts.sessions` + `Sessions::verify_session`** (api/accounts/api/src/lib.rs:212-217,
  modules/accounts/src/lib.rs:1143). This is today's opaque bearer: every request makes one DB read
  through accounts. Extending it means keeping two token kinds once A3 signs JWTs, which the owner
  rejected. It is replaced, not extended.
- **`config::CachedConfig`** (api/config/rpc/src/lib.rs:65-105), a whole-snapshot cache filled over the
  edge. It is the shape copied for the gateway's verification state, but it is config's own type and
  refreshes on NOTIFY invalidation. The owner chose a 5 s pull with a 60 s staleness refusal, so the
  gateway gets its own small snapshot holder; it does not depend on config.
- **A durable `token.revoked` event.** gateway-svc has no DB, and a durable-plane subscriber needs one.
  A pull over the existing accounts edge works in both topologies with no new plane consumer (owner
  answer 1).
- **apikeys** (modules/apikeys) authenticates the *program* per request with `X-Api-Key`. It stays as
  it is. An OIDC client is a different thing: it is named in the JWT at login and limits which grants
  can mint, while the API key still gates op dispatch. Folding one into the other is not requested.

## Facts (verified 2026-11-26, re-checked by the plan review)

Mint and refresh:
- The single mint point for register, login, guest and federated login is `Service::issue_session_tx`
  (modules/accounts/src/lib.rs:405-438). It calls `insert_refresh_family_tx`, `insert_session_tx` and
  `stamp_identity_used_tx`. Callers: `issue_password_session` (:443-462), :686, :719. Refresh mints
  through `issue_access_in_family_tx` (:959-975), called from `complete_rotation_tx` (:953) and the grace
  replay (:1302).
- The Epic browser login (epic_oauth.rs:213-231) calls `external_login` and returns the access and
  refresh tokens in the URL fragment. It is an HTTP route, not an op, so it carries no client today.
- A returning guest logs in through `login_federated(provider="guest")` (api/accounts/api/src/lib.rs:258-262).
- `ACCESS_TTL_MINUTES = 60` is at modules/accounts/src/store.rs:12, bound at :829. `access_expires_in_secs`
  is at :33.

Schema and revocation:
- The schema (lib.rs:219-240) has `accounts.sessions(token PK, player_id FK CASCADE, created_at,
  expires_at, family_id)` and `accounts.refresh_tokens(token PK, player_id FK CASCADE, family_id uuid,
  …)`. There is no families table; a family is only a uuid. The browser session lives in
  `accounts.browser_sessions` (lib.rs:240-250) and never touched `accounts.sessions`.
- The DDL is `CREATE TABLE IF NOT EXISTS`, and accounts tests run `migrate` on the persistent local test
  database (tests.rs:640-651), so a dropped table survives there until the schema is dropped by hand.
- Revocation writers:
  - `kill_family_tx` (store.rs:984-1000): a refresh replay (lib.rs:1309) and admin Revoke of one family;
  - `kill_player_families_tx` (store.rs:1050-1075): `reset_password` (lib.rs:646-664) and
    `delete_player_tx` (store.rs:618), which `delete_player_atomic` (lib.rs:986-1035) reaches in the tx
    that appends `player.deleted`;
  - `revoke_player_families_tx` (store.rs:1121-1143, admin Revoke all at admin.rs:808), which loops
    `kill_family_tx` over the granting families; its modal counts through
    `revocable_families_tx`/`live_browser_sessions_tx` (admin.rs:832-833).
- `granting_families!` (store.rs:351-366) unions refresh tokens and sessions. `family_of_player_tx`
  (store.rs:1022-1041) reads both tables.

Readers of `accounts.sessions` besides verify:
- presence `online_until` (store.rs:119-124);
- `player_by_session` (store.rs:1148-1163), used by `verify_session` and the Epic link flow
  (epic_oauth.rs:121, :180, where the browser sends its bearer);
- prune (store.rs:1172, the `accounts-sessions-prune` schedule);
- the tests that pin "the browser cookie is never a bearer": tests/hosted_login.rs:137,
  tests/epic_browser.rs:178, tests/browser_session.rs:22.

Signing keys:
- They are opened and checked at boot (signing_keys.rs:353-380, `open_live_keys`) and then dropped.
  Nothing signs yet.
- A2 owner answer 5 says signing reads the active key from the DB with a ~60 s cache.
- `jsonwebtoken` 9 `EncodingKey::from_ec_der` takes the PKCS#8 DER that `open` returns.

Gateway verification:
- `resolve_verifier_with` (modules/gateway/src/verifier.rs:130-163) requires `accounts.sessions`. Without
  it, it falls back to `DevSessionVerifier` only under `GATEWAY_DEV_VERIFIER`, and otherwise fails startup.
- `SessionsVerifier` (verifier.rs:94-120) checks `MAX_SESSION_TOKEN_BYTES` (verifier.rs:110).
- Every `/push` invoke frame re-runs bearer admission (push_ws.rs:1781, :1817 → lib.rs:883); the socket
  re-verify at push_ws.rs:1665 closes it with `session_expired` when the token expires.
- In the split, gateway-svc reaches accounts only through `accountsrpc::provide_factories()`
  (cmd/gateway-svc/src/lib.rs:100; api/accounts/rpc/src/lib.rs:71-75, which provides only
  `sessions_rpc`). Other services use `remote_factories()` (:41-55); wallet-, party-, groups- and
  friends-svc build a Sessions client there but never use it. api/accounts/rpc/src/tests.rs:10, :51, :85
  use `dyn Sessions`.
- accounts serves `sessions_rpc` over the edge unconditionally (modules/accounts/src/lib.rs:1727-1748)
  and, for the monolith, provides it in the local registry
  (`ctx.registry().provide::<dyn accountsapi::Sessions>`, lib.rs:1791-1794). The key is `accounts.sessions`.

Contract and gates:
- `MAX_SESSION_TOKEN_BYTES = 128` (api/accounts/api/src/lib.rs:25) caps both token kinds and the browser
  cookie. Its users: `session_token_within_cap` (lib.rs:145-146, used at :1144, :1261, store.rs:1149,
  web/session.rs:86, :135, web/mod.rs:463), gateway verifier.rs:70/:110, conformance rows
  (tools/conformance/src/policy.rs:107, :111, :352-363, :846-851), the gateway and accounts conformance
  probes, and tests.
- `Auth` (api/accounts/api/src/lib.rs:229-317) has five mint ops: register :235, login :242,
  login_federated :253, create_guest :262 (no args), refresh :271. HTTP body names are camelCase via
  `body_names` (`display_name="displayName"`).
- Every external input field needs a policy row: the input-inventory golden
  tools/conformance/input-fields.golden.tsv (auth rows 6-15, `accounts.verifySession token wire` at :15)
  is checked against policy.rs by the `conformance` stage and blessed with `--bless-input-golden`.
- docs/reference/contract-golden/contracts.txt carries the auth arg and body lines (:130-264) and the
  `verifySession` wire line (:335); the `contract-golden` stage gates it (`--bless-contract-golden`).
- The edge frame cap is `MAX_FRAME` = 16 MiB (core/edge/src/frame.rs:12).

Party and deletion:
- Seven modules tombstone deleted players in `purged_players` and refuse their create paths (for example
  characters store.rs:260). accounts' own ops answer `NotFound` once the player row is gone
  (store.rs:488). leaderboard, match, rating and mail do not purge, so nothing there changes.
- `party` purges in the delivery tx without a tombstone (party lib.rs:72). Its `create` (service.rs:367)
  and `accept` (:489) check nothing about deletion. Today that is safe only because the delete revokes
  sessions in the same tx as the append (lib.rs:986-998 doc).

Callers and generated surfaces:
- Callers that mint (grep, a lower bound):
  - splitproof: main.rs `register_login` :6481, `register_capture` :6513, `create_guest` :6700,
    `federated_status` :6679, inline :4295/:4312/:7030; accounts_admin_checks.rs:55-59;
    accounts_browser_checks.rs:145; Epic starts (run-time only) main.rs:4447 and
    accounts_browser_checks.rs:68 `EPIC_START`;
  - demoseed: lib.rs:734-744, scenarios.rs:165-273, client_tests.rs:192-229;
  - webui: demos/webui/src/index.html:130, :139 (:105 is refresh, which takes no client);
  - C#: clients/csharp/Program.cs:137.
- Splitproof SQL on `accounts.sessions`: DL1 main.rs:2930; PH13 :4251, :4269; SP0/SP2 :5192-5197;
  A9 :7064-7075; accounts_admin_checks.rs:259-283. A9, PH13 and the AR checks expect a 401 immediately
  after a revoke.
- Generated surfaces: the public-api baseline docs/reference/public-api-baseline/accountsapi.txt
  (`cargo run -p verifyctl -- --bless-public-api`, needs nightly); csharp-client-gen goldens
  tools/csharp-client-gen/testdata/* with a structural pin of 52 methods and 28 DTOs
  (`cargo run -p csharp-client-gen -- --out clients/csharp/Generated`); the `codegen-freshness` stage;
  the topiccheck golden (tools/topiccheck/src/golden.rs:122-126) and opscatalog-gen
  (tools/opscatalog-gen/src/main.rs:69), which name `sessions_rpc`.
- Dev-seed precedent: `APIKEYS_DEV_SEED` (modules/apikeys/src/lib.rs:125-135, :276-327), explicit-only and
  insert-if-absent. It is set by processctl fleet.rs, weles/fleet.*.toml and compose.dev.yaml:24.
- There is no UILayout mockup for a clients page. The admin precedent for "create an object, show it,
  delete it" is the apikeys keys page (modules/apikeys/src/admin.rs `KEY_CREATE`/`KEY_REVOKE`). Accounts
  admin pages live in `AccountsAdmin` (modules/accounts/src/admin.rs:446-585, :1008-1083).

## Decisions

- **D1 Clients.** Table `accounts.oidc_clients`:
  - `client_id text PK`, generated as `c_` plus 22 base64url chars, never chosen by the operator;
  - `display_name text NOT NULL`;
  - `grant_types text[] NOT NULL`, a subset of `password`, `guest`, `refresh_token`
    (A3/A4 add `authorization_code` and `device_code`);
  - `audience text NOT NULL`;
  - `created_at`.
  All clients in this plan are public (no secret). A3 adds its columns with `ADD COLUMN IF NOT EXISTS`.
  The admin portal creates and deletes clients. Deleting a client deletes its families by cascade and
  writes one client revocation row in the same tx (D4).
- **D2 Families.** Table `accounts.token_families`:
  - `id uuid PK`;
  - `player_id uuid FK players ON DELETE CASCADE`;
  - `client_id text FK oidc_clients ON DELETE CASCADE`;
  - `created_at`;
  - `access_expires_at timestamptz NOT NULL`, the newest access token's expiry, updated on every mint
    in the family.
  `refresh_tokens.family_id` gets an FK to it `ON DELETE CASCADE`. A revoked family's row is deleted, so
  its refresh tokens go with it, `online_until` stops counting it, and the Sessions panel stops listing it.
  A family "grants" while `access_expires_at > now()` or it holds an unexpired refresh token.
  Refresh inherits the family's client and is refused when that client no longer lists `refresh_token`.
- **D3 Mint.** The four login ops carry `client_id`, capped by `accountsapi::MAX_CLIENT_ID_BYTES = 64` and
  checked before the lookup. A mint checks that the client exists and lists the grant:
  - `password` for register and login;
  - `guest` for `create_guest` and for `login_federated(provider="guest")`;
  - none for every other `login_federated` provider and the Epic browser login: the client only has to
    exist. The provider proves who the user is; the token still goes to the client that started the
    login. Limiting providers per client can be added later without a contract break;
  - `refresh_token` for refresh.
  An unknown client, an oversized id or a grant the client does not list answers one
  `Unauthorized("invalid client")`.
  A mint for a client that does not list `refresh_token` issues no refresh token (standard OAuth;
  owner ruling 2026-11-26): `Session.refresh_token` is empty and the family holds no refresh row, so it
  grants only while `access_expires_at > now()`.
  The Epic browser login: `/accounts/epic/start` takes a `client_id` query parameter, stored on the
  `oauth_states` row, and the callback mints for that client after checking that it still exists. This follows
  the ruling that every family has a client.
  The JWT:
  - header `{alg: ES256, typ: "at+jwt", kid}`;
  - claims `iss` (the A2 issuer), `sub` = player_id, `aud` = the client's audience, `client_id`,
    `sid` = family id, `iat`, `exp`, `jti`.
  `iat`/`exp` come from the DB clock in the mint tx (`now()`, the tx start, which is never later than the
  commit). TTL is 15 minutes (`ACCESS_TTL_MINUTES = 15`). `accountsapi::SWAROG_AUDIENCE` is the game
  audience the gateway accepts. The signer reads the active key through a 60 s cache (A2 answer 5).
  Refresh tokens stay opaque in `refresh_tokens`. Boot fails if a token minted with the configured
  issuer and the longest claims would pass `MAX_ACCESS_TOKEN_BYTES`.
- **D4 Revocation.** Table `accounts.token_revocations`:
  - exactly one of `family_id uuid`, `player_id uuid`, `client_id text` (CHECK);
  - `revoked_at timestamptz NOT NULL`, stamped with `clock_timestamp()` after the lock the revoke
    takes, so a mint that commits before the revoke always has `iat <= revoked_at`;
  - `expires_at = revoked_at + ACCESS_TTL + ACCESS_EXP_LEEWAY + REVOCATION_SKEW_MARGIN`, all three
    constants in `accountsapi` (leeway 30 s, margin 60 s), so the row outlives every token it refuses.
  It has **no FK**, so a deleted player's or client's row survives the cascade.
  Writers, each in its existing transaction:
  - `kill_family_tx` → one family row (replay, admin Revoke);
  - `kill_player_families_tx` → one player row (reset password, delete player);
  - `revoke_player_families_tx` (admin Revoke all) → one player row instead of looping family rows;
  - client delete → one client row.
  A family row refuses every token with that `sid`. A player row refuses tokens of that `sub` with
  `iat <= revoked_at`, and a client row refuses tokens of that `client_id` with `iat <= revoked_at`
  (compared in whole seconds, rounding `revoked_at` up, which fails closed). The prune schedule
  `accounts-sessions-prune` keeps its name and deletes expired revocation rows. The snapshot is bounded
  by the revocation rate, not by the number of families: a client delete is one row.
- **D5 The verification capability.** New `#[rpc(prefix="accounts")] trait TokenState
  { #[retry_safe] async fn token_state(&self) -> Result<TokenStateSnapshot, Error>; }`, where
  `TokenStateSnapshot { issuer, keys: Vec<PublicJwk>, revoked_families: Vec<String>,
  revoked_players: Vec<Revoked{id, revoked_at_unix}>, revoked_clients: Vec<Revoked{id, revoked_at_unix}> }`.
  accounts implements it on `Service`, serves it over the edge unconditionally (replacing
  `sessions_rpc` at lib.rs:1727-1748) and provides it in the local registry under
  `registry::key("accounts", "token_state")` (replacing lib.rs:1791-1794). It is added to **both**
  `remote_factories()` and `provide_factories()`.
- **D6 Gateway verifier.** `JwtVerifier` replaces `SessionsVerifier`. A loop calls `token_state` every
  5 s, bounded by a 4 s timeout. It takes the monotonic instant **before** the call and stores the
  snapshot with that instant, so a slow reply never looks fresher than it is. The loop stops on module
  stop. Verify:
  - fail the cap check → `None`;
  - no snapshot yet, or one older than 60 s → `VerifyUnavailable` (503, the existing "outage is 503,
    never 401" rule);
  - otherwise a local ES256 check of the kid, `iss`, `aud == SWAROG_AUDIENCE` and `exp`
    (`ACCESS_EXP_LEEWAY`), then the three revocation sets;
  - pass → `sub`.
  The clocks are injected (wall time for `exp`, a monotonic instant for staleness), so no test races a
  real clock. `resolve_verifier_with` requires `accounts.token_state`, with the same dev fallback and
  fail-startup message. `/push` re-verify reuses the verifier. The gateway boots without a snapshot and
  answers 503 until the first pull succeeds.
- **D7 Token caps.** `MAX_SESSION_TOKEN_BYTES` splits into `MAX_OPAQUE_TOKEN_BYTES = 128` (refresh token,
  browser cookie, unchanged) and `MAX_ACCESS_TOKEN_BYTES = 1024` (the JWT). Each conformance row takes
  the cap of what it caps. `oauth_states.session_token` is unbounded `text`, so a JWT fits.
- **D8 Removed.** `accounts.sessions` and its three indexes, `insert_session_tx`, `player_by_session`,
  `prune_expired_sessions`, the `Sessions` trait, `sessions_rpc`, `verify_session`, `SessionsVerifier`,
  the `accounts.verifySession` conformance row and the admin test helper `fake_client`. This is a schema
  contract break: roll out with `DROP SCHEMA accounts CASCADE` and a fresh boot, with no migration,
  including the local test database before each accounts test run from Step 2 on.
- **D9 Readers rebuilt.**
  - `online_until` = max `access_expires_at` over the player's families.
  - `granting_families!`, `family_of_player_tx` and the admin counts read `token_families` and
    `refresh_tokens`.
  - The Epic link flow verifies the presented bearer with accounts' own `verify_access` (signature with
    its own keys plus `token_revocations` read from the DB, so accounts needs no pull).
  - The "cookie is never a bearer" tests assert `verify_access(cookie) == None`; they are kept.
- **D10 Party guard.** Party gets `party.purged_players(player_id PK, purged_at)`. Its existing
  `party.player-deleted.v1` handler inserts the row in the delivery tx, and `create` and `accept` refuse
  a purged caller with `NotFound`. Reason: a deleted player's JWT can still verify until the gateway's
  next pull (≤5 s, ≤60 s worst case), which the old same-tx session delete never allowed. Party is the
  only module with a create path and no tombstone (plan review, checked). The rows are permanent like
  the other modules' tombstones.
- **D11 First client on a fresh dev/proof boot** (owner answer Q1: yes). An explicit-only
  `ACCOUNTS_DEV_SEED` that inserts, if absent, one client `accountsapi::DEV_GAME_CLIENT_ID = "dev-game"`
  with all three grants and `SWAROG_AUDIENCE`. It is the `APIKEYS_DEV_SEED`/`WALLET_DEV_SEED` precedent,
  is set only by dev/proof launchers, and is off in production, where clients come from the portal.

## Steps

The contract break runs from Step 2 to Step 3 (the `client_id` argument) and from Step 5 to Step 6 (the
switch to JWTs). Inside a cut each step runs its own crates' tests; the workspace suite runs after
Step 3 and after Step 6. Every accounts test run from Step 2 on starts with
`DROP SCHEMA accounts CASCADE` on the local test database (D8).

### Step 1 [core-implementer, opus, high]: clients table, admin page, dev seed

(a) modules/accounts/src/lib.rs (schema DDL, `migrate`, env half of `init` for `ACCOUNTS_DEV_SEED`), a
new modules/accounts/src/clients.rs (store functions, id generation, grant parsing),
modules/accounts/src/admin.rs (a Clients section in `AccountsAdmin`: list, create with display name and
grant checkboxes, delete with confirm), api/accounts/api/src/lib.rs (`DEV_GAME_CLIENT_ID`,
`SWAROG_AUDIENCE`, grant names), and tests beside them. Every launcher that sets `APIKEYS_DEV_SEED` also
sets `ACCOUNTS_DEV_SEED=1`: tools/processctl/src/fleet.rs (monolith and accounts service lists),
weles/fleet.*.toml (where accounts runs), compose.dev.yaml:24 and tools/verifyctl/src/stages/csharp.rs.
(b) Every later step mints against a client. The step is additive: nothing reads clients yet.
(c) The seed runs in `migrate` under the migrate lock like the apikeys seed (apikeys lib.rs:276-327),
insert-if-absent. Admin create and delete are `controlplane` actions like apikeys
`KEY_CREATE`/`KEY_REVOKE`. Delete is a single `DELETE` here; Step 4 adds the client revocation row to
the same tx. The view follows the apikeys keys page shape (table plus form), because no mockup exists.
Tests: the seed is off when unset and inserts once when set; admin create rejects an empty name or no
grants; delete removes the row. If fleet_tests already pins `APIKEYS_DEV_SEED`, extend that test; do not
add new per-launcher pin tests (docs/todo/processctl.md "Launcher KEK and issuer pins may be
over-built").
(d) core-implementer, opus, high. Tell it that `cargo fmt` is not safe.

### Step 2 [core-implementer, opus, high]: client_id at login and token families

(a) api/accounts/api/src/lib.rs: `client_id: String` on register, login, login_federated, create_guest
(body name `clientId`), `MAX_CLIENT_ID_BYTES`. modules/accounts/src:
- lib.rs: the `token_families` DDL and the `refresh_tokens` FK; the client and grant check in
  `issue_session_tx` (D3 grant table) and in refresh;
- store.rs: the family insert and its `access_expires_at` update on every mint; `online_until`,
  `granting_families!`, `family_of_player_tx` and the admin counts read families (D9); `kill_family_tx`
  and `kill_player_families_tx` delete the family rows;
- epic_oauth.rs: `/accounts/epic/start` takes `client_id`, stored on `oauth_states`, checked at the
  callback (D3);
- conformance.rs and tools/conformance/src/policy.rs: four `clientId` rows capped by
  `MAX_CLIENT_ID_BYTES`;
- `issue_session_tx` writes no refresh row and returns an empty `refresh_token` when the client does
  not list `refresh_token`; the `Session.refresh_token` doc in api/accounts/api/src/lib.rs says so (D3,
  owner ruling 2026-11-26, added in review of Step 2).
Tests: one `login_with_dev_client` style helper; point the ~175 accounts test call sites at it instead of
spelling the id each time.
(b) Families must carry a client before a JWT can name it. Opaque sessions keep minting and verifying in
this step, so accounts and the gateway still work.
(c) Sessions are still written; the readers move to families now so that Step 6 only deletes. Tests:
- an unknown client, an oversized id and a grant the client lacks each give 401;
- a guest-only client can `create_guest` and log back in with `provider="guest"`, but not `password`;
- refresh is refused once the client drops `refresh_token`;
- a mint for a client without `refresh_token` returns an empty refresh token, writes no refresh row, and
  its family is pruned once its access expires;
- the Epic start without `client_id` is refused;
- a revoked family's row is gone and `online_until` no longer counts it.
(d) core-implementer, opus, high. Tell it that `cargo fmt` is not safe and that the workspace does not
compile outside accounts until Step 3.

### Step 3 [core-implementer, sonnet, high]: callers and gates for client_id

(a) The callers pass `accountsapi::DEV_GAME_CLIENT_ID` (or `"dev-game"` outside Rust):
- tools/demoseed (lib.rs:734-744, scenarios.rs:165-273, client_tests.rs:192-229);
- tools/splitproof, compile-level only (the mint helpers and inline sites in Facts); the proof logic is
  Step 8's. The Epic start callers compile without `client_id` and fail only at run time, so they are
  named here: main.rs:4447 ([EP1]) and accounts_browser_checks.rs `EPIC_START` :68 (used by [BG3] :373,
  [BR2]/[BR3] :470) take `?client_id=dev-game` (added in review of Step 2);
- any mint call in tools/loadgen (grep);
- demos/webui/src/index.html: done in Step 2 part 2 (9051e4b9), nothing left;
- clients/csharp/Program.cs:137;
- tools/conformance/src/policy.rs: four `CapCase`s with `cap: accountsapi::MAX_CLIENT_ID_BYTES`, next to
  "accounts refresh token", each probing its `accounts::conformance::conformance_*_client_id_rejected`
  (landed in 6e3c5b8f); the four `clientId` row texts name their CapCase (added in review of Step 2).
Then regenerate:
- clients/csharp/Generated and the csharp-client-gen goldens (the 52/28 pins hold; re-bless the manifest
  with `--emit-manifest`);
- docs/reference/public-api-baseline/accountsapi.txt (`--bless-public-api`);
- docs/reference/contract-golden/contracts.txt (`--bless-contract-golden`);
- tools/conformance/input-fields.golden.tsv (`--bless-input-golden`).
(b) Step 2's contract change breaks these. This step closes the first cut.
(c) Regenerate with the named commands, never by hand, and diff each blessed file: only `clientId` lines
move. Then the workspace suite, and the verifyctl stages `public-api`, `contract-golden`,
`codegen-freshness`, `csharp-client` and `conformance` in one verifyctl rollout (safe-verification).
(d) core-implementer, sonnet, high.

### Step 4 [core-implementer, opus, high]: signer, revocations and the TokenState capability

(a) api/accounts/api/src/lib.rs: `TokenState` and its snapshot types (D5), `ACCESS_EXP_LEEWAY` and
`REVOCATION_SKEW_MARGIN`. api/accounts/rpc/src/lib.rs: `token_state_rpc` in both factory lists (next to
`sessions_rpc` for now). modules/accounts/src:
- a new jwt.rs: the signer with the 60 s active-key cache, `verify_access` (D9), and the boot size check
  (D3);
- lib.rs: the `token_revocations` DDL; `TokenState` for `Service`, served over the edge and provided in
  the registry (D5); the prune handler also deletes expired revocations;
- store.rs: the revocation writers of D4, with `clock_timestamp()` after the lock;
  `revoke_player_families_tx` writes one player row; the client delete from Step 1 writes a client row.
(b) Additive: nothing mints a JWT yet, and the gateway still uses `Sessions`. Step 5 needs a capability to
pull and Step 6 needs a signer.
(c) `token_state` reads the live JWK set (the A2 JWKS reader) and the revocations with
`expires_at > now()`. Tests:
- a signed token round-trips through `verify_access`, and an expired one (crafted past `exp`, no sleep)
  is refused;
- a replayed refresh writes a family row; reset password, delete and Revoke all each write one player
  row; a client delete writes one client row; the player row survives the player's cascade;
- a revocation stamped after a concurrent mint commits has `revoked_at >= iat` (two connections, the mint
  committed first; no sleeps);
- prune removes only expired revocations;
- `token_state` returns the keys and the three lists.
(d) core-implementer, opus, high.

### Step 5 [core-implementer, opus, high]: gateway JwtVerifier

(a) modules/gateway/src/verifier.rs (`JwtVerifier`, `resolve_verifier_with` requiring
`accounts.token_state`), modules/gateway/src/lib.rs (start and stop of the pull loop),
modules/gateway/src/conformance.rs (the probe drives `JwtVerifier` with an unavailable `TokenState`),
modules/gateway/src/tests.rs, and the gateway `Cargo.toml` (`jsonwebtoken`). `SessionsVerifier` is
deleted in Step 6 with the trait.
(b) Opens the second cut: from here the live gateway refuses the still-opaque tokens until Step 6.
(c) The loop follows the party sweep's start and stop (modules/party/src/sweep.rs:133-205,
lib.rs:251-270). The snapshot is replaced whole behind a lock. The verify order is D6. Tests use a fake
`TokenState` and injected clocks:
- no snapshot gives 503; a snapshot 61 s old gives 503, and one 59 s old verifies;
- a reply that arrives late is stamped with the instant before the call, so it can be stale on arrival;
- a revoked `sid` gives 401; a player or client cut-off refuses `iat <= revoked_at` and accepts a later
  token;
- a wrong `aud`, a wrong `iss`, an unknown kid and a bad signature each give 401;
- an oversized token gives 401 before parsing;
- `resolve_verifier_with` fails startup without the capability and the dev flag.
(d) core-implementer, opus, high.

### Step 6 [core-implementer, opus, high]: switch to JWTs and remove sessions

(a) api/accounts/api/src/lib.rs (the cap split D7; removal of `Sessions`), api/accounts/rpc/src/lib.rs
and tests.rs (`sessions_rpc` removed), modules/accounts/src (mint signs a JWT with `ACCESS_TTL_MINUTES =
15`; the sessions DDL, `insert_session_tx`, `player_by_session`, `prune_expired_sessions` and the
`Sessions` server and registry provide removed; epic_oauth.rs :121/:180 call `verify_access`; the cookie
tests retargeted per D9; the delete doc at lib.rs:986-998 drops "60-minute"), modules/gateway
(`SessionsVerifier` removed, cap tests at tests.rs:96, :148, :152, :210, :705), the topiccheck golden
(tools/topiccheck/src/golden.rs:122-126), tools/opscatalog-gen/src/main.rs:69, the conformance rows
(D7, and the `accounts.verifySession` row removed), and the re-blessed public-api, contract and
input goldens. Also (added in review of Step 2):
- the admin Sessions panel's Client column (admin.rs:1186) reads `oidc_clients.display_name` through
  `token_families.client_id`; it replaces `fake_client`, which is production view code, not only a test
  helper;
(b) Closes the second cut. After it there is one token path.
(c) Diff every blessed file: only session lines go and cap lines change. Then the workspace suite and
the same verifyctl stages as Step 3 in one rollout.
(d) core-implementer, opus, high. Mostly deletion; Steps 2 and 4 already built what remains.

### Step 7 [core-implementer, sonnet, high]: party purged-player guard

(a) modules/party/src/lib.rs (DDL, the `party.player-deleted.v1` handler), store.rs (insert and
check), service.rs `create` (:367) and `accept` (:489), and purge_tests.rs.
(b) After Step 6, D10's window lets a deleted player's still-valid JWT create a party.
(c) Copy the characters guard (characters store.rs:260, :288). The insert goes in the existing delivery
tx; the check runs in the same tx as the create or join, before the members insert.
Tests: after a delivered `player.deleted`, `create` and `accept` by that player are `NotFound`; with the
check reverted they create.
(d) core-implementer, sonnet, high (one module, a written-out copy of a precedent).

### Step 8 [test-author, opus, high]: split proof

(a) tools/splitproof/src/main.rs (the session SQL at DL1/PH13/SP0/SP2/A9, the revoke checks),
accounts_admin_checks.rs (:259-283) and new checks.
(b) The proof runs only once the code is final (Steps 1-7).
(c)
- The session SQL becomes `token_families`/`token_revocations`.
- A9, PH13 and AR poll for the 401 within a bound of two pull periods, taken from the gateway's pull
  constant, not a literal. A 401 still missing at the bound is red.
- New assertions, in the split through gateway-svc and in the monolith:
  - a token from the dev client verifies;
  - after admin Revoke of its family it is refused within the bound;
  - a login with an unknown `clientId` is 401;
  - a client created through the admin action can log in, and once it is deleted its token is refused
    within the bound.
- Previously-wrong branches: a gateway still on `verify_session` (no `accounts.sessions` capability is
  left, so it fails startup), and a revocation that never reaches gateway-svc (the bound goes red).
(d) test-author, opus, high, followed by a proof-auditor.

### Step 9 [main session, docs]

- docs/reference: the accounts and gateway auth sections, docs/reference/push-hub.md (a socket now
  closes with `session_expired` at least every 15 minutes), docs/reference/csharp-client.md:121.
- .claude/rules/domain-modules.md: the accounts and gateway entries (the `accounts.token_state`
  capability, `ACCOUNTS_DEV_SEED`, the 15 min JWT, the 5 s and 60 s rule, the party tombstone) and the
  "a client re-POST is a 401 by construction" statement, which now holds only after the next pull.
- Prose that still names `accounts.sessions`: cmd/gateway-svc/src/main.rs:12, :100,
  cmd/gateway-svc/src/lib.rs:100, cmd/accounts-svc/src/main.rs:3, :28, cmd/gateway-svc/tests/boots.rs:32,
  api/wallet/api/src/lib.rs:101, api/apikeys/api/src/lib.rs:41, :44, tools/requirecheck/src/main.rs:16,
  and the `Auth::delete_account` doc (api/accounts/api/src/lib.rs:~305-311).
- The roadmap lines :131 and :201 (Swarog-audience tokens are JWTs now), docs/roadmap/feature-tracker.md,
  docs/todo/accounts.md. The plan Status becomes done.

## Not covered

- A3/A4 scope (see Context).
- API-key verification is unchanged.
- Presence stays token-derived with a ≤15 min tail instead of ≤60 min. No socket presence.
- Signing-key and KEK rotation stay on roadmap Step 9. The verifier accepts any kid in the snapshot, so a
  retiring key keeps verifying.
- wallet-, party-, groups- and friends-svc keep `remote_factories()`, which gains `token_state` and loses
  `sessions_rpc`; none of them uses either.

