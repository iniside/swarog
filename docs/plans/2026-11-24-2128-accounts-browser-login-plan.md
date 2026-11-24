# Accounts: browser login for studio users (lore roadmap step 1)

Status: waiting for approval

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
(roadmap owner question 2). No OIDC endpoints yet (Steps 2–4). Weles is untouched
beyond the env names its fleet files already carry.

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

**Step 1: Password identity and the split gate.**
- (a) `api/accounts/events/src/lib.rs` (`providers::DEV` → `providers::PASSWORD = "password"`),
  `modules/accounts/src/{providers.rs,lib.rs,store.rs,admin.rs,ops.rs}`, the
  accounts tests that set `dev_auth` (`tests.rs`, `tests/{delete,directory,guest,dev_auth_gate}.rs`,
  `conformance.rs`, `oidc_tests.rs`), `modules/gateway/src/verifier.rs`,
  `modules/gateway/src/{lib.rs,tests.rs}` mentions, public-api baseline
  `accountsevents.txt`.
- (b) Every later step signs in with this identity.
- (c)
  - Rename the field `Service.dev_auth` to two typed booleans read once in
    `register()`: login is always on (no flag); `register` is gated by
    `ACCOUNTS_OPEN_REGISTRATION` (explicit-only, default off, loud warn) until Step 1b
    replaces it with verified self-registration (owner question 1).
  - Email: trimmed and lowercased (ASCII lowercase of the whole address) at the one
    boundary `register`/`login`/operator-create share, before the advisory lock key
    and the PK; the existing length cap applies to the normalised value.
  - Gateway: `dev_auth_explicitly_on` reads `GATEWAY_DEV_VERIFIER` instead; the
    stale "default-ON convenience" comment goes.
  - Wipe: existing `provider='dev'` rows are not migrated (DROP + boot fresh).
  - Tests: login without any env succeeds; register without the flag is 404 and with
    it 201; `A@X.io` registered then `a@x.io` login succeeds and a second register
    is 409; the gateway picks `DevSessionVerifier` only on `GATEWAY_DEV_VERIFIER=1`.
- (d) `[opus]` core-implementer.

**Step 2: Env and tooling sweep.**
- (a) `ACCOUNTS_DEV_AUTH` → `ACCOUNTS_OPEN_REGISTRATION` (accounts processes and the
  monolith) and `GATEWAY_DEV_VERIFIER` (only where a gateway runs without accounts):
  `tools/processctl/src/fleet.rs:26,47,911,943,996,1021` (allowlists and builders;
  `MONOLITH_BASE_ENV` is a fixed-size array), `tools/processctl/src/fleet_tests.rs:53,63`,
  `tools/devctl/src/tests.rs:191,210,231`, `weles/fleet.monolith.toml:33`,
  `weles/fleet.split.toml:49`, `deploy/gen-3/fleet.toml:49`, `compose.dev.yaml:25`,
  `weles/master/src/manifest_tests.rs:113,365`, `tools/verifyctl/src/stages/csharp.rs:224`,
  `tools/routecheck/src/main.rs:109`, `cmd/gateway-svc/tests/tls_front.rs:192`
  (→ `GATEWAY_DEV_VERIFIER`), `cmd/gateway-svc/src/main.rs:15`,
  `tools/demoseed/src/{report.rs:249,report_tests.rs:97,lib.rs:118}` (the hint names
  the new flag).
- (b) Step 1 changed the names; nothing boots right until the tooling follows.
- (c) The sites above are a grep lower bound: the step starts with a repo-wide
  `rg ACCOUNTS_DEV_AUTH` over code and live docs (not `docs/{plans,reviews,research,design,status}`,
  which are archives) and ends with that grep empty outside archives. splitproof
  check A5 (`tools/splitproof/src/main.rs:4415`) keeps its meaning: gateway-svc never
  sets `GATEWAY_DEV_VERIFIER`; its message is updated.
