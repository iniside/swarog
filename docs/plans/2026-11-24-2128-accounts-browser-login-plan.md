# Accounts: browser login for studio users (lore roadmap step 1)

Status: approved 2026-11-25

## Context

**Requested outcome (owner, 2026-11-24):** "mozesz zaczac od step 1, ale narazie
tylko plan do niego". Step 1 of `docs/plans/2026-11-24-0004-lore-integration-roadmap-plan.md`:
studio users can sign in to Swarog in a browser. In the roadmap's words: a
production email-and-password identity (the argon2 `dev` identity promoted and
renamed, not kept beside a new one), the `ACCOUNTS_DEV_AUTH` gate removed except
the gateway dev-verifier part, which gets its own flag, operator-created accounts
through the accounts admin `Provider`, a hosted login page under
`/accounts/oidc/` with the configured federated providers next to it, a
server-side browser session distinct from the player bearer, and one gateway-svc
passthrough entry for the prefix.

**Excluded (owner):** self-registration with email verification is Step 1b
(roadmap owner question 2). No OIDC endpoints yet (Steps 2–4). Weles code is untouched; its fleet
tomls get env lines like every other fleet (Step 2), which is fleet config, not a Lore
install.

**What exists (verified 2026-11-24, lines are lower bounds from grep + reads):**
- The `dev` identity is a `provider="dev"` row in `accounts.identities`
  (PK `(provider, subject)`, `subject` = raw email, `secret_hash` = argon2id PHC;
  `modules/accounts/src/lib.rs:164-172`). `players` has no email column
  (`lib.rs:122-128`). No email normalisation exists, so `A@x.io` and `a@x.io`
  are two accounts.
- `providers::DEV = "dev"` is the contract constant (`api/accounts/events/src/lib.rs:100`,
  public-api baseline `docs/reference/public-api-baseline/accountsevents.txt:4`),
  bound at `store.rs:639,1259` and in the admin (`admin.rs:20,63`). It is the
  provider string inside the `player.registered` payload.
- `register`/`login` (`api/accounts/api/src/lib.rs:182,188`, `auth="none"`) return
  `NotFound` unless `Service.dev_auth` (`lib.rs:826-829,880-883`), read once from
  `ACCOUNTS_DEV_AUTH` (`lib.rs:1253`, default off). Login already has timing
  equalisation (decoy hash), a 2-permit argon semaphore and 32 login slots.
- The gateway reads the same env name on its own (`modules/gateway/src/verifier.rs:127-160`,
  `dev_auth_explicitly_on`) to choose `DevSessionVerifier` (`dev-<id>` tokens)
  only when no `accountsapi::Sessions` capability exists.
- About 20 non-code places set or describe `ACCOUNTS_DEV_AUTH` (Step 2 lists them).
- `accounts.sessions` rows are player bearers: anything stored there passes the
  gateway verifier (`store.rs:1071` `player_by_session`).
- The only `ctx.mount` in accounts is the Epic web OAuth router at
  `/accounts/epic` (`lib.rs:1385-1394`, `epic_oauth.rs:212-230`), mounted only when
  Epic is configured; its callback hands a *player* session to `/?epic=…`.
  Google login takes a client-side id token; there is no server redirect flow for it.
- gateway-svc passthroughs are `ADDR_SPECS` rows with one prefix each
  (`cmd/gateway-svc/src/addrs.rs:232-246`): `/admin` and `/accounts/epic` on
  `ACCOUNTS_HTTP_ADDR`. Matching is `path == prefix || starts_with(prefix + "/")`
  (`modules/gateway/src/proxy.rs:109,121`); the proxy extends `X-Forwarded-For`
  (`proxy.rs:146,211`).
- `core/admin` has the closest browser-session precedent: opaque token in its own
  table with a per-session CSRF token, `HttpOnly`, `Secure` unless an explicit
  opt-out, IP limiter via `httpmw` + `TRUSTED_PROXY_CIDRS`, argon permit owned by the
  blocking closure (`core/admin/src/lib.rs:7-25,182-186,383-405,583-630,914-1010`).
  accounts already depends on `axum-extra` and `argon2`; `minijinja` and `httpmw`
  are workspace crates.
