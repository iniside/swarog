# OIDC device authorization grant (roadmap A4)

Date: 2026-11-27
Parent: docs/plans/2026-11-24-0004-lore-integration-roadmap-plan.md, Phase A, Step 4.
Builds on: docs/plans/2026-11-27-1020-oidc-authorization-code-plan.md (A3: `/authorize`, `/token`,
`/revoke`, `oidc_codes`, the generalised mint, confidential clients).
Status: approved (owner, 2026-11-27); Step 1 done; Steps 2, 3a and 3b done ("ok to zrob kroki 2 i 3"); Step 4 done (e86554d8, review PASS); Steps 5 and later wait for the owner
Plan review: core-reviewer (opus, high), REJECT with 13 items; all applied below.
Owner answers (2026-11-27, "na punkty mozesz byc jak rekomendujesz"): D2 per-client lifetime on the
Clients page, as recommended; D10 "View devices" drawn disabled until roadmap Step 12, as recommended.

## Context

Owner (2026-11-27): "dobra mozesz zrobic resreach pod A4". The outcome is roadmap Step 4: the Lore CLI
(`lore login`) signs a player in through the device grant (RFC 8628). The player approves the device in
the browser on the portal page drawn in `UILayout/portal/device-approval.html`.

**Owner answers (2026-11-27):**
1. user_code: 8 characters, `XXXX-XXXX`, a consonant alphabet without lookalikes, fixed length. The
   code's **lifetime** is configurable from the portal (owner: "chcialem zeby czas waznosci sie dalo
   konfigurowac"), default 10 min, with bounds.
2. Device line: the User-Agent, labelled "reported by the device".
3. "Session lasts": the real family lifetime, not the mockup's 90 d.
4. Deny marks the row denied; the next poll gets `access_denied`.
5. Eligible clients: any **public** client given the `device_code` grant in the portal. No hardcoded Lore
   client.
6. Polling: `slow_down` adds 5 s, state in Postgres. The `accounts_login` IP bucket applies to code entry.
7. Scope text: generic, naming the client, until `authz` exists (roadmap Step 5).
8. `verification_uri_complete` is issued; the confirm click is still required.

**Explicit exclusions:**
- A Profile / "devices" page (portal, roadmap Step 12).
- `authz` scopes and their wording (Step 5).
- Confidential device clients, `client_credentials`, token exchange.
- A per-code wrong-guess counter beyond the IP bucket.
- The C# client.

## Facts (HEAD f432fbca; path:line read 2026-11-27)