- (d) `[sonnet]` core-implementer (mechanical).

**Step 3: One passthrough entry for both accounts browser prefixes.**
- (a) `cmd/gateway-svc/src/addrs.rs` (`AddrClass::Passthrough { prefixes: &'static [&'static str] }`,
  the accounts row carries `["/accounts/epic", "/accounts/oidc"]`, `to_wiring` calls
  `with_passthrough` once per prefix), `addrs_tests.rs`, the table doc comment.
- (b) Step 4's pages must be reachable through gateway-svc before they exist in the
  split proof.
- (c) The address count stays ten; managed mode (`addrs.rs:425` `exactly_one`,
  `tools/verifyctl/src/stages/weles_managed_gateway.rs:61`) resolves one origin per
  provider as today. Test: the wiring holds both prefixes for one origin, and
  `/accounts/oidcx` does not match.
- (d) `[opus]` core-implementer.

**Step 4: Browser session and the hosted login page.**
- (a) `modules/accounts/src/web/` (new: `mod.rs` router, `session.rs`, `templates/`),
  `modules/accounts/src/lib.rs` (schema + unconditional `ctx.mount`), `store.rs`
  (session rows; `delete_account` and the expiry sweep at `store.rs:1026,1095`),
  `modules/accounts/Cargo.toml` (`minijinja`, `httpmw`).