- The accounts admin `Provider` (`admin.rs:794`) has one page, `players`, with
  no `head_actions`. The apikeys create + `SecretReveal` flow is the precedent
  (`modules/apikeys/src/admin.rs:119-147,605-634,770-830,898-905`). The mockup
  `UILayout/redesign/players.html` has no create action.

**Overlap, why not extend X:**
- *A second identity beside `dev`*: rejected by the roadmap ("promoted and renamed").
  The register/login code, its caps and decoy timing are kept; the provider name and
  the gate change.
- *Store browser sessions in `accounts.sessions`*: they would pass as player bearers
  at every op. A separate table is required.
- *Reuse `core/admin`'s session code*: `core/admin` is a core module and not a
  library for modules; the pattern is copied, not imported.
- *New passthrough row per prefix*: `addrs_tests.rs:486` refuses two origins for one
  provider and the ten-address source would grow an address that is the same
  process. One row with a prefix list keeps "one entry" and the ten addresses.

## Steps

**Step 1: Password identity, one registration flag, one email boundary.**
- (a) `api/accounts/events/src/lib.rs` (`providers::DEV` → `providers::PASSWORD = "password"`),
  `modules/accounts/src/{providers.rs,lib.rs,store.rs,admin.rs,ops.rs}`, the accounts
  tests that set `dev_auth` or assert `"dev"` (`tests.rs`, `admin_tests.rs:196,213,336`,
  `tests/{delete,directory,guest,dev_auth_gate,last_used}.rs`, `conformance.rs`,
  `oidc_tests.rs`; start from `rg -n '"dev"|dev_auth|providers::DEV' modules/accounts`),
  `modules/gateway/src/verifier.rs`, `modules/gateway/src/{lib.rs,tests.rs}` mentions,
  public-api baseline `accountsevents.txt`.
- (b) Every later step signs in with this identity.
- (c)
  - `Service.dev_auth` becomes one boolean `open_registration` from
    `ACCOUNTS_OPEN_REGISTRATION` (explicit-only, default off, loud warn), gating only
    `register`. Login has no flag. Step 1b replaces the flag with verified
    self-registration (owner question 1).
  - `normalize_email` (trim + ASCII lowercase, then the existing length cap) is called
    first thing in `register`, `login`, and later in operator-create (Step 6) and the
    web form (Step 4b). Every later use (`password_identity`,
    `register_hashed_with_token`, `issue_password_session(&p, &email)`) gets the
    normalised value. The password path has no advisory lock today; the identities PK
    stays the only uniqueness guard.
  - Pull the body of the `login` op (caps, decoy hash, argon permit, identity lookup)
    into one `verify_password` function that the op and Step 4b's form both call.
  - Gateway: `dev_auth_explicitly_on` reads `GATEWAY_DEV_VERIFIER` instead; the stale
    "default-ON convenience" comment goes.
  - Wipe: existing `provider='dev'` rows are not migrated (DROP + boot fresh).
  - Tests: login without any env succeeds; register without the flag is 404 and with it
    201; `A@X.io` registered then `a@x.io` login succeeds and the reverse; a second
    register with other case is 409; `last_used` is recorded for a mixed-case login;
    the gateway picks `DevSessionVerifier` only on `GATEWAY_DEV_VERIFIER=1`.
- (d) `[opus]` core-implementer.