**Lore CLI** (Lore v0.14 source, `G:/tmp/lore-latest`):
- `lore-transport/src/auth/oidc/device.rs`: the device request posts only `client_id` and `scope` (the
  server's default scopes plus `offline_access`). It needs `device_code`, `user_code`,
  `verification_uri`, `expires_in`; `verification_uri_complete` and `interval` are optional (default 5 s).
  It opens `verification_uri_complete` when present. Endpoints must be https, or loopback http when the
  issuer is http.
- The poll posts `grant_type`, `device_code`, `client_id`. `authorization_pending` → keep polling;
  `slow_down` → interval += 5 s (`lore-revision/src/auth/login.rs:545`); `access_denied` →
  NotAuthenticated; `expired_token` → "Login session expired"; anything else is an error.
  Any 4xx with a JSON error is a refusal and any 5xx a hard failure
  (`lore-transport/src/auth/oidc/token.rs:133-157`). It polls straight away, with no first sleep;
  `MIN_POLL_INTERVAL` is 1 s (`login.rs:123`, `:526-540`).

**Grants and clients:**
- `grant_types::ALL: [&str; 4]` (api/accounts/api/src/lib.rs:103-111). The client form's grant picker
  is `FieldKind::Multi(grant_types::ALL)` (modules/accounts/src/admin.rs:304), and `canonical_grants`
  (modules/accounts/src/clients.rs:93) filters against `ALL`, so a new constant reaches the form.
- `CONFIDENTIAL_GRANTS = [AUTHORIZATION_CODE, REFRESH_TOKEN]` (clients.rs:31). The admin refuses any
  other grant for a confidential client, and `client_allows_tx` (clients.rs:280) mints for a confidential
  client only under `authorization_code`. A device client is therefore public by construction.
- Clients are create-only: `create_client_form` (admin.rs:323); no edit form exists.
- `FieldKind` has `Int` and `Duration` (core/controlplane/src/lib.rs:848-866).

**/token and the mint:**
- `grant()` dispatch (modules/accounts/src/web/token.rs:222-230), `code_grant` (:256),
  `refuse_unredeemable` (:288), `mint_code` / `mint_code_tx` (:308, :329). `mint_code_tx` lock order:
  code-family lock → family revoked → `client_allows_tx` → player key share → code kept → `mint_session_tx`
  → id_token when the scope has `openid`.
- Families live `REFRESH_TTL_DAYS = 30` (modules/accounts/src/store.rs:17).

**Codes, sessions, pages:**
- `oidc_codes` DDL (modules/accounts/src/lib.rs:297): digest PK, client and player FKs, `session_digest`
  FK ON DELETE SET NULL, `redeemed_at` + `family_id` CHECK.
- codes.rs: `CODE_TTL_SECS` 60 (:10), `REPLAY_WINDOW_SECS` 600 (:12), on-demand `prune_oidc_codes`
  (:56), `insert_oidc_code` (:78, INSERT … SELECT FROM browser_sessions), `redeem_oidc_code` (:118),
  `delete_player_oidc_codes_tx` (:233), called from store.rs:1263 and :1342 (the player-deletion paths).
- `/authorize` signed-out path: `held_session_digest` (authorize.rs:211) → 303 to
  `LOGIN?return_to=` (:241-245). Browser sessions carry `csrf_token` (web/session.rs:31-33).
- `LoginLimiter` (web/mod.rs:76) reads the client IP through trusted proxies. Hosted routes at
  web/mod.rs:263-269.
- Discovery lists `grant_types_supported: [AUTHORIZATION_CODE, REFRESH_TOKEN]` (discovery.rs:39). The
  splitproof checks it in `tools/splitproof/src/oidc_discovery_checks.rs`.
- gateway-svc passes the whole `/accounts/oidc` prefix to accounts-svc (cmd/gateway-svc/src/addrs.rs:
  231-246) and extends `X-Forwarded-For` (modules/gateway/src/proxy.rs:127-146). The fleet sets
  `TRUSTED_PROXY_CIDRS=127.0.0.1/32` for accounts-svc (tools/processctl/src/fleet.rs:962, :983, :1106), so
  the trusted-proxy walk (web/mod.rs:86-90) sees the real client IP in the split.
- Lock order: `kill_player_families_tx` and `revoke_player_families_tx` (store.rs:1248, :1333; reset,
  Revoke all, self-delete lib.rs:872, `delete_player_tx` :700) lock the player `FOR UPDATE` first, then
  delete browser sessions and the player's codes (store.rs:1263, :1342). `delete_client` locks the
  client `FOR UPDATE` and cascades (clients.rs:325-349). The code grant therefore commits its redemption
  before the mint, so the code row lock is never held across the player lock (codes.rs:113-115), and
  `oidc_codes` declares the player FK before the session FK so FK checks lock player before session
  (lib.rs:290-293).
- `mint_code_tx` passes `Some(AUTHORIZATION_CODE)` to `client_allows_tx` (token.rs:337-341);
  `redeemed_code_kept_tx` re-reads only `oidc_codes` (codes.rs:220-235). `RedeemedCode.auth_time` is
  the session's `created_at` captured at insert (codes.rs:94, :131).
- `/authorize` refuses a scope the client does not list (authorize.rs:142-145).
- A create-client split proof (tools/splitproof/src/accounts_token_checks.rs:139, TK3) posts the form
  with every grant in `grant_types::ALL` and no lifetime field.

**The `config` module** (researcher, 2026-11-27):
- `configapi::Config` is a read-only getter trait (api/config/api/src/lib.rs:67-81). There is no way to
  declare a key, a default or bounds; the portal edits only existing rows and checks only the type
  (modules/config/src/admin.rs:161-211). The only bounded-integer precedent is a reader-side clamp
  (modules/characters/src/service.rs:117-122).
- accounts has no `requires()` and no config use (modules/accounts/src/lib.rs:2091-2171). In the split,
  using config means a `configrpc` stub in accounts-svc whose boot fill fails startup when config-svc is
  down (api/config/rpc/src/lib.rs:188-190), and moving config-svc before accounts-svc in the fleet
  (tools/processctl/src/fleet.rs:841, :1075). Every front-door service depends on accounts-svc, so
  config-svc becomes a boot dependency of the whole front door.

**Mockup** (`UILayout/portal/device-approval.html`): 1 enter code (8 boxes, "The code expires in …",
Continue); 2 confirm ("Allow <client> on <device> to act as <handle>?", scope sentence, Code / Device /
Requested from (IP · age) / Session lasts; Deny / Allow device); 3a approved ("… is connected", "View
devices" :70); 3b denied.

## Overlap

| Existing | Use | Why not extend it instead |
| --- | --- | --- |
| `/token` `grant()` dispatch | Extended: one `DEVICE_CODE` arm | — |
| `mint_code_tx` | Generalised to a redeemed grant (code or device) | Copying it would fork the lock order and the replay/family checks |
| `oidc_codes` | Not reused; sibling table `device_codes` | Different lifecycle (pending → approved/denied), polling state, user_code lookup (roadmap :132) |
| `/authorize` page pattern | Copied for `/device` | Different form and states; shared helpers (`held_session_digest`, `see_other`, `safe_return_to`) reused |
| `LoginLimiter` | Reused on code entry | — |
| `config` module | Not used (D2) | No declared keys or bounds; adds a front-door boot dependency |

## Decisions

- **D1 Grant.** `grant_types::DEVICE_CODE = "urn:ietf:params:oauth:grant-type:device_code"` joins `ALL`
  (additive contract change). It stays out of `CONFIDENTIAL_GRANTS`. The client form picks it up.
  `grant()`'s `unauthorized_client` arm (token.rs:228) lists it, so a client without the grant gets
  `unauthorized_client`, not `unsupported_grant_type`.
- **D2 Lifetime setting (owner: as recommended).** A per-client `device_code_ttl_secs` column on
  `oidc_clients`, bounded 2–30 min, default 10 min, with a DDL CHECK fed by the same bound constants. On
  the create form it is an optional `Duration` field (missing → default, so TK3 still passes); a
  "Device code lifetime" row action on the Clients page (a `Then::Form` with `prepare`,
  core/controlplane/src/lib.rs:701-728) changes it; out-of-range values are refused by accounts' admin. The
  alternative, a `config` key, gets no portal bounds, needs the operator to create the row by hand and
  makes config-svc a boot dependency of the whole front door (Facts).
- **D3 Codes.** `device_code`: 32 random bytes, base64url, stored as a SHA-256 digest (PK). `user_code`:
  8 characters from `BCDFGHJKLMNPQRSTVWXZ` (RFC 8628 §6.1, ~34.6 bits), shown `XXXX-XXXX`; input is
  upper-cased and stripped of `-` and spaces; stored as a digest with a UNIQUE constraint. An insert
  collision retries with a new code (bounded tries).
- **D4 Table** `accounts.device_codes`: `device_code_digest` PK, `user_code_digest` UNIQUE, `client_id`
  FK ON DELETE CASCADE, `scope`, `interval_secs`, `last_polled_at`, `expires_at`, `status`
  (pending/approved/denied, CHECK), then `player_id` FK ON DELETE CASCADE declared **before**
  `session_digest` FK ON DELETE SET NULL (FK checks lock player before session, the order a reset takes
  them; lib.rs:290-293), `auth_time` (the session's `created_at`, set on approval; CHECK: player,
  auth_time set iff status = approved), `redeemed_at` + `family_id` with the `oidc_codes` CHECK,
  `requested_ip`, `user_agent` (bounded length), `created_at`. New table, so it boots fresh.
- **D5 Endpoint** `POST /accounts/oidc/device_authorization`: form `client_id`, `scope`. Public clients
  that list `DEVICE_CODE` only (else `unauthorized_client`). Scope checked like `/authorize`.
  `verification_uri` = `{issuer}/device` and `verification_uri_complete` = `{issuer}/device?user_code=…`,
  built from the issuer like discovery.rs:35-38, never from `Host`. `requested_ip` comes from the
  trusted-proxy walk (web/mod.rs:86-90, made `pub(crate)`). Answers `{device_code, user_code,
  verification_uri, verification_uri_complete, expires_in, interval: 5}` with `Cache-Control: no-store`.
  Prunes first: `FOR UPDATE SKIP LOCKED` like codes.rs:57-60, cut-off `expires_at + REPLAY_WINDOW_SECS`.
- **D6 Polling** on `/token` with `grant_type=DEVICE_CODE`. The poll never holds the device row across a
  client or player lock (Facts, lock order). Each check below is its own short statement that commits on
  its own, in this order:
  1. unknown `device_code`, or another client's → `invalid_grant`;
  2. already redeemed → revoke that family (as `refuse_unredeemable`, token.rs:288-306) and
     `invalid_grant` (D8);
  3. past `expires_at` → `expired_token`;
  4. polled sooner than `interval_secs` after `last_polled_at` → `interval_secs += 5`, `slow_down`;
  5. stamp `last_polled_at`; pending → `authorization_pending`; denied → `access_denied`; approved with
     `session_digest` NULL (signed out since) → `expired_token`;
  6. approved → redeem with one guarded `UPDATE … SET redeemed_at, family_id = gen_random_uuid() WHERE
     status = 'approved' AND redeemed_at IS NULL AND session_digest IS NOT NULL AND expires_at > now()
     RETURNING …`, committed, then mint in a separate transaction (D7).
  All errors are 400 JSON. A lost race between steps falls through to the refusal of a later step, never a
  5xx. Erratum (review of b1343acf): after a guarded statement loses, the row is read and classified
  again once, so the settled row gets its own answer; only a second loss in a row answers `slow_down`.
- **D7 Mint.** `mint_code_tx` takes a redeemed-grant value (player, client, scope, family,
  auth_time, nonce = None; erratum: no session, the redeem UPDATE alone checks it) that also carries (a) the grant name passed to `client_allows_tx` and (b) which
  table the "kept" check re-reads after the player key share. Both grants share its lock order. The
  answer equals the code exchange: `refresh_token` when the client has the grant, `scope`, and
  `id_token` when the scope has `openid`.
- **D8 Replay.** A `device_code` polled again after its redemption gets `invalid_grant` and revokes the
  first redemption's family, as A3 D5 does for codes. This check runs before expiry and slow_down, and
  rows are kept `REPLAY_WINDOW_SECS` past expiry for it. An overlapping poll that loses to the redemption
  is a replay too and revokes the family, the same as the code grant (owner "ok", 2026-11-27).
- **D9 Approval page** `GET /accounts/oidc/device[?user_code=]`: signed out → 303 to the login page with
  `return_to`. No code → the enter-code form (screen 1). A code is looked up under `LoginLimiter`;
  unknown, expired or not pending → the enter-code form with one generic error. Found → the confirm
  screen (screen 2): client display name, device = User-Agent "(reported by the device)", requesting IP
  and age, the code, "Session lasts" derived from `REFRESH_TTL_DAYS` when the client has
  `refresh_token`, otherwise from `accountsapi::ACCESS_TTL_MINUTES` (no literals), and the generic scope
  sentence naming the client. `POST /accounts/oidc/device/approve` and `/deny` carry the user_code and
  the session's `csrf_token` and also pass `LoginLimiter::allow`, so they are not a guessing side door.
  Approve sets `player_id`, `session_digest` and `auth_time` from a live `browser_sessions` row (like
  `insert_oidc_code`); deny sets `denied`. Both only move a `pending`, unexpired row. Result screens
  3a / 3b.
- **D10 "View devices" (owner: as recommended).** No Profile page exists until roadmap Step 12. Draw the element disabled until then, as the repo does for unbuilt actions (admin.rs:558
  `not_implemented`), rather than pointing a "View devices" label at a page that lists no devices.
- **D11 Discovery.** Add `device_authorization_endpoint` and `DEVICE_CODE` to `grant_types_supported`.
- **D12 Reset, Revoke all, delete.** `kill_player_families_tx` and `revoke_player_families_tx`
  (store.rs:1263, :1342) delete the player's `device_codes` like `delete_player_oidc_codes_tx`, which
  makes D7's "kept" check answer false for a grant redeemed before them. Client delete cascades.

## Steps

All DB tests are sequential and never race a real clock: expiry, interval and age are set by backdating
`expires_at`, `last_polled_at` and `created_at` in SQL, never by sleeping.

### Step 1 [core-implementer, opus, high]: the grant constant and the per-client lifetime
(a) api/accounts/api/src/lib.rs (`DEVICE_CODE`, `ALL: [&str; 5]`), `oidc_clients.device_code_ttl_secs`
with its CHECK (lib.rs DDL, clients.rs, admin.rs: optional create field, row action, bounds), the
public-api golden.
(b) Every later step reads the client's grant and lifetime.
(c) Tests: a create without the field stores 10 min; below min and above max refused; the row action;
a confidential client cannot pick the grant. Re-bless and diff the public-api and contract goldens.

### Step 2 [core-implementer, opus, high]: the `device_codes` table and store
(a) A new `modules/accounts/src/device_codes.rs` and the DDL (D4): insert with retry, lookup by
user_code, approve/deny, the D6 statements, redeem, prune (D5), the D12 deletes.
(b) Steps 3b and 4 call it.
(c) DB unit tests (DROP SCHEMA accounts first): every D6 branch in order, including a replay after expiry
and a replay inside the interval (both revoke); the `slow_down` increment; a second approve refused;
approve after expiry refused; sign-out after approval → `expired_token`; reset deletes the rows; prune
keeps rows inside the replay window and skips locked rows.

### Step 3a [core-implementer, opus, high]: generalise the mint
(a) token.rs and codes.rs: the redeemed-grant value of D7 (grant name, kept-check source). No new route.
(b) Step 3b needs it; doing it alone keeps the code grant provably unchanged.
(c) The existing code-grant and race tests stay unchanged and green.

### Step 3b [core-implementer, opus, high]: `/device_authorization` and the `/token` device arm
(a) A new `web/device.rs` (endpoint, D5), token.rs (`DEVICE_CODE` arm in `grant()` and its
`unauthorized_client` arm, D6, D8), web/mod.rs routes.
(b) The CLI-facing half of the flow.
(c) Router tests: the D5 answer shape, URIs from the issuer and `no-store`; refusals (confidential
client, client without the grant → `unauthorized_client`, unknown client, unlisted scope); the poll
sequence pending → slow_down → approved → tokens with refresh and id_token; `access_denied`;
`expired_token`; replay revokes the family. Sequential race tests in the shape of tests/token.rs:494-496
and tests/authorize.rs:490: reset or Revoke all between redemption and mint → `invalid_grant` and no
family; `delete_client` during a poll; a poll/reset interleave never deadlocks or answers 5xx.

### Step 4 [core-implementer, opus, high]: the approval pages
(a) `web/device.rs` handlers and the hosted-page templates for screens 1, 2, 3a, 3b.
(b) The human half of the flow.
(c) Mockup checklist from `UILayout/portal/device-approval.html` (mockup-implementation skill): every
element present with D9 data, "View devices" per the owner's D10 answer, nothing invented. Router tests:
signed-out 303 with `return_to`; CSRF refusal; the limiter on lookup, approve and deny; one generic error
for unknown, expired and used codes; approve and deny move only a pending row; "Session lasts" follows
the constants.

### Step 5 [core-implementer, sonnet, medium]: discovery
(a) discovery.rs (D11), `tools/splitproof/src/oidc_discovery_checks.rs` (:153) and its tests,
`modules/accounts/src/tests/revoke.rs:230`.
(b) Lore finds the endpoint through discovery.
(c) The discovery unit test and `oidc_discovery_checks_tests.rs`.

### Step 6 [test-author, opus, high]: the device-grant split proof
(a) A new `tools/splitproof/src/oidc_device_checks.rs`, run through gateway-svc in the split and through
the monolith front.
(b) Proves the at-risk topology: the passthrough carries every new route and the browser leg.
(c) Checks: device_authorization → poll pending → sign in and approve through the hosted pages (the
confirm screen shows the forwarded client IP) → poll gives tokens the gateway accepts; deny →
`access_denied`; a fast second poll on its own device_code → `slow_down`; replayed device_code →
`invalid_grant` and the first family's bearer refused within one `token_state` pull. Prove each red with
a hand-injected fault (skip the status check, skip the interval check, skip the replay revoke, approve
without CSRF), then green. Then a proof-auditor (opus, high) review.

### Step 7 [docs-writer, sonnet, medium]: docs
(a) docs/reference/accounts-oidc.md (routes, including the approval pages `/accounts/oidc/device`, `/device/approve`
and `/device/deny` and their accounts entry, the device flow, errors, the lifetime setting, and that the
Lore client must list `offline_access` and every default scope Lore is configured with), the accounts
entry and the `/token` wording in .claude/rules/domain-modules.md.
(b) Lore operators configure the client from it.

## Plan-end verification
Workspace suite (accounts schema dropped first), then verifyctl, then the public-api, contract and input
goldens re-blessed and diffed. Splitproof once, after Step 6.