- (b) Steps 2–4 of the roadmap (authorize, device approval) need a signed-in browser.
- (c)
  - Table `accounts.browser_sessions`: `token_digest` (SHA-256 of the opaque
    32-byte token, PK), `player_id` (FK `players` ON DELETE CASCADE, same module),
    `csrf_token`, `created_at`, `expires_at` (12 h). Never readable by
    `player_by_session`.
  - Cookie `swarog_session`: `HttpOnly`, `Path=/accounts/oidc`, `SameSite=Lax`
    (Step 3's authorize is a cross-site top-level GET; `Strict` would drop it),
    `Secure` unless `ACCOUNTS_COOKIE_SECURE=0` (loud warn, the admin precedent).
  - Routes: `GET /accounts/oidc/login`, `POST /accounts/oidc/login`,
    `POST /accounts/oidc/logout` (session + CSRF), `GET /accounts/oidc/` (signed-in
    landing: handle, logout).
  - Login CSRF: a pre-session double-submit cookie `swarog_login_csrf`
    (`Path=/accounts/oidc`, `SameSite=Strict`, 10 min) compared in constant time with
    the form field.
  - `return_to`: accepted only as a path starting with `/accounts/oidc/`, no scheme,
    no `//`, no backslash; otherwise the landing page. No open redirect.
  - The POST calls the same verification as the `login` op (Step 1), so caps, decoy
    timing and the argon permit are shared; plus a per-IP limiter resolved via
    `httpmw` + `TRUSTED_PROXY_CIDRS` (the gateway in the split must be inside it; the
    fleet sets it in Step 2's files). One generic error for unknown email and wrong
    password.
  - Security headers on this router only (CSP without inline script, `frame-ancestors 'none'`,
    `no-store`), copied from `core/admin`.
  - Templates embedded, styled like `core/admin`'s login (owner question 2).
  - Tests: cookie session never authenticates an op; logout without CSRF is 403;
    `return_to=//evil` lands on the landing page; deleting the player removes its
    browser sessions; expired sessions are swept.
- (d) `[opus]` core-implementer.

**Step 5: Epic on the hosted login page.**
- (a) `modules/accounts/src/epic_oauth.rs`, the `oauth_states` table (`lib.rs:197`),
  the login template.
- (b) The roadmap puts the configured federated providers on the page; Epic is the
  only one with a server redirect flow.
- (c) `oauth_states` gains `intent` (`player` | `browser`) and `return_to`. The
  login page shows "Continue with Epic" only when Epic is configured; it posts to
  `/accounts/epic/start` with `intent=browser`. A browser-intent callback creates a
  `browser_sessions` row (Step 4) instead of a player session and redirects to the
  validated `return_to`. The player intent is unchanged. Google stays off the page
  (no redirect flow); it is listed as a leftover, not built.
  Tests: browser intent sets `swarog_session` and issues no player session; player
  intent unchanged.
- (d) `[opus]` core-implementer.

**Step 6: Operator-created accounts and password reset.**
- (a) `modules/accounts/src/admin.rs` (a `PageHead` "Create account" on `players`;
  a `RowAction`/entity action "Reset password"), `store.rs`, `admin_tests.rs`,
  `conformance.rs`, `tools/admindiff/accept/players*.txt`.
- (b) Owner question 2 of the roadmap: operator-created accounts come first. Without
  a reset, a forgotten password has no recovery until 1b.
- (c)
  - Create: fields email and display name; `prepare` returns the `FormSpec`;
    `invoke` normalises the email, generates a 20-character random password, hashes
    it on the argon permit, inserts the player + `password` identity and emits
    `player.registered` in one tx (the `register_hashed_with_token` core without the
    session), and returns it once as `SecretReveal`. A taken email is
    `field_error(email)`.
  - Reset: new random password, same reveal, and in the same tx deletes the player's
    `browser_sessions` and `sessions`. Only for players with a `password` identity.
  - Both run on the `cp.*` face, so the split works through admin-svc unchanged.
  - Tests per apikeys: reveal shown once, field error on duplicate, reset revokes
    sessions; conformance input caps.
- (d) Backend `[opus]` core-implementer; the head action renders with the existing
  generic form modal, so no view part unless owner question 2 asks for a mockup.

**Step 7: Split proof.**
- (a) `tools/splitproof/src/main.rs` next to the Epic passthrough proof (`:4426-4449`)
  and the accounts admin checks.
- (b) Proves the at-risk topology: the page and cookie through gateway-svc → accounts-svc.
- (c) Assertions: `GET {g}/accounts/oidc/login` 200 with `swarog_login_csrf`;
  `POST` with an operator-created account (created over `cp.invoke`) 303 with
  `swarog_session` (`Path=/accounts/oidc`, `HttpOnly`); that cookie value as a bearer
  on an op is 401; `GET {g}/accounts/oidc/` shows the handle; logout clears it.
  Each assertion is shown failing against a reverted Step 3 or 4.
- (d) `[test-author]` opus.

**Step 8: Docs.**
- (a) `.claude/rules/domain-modules.md` (accounts and gateway bullets),
  `docs/reference/hetzner-deploy-checklist.md:39`, `docs/reference/weles-fleet-parity.md:129`,
  `docs/roadmap/feature-tracker.md` (accounts row), `.agents/skills/architecture-review/SKILL.md:86`,
  `.agents/skills/split-topology-debugger/SKILL.md:32,84`, `docs/todo/accounts.md`
  (Google on the hosted page; 1b).
- (b) Closes the step; archives are not rewritten.
- (d) `[docs]` docs-writer sonnet.

Verification per step follows the `safe-verification` skill (one rollout at a time);
Steps 3 and 7 end with the split proof, Steps 1, 4, 6 with `verifyctl --fast`.

## Owner questions

1. **Self-registration until 1b.** Proposal: login is always on; `/accounts/register`
   stays off in production behind `ACCOUNTS_OPEN_REGISTRATION` (dev fleets, demoseed
   and the C# stage set it) until 1b adds verified registration. OK?
2. **No mockup for the hosted login page or the create-account form.** Proposal: the
   login page copies `core/admin`'s login look, and the create form is the generic
   form modal apikeys uses. Or a mockup first?
3. **Operator password reset in Step 6.** Not named in the roadmap; proposed because
   otherwise a forgotten password cannot be recovered before 1b. OK?