**Step 2: Env and tooling sweep.**
- (a) Code and fleet config:
  - `ACCOUNTS_DEV_AUTH` → `ACCOUNTS_OPEN_REGISTRATION` (accounts processes and the
    monolith) or `GATEWAY_DEV_VERIFIER` (only where a gateway runs without accounts):
    `tools/processctl/src/fleet.rs:26,47,911,943,996,1021` (allowlists and builders;
    `MONOLITH_BASE_ENV` is a fixed-size array), `tools/processctl/src/fleet_tests.rs:53,63`,
    `tools/devctl/src/tests.rs:191,210,231`, `weles/fleet.monolith.toml:33`,
    `weles/fleet.split.toml:49`, `compose.dev.yaml:25`,
    `weles/master/src/manifest_tests.rs:113,365`, `tools/verifyctl/src/stages/csharp.rs:224`,
    `tools/routecheck/src/main.rs:109`, `cmd/gateway-svc/tests/tls_front.rs:192`
    (→ `GATEWAY_DEV_VERIFIER`), `cmd/gateway-svc/src/main.rs:15`,
    `tools/demoseed/src/{report.rs:249,report_tests.rs:97,lib.rs:118}` (the hint names
    the new flag). `deploy/` is gitignored and not touched.
  - New env for Step 4: `ACCOUNTS_COOKIE_SECURE=0` wherever a fleet serves plain http
    (processctl dev and proof fleets, `MONOLITH_BASE_ENV`, `compose.dev.yaml`, the weles
    tomls), and `TRUSTED_PROXY_CIDRS` for accounts-svc in the split (the
    `ACCOUNTS_OVERRIDEABLE_ENV` allowlist `fleet.rs:26`, the processctl split builder
    next to admin-svc's `fleet.rs:900,906`, `weles/fleet.split.toml` accounts-svc
    block, as `:254` does for admin-svc).
- (b) Step 1 changed the names; nothing boots right until the tooling follows.
- (c) The sites are a grep lower bound: the step starts and ends with
  `rg ACCOUNTS_DEV_AUTH` over everything except `docs/`, `.claude/`, `.agents/`
  (Step 8) and `experiments/` (not maintained), and ends with that grep empty.
  splitproof A5 (`tools/splitproof/src/main.rs:4415`) keeps its meaning: gateway-svc
  never sets `GATEWAY_DEV_VERIFIER`; its message is updated. `fleet_tests` assert that
  accounts-svc gets `TRUSTED_PROXY_CIDRS` in the split.
- (d) `[sonnet]` core-implementer (mechanical).

**Step 3: One passthrough entry for both accounts browser prefixes.**
- (a) `cmd/gateway-svc/src/addrs.rs` (`AddrClass::Passthrough { prefixes: &'static [&'static str] }`,
  the accounts row carries `["/accounts/epic", "/accounts/oidc"]`, `to_wiring` calls
  `with_passthrough` once per prefix), `addrs_tests.rs`, the table doc comment.
- (b) Step 4's pages must be reachable through gateway-svc.
- (c) The address count stays ten; managed mode (`addrs.rs:422-425` `exactly_one`,
  `tools/verifyctl/src/stages/weles_managed_gateway.rs:61`) resolves one origin per
  provider as today. Test: the wiring for the accounts origin holds both prefixes
  (fails on revert).
- (d) `[opus]` core-implementer.

**Step 4a: Browser session storage.**
- (a) `modules/accounts/src/{lib.rs,store.rs,admin.rs}`, new `modules/accounts/src/web/session.rs`.
- (b) Roadmap Steps 2–4 (authorize, device approval) need a signed-in browser.
- (c)
  - Table `accounts.browser_sessions`: `token_digest` (SHA-256 of an opaque 32-byte
    token, PK), `player_id` (FK `players` ON DELETE CASCADE, same module),
    `csrf_token`, `created_at`, `expires_at` (12 h). `player_by_session` never reads it.
  - Store: create, look up (expired = absent), delete one, delete all for a player.
    The expiry sweep next to `store.rs:1095` removes expired rows. `kill_player_families_tx`
    (`store.rs:1022-1026`) and `revoke_player_families_tx` (the operator "Revoke all",
    `admin.rs:694-700`) also delete the player's browser sessions.
  - Tests: a browser token never authenticates an op (`player_by_session` misses);
    deleting the player removes its rows; Revoke all removes them; expired rows are
    swept and not returned.
- (d) `[opus]` core-implementer.

**Step 4b: Hosted login page.**
- (a) `modules/accounts/src/web/{mod.rs,templates/}`, `modules/accounts/src/lib.rs`
  (unconditional `ctx.mount` for `/accounts/oidc`), `modules/accounts/Cargo.toml`
  (`minijinja`, `httpmw`).
- (c)
  - Cookie `swarog_session`: `HttpOnly`, `Path=/accounts/oidc`, `SameSite=Lax` (roadmap
    Step 3's authorize is a cross-site top-level GET; `Strict` would drop it).
  - One `Secure` rule for every accounts cookie: `ACCOUNTS_COOKIE_SECURE`, default on,
    `0` opts out with a loud warn (the `ADMIN_COOKIE_SECURE` precedent). Epic's
    scheme-derived flag (`epic_oauth.rs:49,56-76`) is replaced by it.
  - Routes: `GET /accounts/oidc/login`, `POST /accounts/oidc/login`,
    `POST /accounts/oidc/logout` (session + CSRF), `GET /accounts/oidc/` (signed-in
    landing: handle, logout).
  - Login CSRF: a pre-session double-submit cookie `swarog_login_csrf`
    (`Path=/accounts/oidc`, `SameSite=Strict`, 10 min), compared in constant time.
  - `return_to`: a path starting with `/accounts/oidc/`, query string kept (Step 3's
    authorize parameters ride on it), at most 2048 bytes, no scheme, no `//`, no
    backslash; anything else lands on the landing page.
  - POST calls Step 1's `verify_password` (shared caps, decoy timing, argon permit) plus
    a per-IP limiter via `httpmw` + `TRUSTED_PROXY_CIDRS` (Step 2 sets it in the split).
    One generic error for unknown email and wrong password.
  - Security headers on this router only (CSP without inline script,
    `frame-ancestors 'none'`, `no-store`), copied from `core/admin`.
  - Templates embedded, styled like `core/admin`'s login (owner question 2).
  - Tests: login sets both cookie attributes; logout without CSRF is 403;
    `return_to=//evil` and `return_to=https://x` land on the landing page; a valid
    `return_to` keeps its query; two client IPs behind a trusted proxy have separate
    limiter buckets.
- (d) `[opus]` core-implementer.

**Step 5: Epic on the hosted login page.**
- (a) `modules/accounts/src/{epic_oauth.rs,lib.rs}` (`oauth_states` at `lib.rs:186-197`,
  `external_login` at `lib.rs:381`), `web/` templates and router.
- (b) The roadmap puts the configured federated providers on the page.
- (c)
  - `oauth_states` gets one `intent` column (`login` | `link` | `browser`) in place
    of the empty-`session_token` encoding, plus `return_to`. Wipe: the table is
    DROPped and recreated (DROP + boot fresh); no ALTER.
  - The login page shows "Continue with Epic" only when Epic is configured: a form
    `POST /accounts/oidc/epic` (login CSRF, `return_to`) on Step 4b's router writes a
    `browser` state row and answers 303 to the Epic authorize URL. No script.
    The existing `/accounts/epic/start` (JSON for the game client) is unchanged.
  - The callback branches on `intent`: `browser` resolves or creates the player
    through an `external_login` variant that issues no player session, creates a
    `browser_sessions` row, sets `swarog_session` and 303s to the stored
    `return_to`. `login`/`link` behave as today.
  - Tests: browser intent sets `swarog_session` and issues no player session or
    refresh token; login and link intents unchanged.
- (d) `[opus]` core-implementer.

**Step 6: Operator-created accounts and password reset.**
- (a) `modules/accounts/src/admin.rs` (`PageHead` "Create account" on `players`; entity
  action "Reset password"), `store.rs`, `admin_tests.rs`, `conformance.rs`,
  `tools/admindiff/accept/`: `players.txt` (reason text naming "(dev)" at `:27`), the
  user menus `players.menu.user.txt`, `player.menu.user.txt`, `player.*.menu.user.txt`.
- (b) Roadmap owner question 2: operator-created accounts come first. Without a reset,
  a forgotten password has no recovery until 1b.
- (c)
  - Create: fields email and display name; `prepare` returns the `FormSpec`; `invoke`
    calls `normalize_email`, generates a 20-character random password, hashes it on the
    argon permit, inserts the player + `password` identity and emits
    `player.registered` in one tx (the `register_hashed_with_token` core without the
    session), and returns the password once as `SecretReveal`. A taken email is
    `field_error(email)`.
  - Reset: only for players with a `password` identity; a new random password, the
    same reveal, and in the same tx `kill_player_families_tx` (sessions, refresh tokens,
    browser sessions after 4a).
  - Both run on the `cp.*` face, so the split works through admin-svc unchanged.
  - The player tab's Sessions panel also lists the player's live browser sessions
    (created, expires; no token or digest shown), next to the refresh families
    (owner decision 6).
  - Tests per apikeys: the reveal is shown once; field error on duplicate (other case
    too); after a reset the old refresh token fails and the browser session is gone;
    conformance input caps.
- (d) Backend `[opus]` core-implementer; both actions render through the existing
  generic form modal and entity menu, so no view part unless owner question 2 asks for a
  mockup.

**Step 7: Split proof.**
- (a) `tools/splitproof/src/main.rs` next to the Epic passthrough proof (`:4426-4449`)
  and the accounts admin checks.
- (b) Proves the at-risk topology: page and cookie through gateway-svc → accounts-svc.
- (c) Assertions: `GET {g}/accounts/oidc/login` 200 with `swarog_login_csrf`; `POST`
  with an operator-created account (created over `cp.invoke`) 303 with `swarog_session`
  (`Path=/accounts/oidc`, `HttpOnly`); that cookie value as a bearer on an op is 401;
  `GET {g}/accounts/oidc/` shows the handle; logout clears it; a failed-login burst
  from one `X-Forwarded-For` client does not lock out a second one (the split
  `TRUSTED_PROXY_CIDRS`). Each assertion is shown failing against a reverted Step 3,
  4b, or 2's CIDR line.
- (d) `[test-author]` opus.

**Step 8: Docs.**
- (a) `.claude/rules/domain-modules.md:22-23` (accounts and gateway bullets),
  `docs/reference/hetzner-deploy-checklist.md:39`, `docs/reference/weles-fleet-parity.md:129`,
  `docs/roadmap/feature-tracker.md:144`, `.agents/skills/architecture-review/SKILL.md:86`,
  `.agents/skills/split-topology-debugger/SKILL.md:32,84`, `docs/todo/accounts.md`
  (1b; the Google redirect flow on the hosted page as the next phase, owner decision 4;
  a `UILayout/` mockup of the hosted login page, owner decision 8).
- (b) Closes the step; archives (`docs/{plans,reviews,research,design,status}`) are not
  rewritten.
- (c) Ends with `rg ACCOUNTS_DEV_AUTH docs/reference docs/roadmap .claude .agents` empty.
- (d) `[docs]` docs-writer sonnet.

Verification per step follows the `safe-verification` skill (one rollout at a time);
each code step runs only its own crates' `cargo test` and clippy; one `verifyctl --fast` and the split
proof run once, after Step 6, as part of Step 7 (owner, 2026-11-25).

### Owner decisions (2026-11-25)

1. **Self-registration until 1b:** accepted ("ok"). `/accounts/register` stays behind
   `ACCOUNTS_OPEN_REGISTRATION` until 1b.
2. **Mockup:** "niech bedzie generyczny pozniej zrobimy mockup". The login page copies
   `core/admin`'s login look; create and reset use the generic form modal and entity
   menu. A mockup comes later.
3. **Operator password reset:** "to dopiszmy" — in Step 6 as written.
4. **Google on the login page:** option B, "zrobimy ale po zakonczeniu tego planu, jako
   osobny krok albo kroki". Not in this plan; Step 8 records it in `docs/todo/accounts.md`
   as the next phase (server-side Google OIDC redirect flow on the hosted page).
5. **Starter currency for studio users:** accepted (the owner's "8. tak"; read as the
   fifth question, the plan had no eighth).
6. **Browser sessions on the player tab:** "dopisz" (after Step 4a). Step 6 adds them to
   the Sessions panel.
7. **Red `verifyctl` stages outside this plan** (config clippy/archcheck, inventory
   archcheck, audit, admindiff, core/admin poison test, processctl shutdown test,
   scheduler `SH2`/`SH2m`): "zostaw" — not this plan's work.
8. **Login page mockup:** "po planie dodaj te strone do mockupy zeby bylo mniej wiecej
   synced". After this plan, the hosted login page (Steps 4b and 5) gets a mockup under
   `UILayout/` that roughly matches the shipped page. Step 8 records it in
   `docs/todo/accounts.md`.
