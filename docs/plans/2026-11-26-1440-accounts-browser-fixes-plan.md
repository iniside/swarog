# Accounts browser sign-in fixes and cleanups

Status: waiting for approval
Date: 2026-11-26
Source: owner, on the Known limits list in docs/todo/accounts.md: "cos mi sie wydaje, ze czesc z nich to
zwyczajnie bugi to naprawy" and "ok to zrob plan razem z cleanupem" (2026-11-26).
Plan review: core-reviewer (opus), ACCEPT with changes; folded in below.
Sequencing (needs the owner's OK, since that plan was approved to run straight through): after Step 1 of
docs/plans/2026-11-26-1415-oidc-signing-keys-plan.md (the launchers, landed as bcd91a64) and before its Step 2 (Step 2 edits
`modules/accounts/src/lib.rs` too). After this plan, oidc Steps 2 and 3 put any new env read in the env half of
`init` (D3) and any new route in its body, so the new `init` test keeps covering them. Step 4 here edits
`tools/processctl/src/fleet.rs`, so it waits for that Step 1 commit.

## Problem (verified 2026-11-26)

Bugs:
1. Replacing a browser session isn't atomic. `login_submit` (modules/accounts/src/web/mod.rs:416-421)
   deletes the session the browser held and only then calls `Service::issue_password_browser_session`
   (modules/accounts/src/lib.rs:462-490), which can refuse, e.g. when the password-identity stamp check fails
   (:478-485). After that refusal the player has lost their old session and has no new one. `browser_sign_in`
   (modules/accounts/src/oauth_flow.rs:419-433) does the same before `create_browser_session`, which
   returns `None` for a player deleted meanwhile. Both deletes run on the pool, outside any transaction
   (`Store::delete_browser_session_digest`, modules/accounts/src/web/session.rs:143-149).
2. The Google callback (oauth_flow.rs:322-340) and the Epic callback (modules/accounts/src/epic_oauth.rs:96-105)
   answer with no `Cache-Control: no-store`, though their success responses set the session cookie.
   Only the hosted pages send it (modules/accounts/src/web/mod.rs:281).
3. No test serves the router `init` builds. `init` (lib.rs:1633, mount at :1705) reads the provider config from env
   (`ProviderConfig::from_env`, providers.rs:248) and passes the flows to `web::hosted_browser_router`
   (:1705). Every browser test builds its own flows, so dropping a flow in `init`, or adding a layer in
   `init` around the builder's result, keeps every test green.

Cleanups:
4. `google.oauth` is tested twice to decide whether the browser flow exists:
   `GoogleConfig::browser_flow` (providers.rs:195) and `init` (lib.rs:1696, which only logs).
5. Three test helpers hard-code the sign-in bucket as `5.0, 20`: modules/accounts/src/tests.rs:1853,
   modules/accounts/src/tests/hosted_login.rs:24, modules/accounts/src/tests/epic_browser.rs:61. google_browser.rs:111-112 already uses
   `accountsapi::LOGIN_LIMIT_PER_SEC`/`LOGIN_LIMIT_BURST`.
6. The split gateway port 8082 is written out at tools/processctl/src/fleet.rs:863 and :891, in the callback
   URIs at fleet.rs:999 and :1017, fleet_tests.rs:151 (the gateway spec row), :208, :335 and :374, and
   tools/splitproof/src/accounts_browser_checks.rs:73. oidc-signing-keys Step 1 adds the split proof issuer
   `http://127.0.0.1:8082/accounts/oidc` as a further copy. splitproof already depends on processctl
   (tools/splitproof/Cargo.toml:16).

Not a gap:
7. The TODO item "the `browser_router` non-browser 500 arm has no test": the arm (oauth_flow.rs:332-333)
   is unreachable, because the `oauth_states_epic_only` CHECK (lib.rs:269) refuses any non-`browser` intent for a provider
   other than Epic, and `browser_router` serves only Google. That CHECK is tested in
   tests/oauth_flow.rs:50-64. Besides, `take_state` filters on `provider = $3` (oauth_flow.rs:191) and nothing
   writes a Google login or link state, so even a stale table without the CHECK (`refuse_stale_oauth_states`,
   lib.rs:282-300, checks columns only) cannot reach the arm. The item closes as obsolete with both reasons;
   no code changes.

Stays open: `[BG3]` has never been seen to fail (watch it during oidc-signing-keys Step 4's split proof run); the old
`/accounts/epic` binding cookie coexisting for up to 10 minutes (it expires on its own, no harm).

## Decisions

- D1. One helper on the caller's connection, `Store::delete_browser_session_digest_tx`, deletes the old
  session; both callers run it in one transaction with the mint, and LAST: insert the new session
  (`create_browser_session_tx`, session.rs:54), then the stamp check on the password path, then the delete.
  Every login takes the player row (FOR KEY SHARE through the insert's foreign key) as its first lock
  (store.rs:1001-1006, :1048-1052), while `kill_player_families_tx` (store.rs:1056-1070) and
  `delete_player_atomic` (lib.rs:980+) lock the player row FOR UPDATE and then delete its sessions; deleting
  the old session first would hold a session row while waiting for the player row and can deadlock.
  `browser_sign_in` opens the transaction itself; `login_submit` passes the old session's digest into
  `issue_password_browser_session`, which deletes it inside its existing transaction. If the mint or the
  stamp check refuses, the transaction rolls back and the old session survives. The responses are
  unchanged. `Store::create_browser_session` (session.rs:44) loses its only production caller
  (oauth_flow.rs:425): it becomes `#[cfg(test)]`, kept for its test callers.
- D2. Both callback routes answer every response, success and error, with `Cache-Control: no-store`, set
  where each callback route is built. The hosted pages' other headers (CSP etc.) do not spread to the
  callbacks.
- D3. `init` splits into the env read and an `init` body that takes the parsed inputs (provider config,
  `cookie_secure`, trusted proxies). The real `init` reads and parses all env before the body's first side
  effect (`web_limiter.set`, `ctx.mount`), as today, and calls the body; no test mutates process env. A new
  test runs `register`, then the body with both flows configured through `ProviderConfig::from_vars`
  (providers.rs:268), on `Context::with_db_and_transport` (the `wired` helper, tests.rs:659; a bus with no
  transport panics on the body's durable subscription, core/bus/src/lib.rs:406-411), and serves
  `ctx.take_router()` (core/lifecycle/src/context.rs:145). It empties the sign-in bucket through
  `Accounts.web_limiter` (lib.rs:1474).
  It asserts:
  - the Epic start, the Epic callback and the Google callback are routed (not 404);
  - with the sign-in bucket empty, the callbacks answer non-429 and carry no CSP header;
  - the callbacks carry `Cache-Control: no-store`.
- D4. `init` asks the built Google flow, not `google.oauth`, whether to log "google OAuth enabled"; one
  check remains.
- D5. The three test helpers use `accountsapi::LOGIN_LIMIT_PER_SEC`/`LOGIN_LIMIT_BURST`.
- D6. processctl exports the split gateway origin once as a `#[macro_export]` literal macro
  (`split_gateway_origin!()` → `"http://127.0.0.1:8082"`), because a `const &str` cannot be formatted from a
  `u16` and `concat!` takes only literals. The callback URIs and the split proof issuer from
  oidc-signing-keys Step 1 (`PROOF_ACCOUNTS_ISSUER_SPLIT`) become `concat!(split_gateway_origin!(), …)`
  consts, so their public API is unchanged. The gateway's port (fleet.rs:863 `":8082"`, :891 `8082`) is
  pinned to the origin by a fleet test. fleet.rs, fleet_tests.rs and splitproof use it. The weles tomls, the
  other non-Rust copies, and `modules/accounts/src/providers_tests.rs:492` and `tests.rs:1891` (a module
  must not depend on a tool crate) stay as they are.

## Steps

### Step 1 [core-implementer, opus, high]: replace a browser session in one transaction

D1. Files: modules/accounts/src/web/session.rs, modules/accounts/src/web/mod.rs, modules/accounts/src/lib.rs
(`issue_password_browser_session` only), modules/accounts/src/oauth_flow.rs, accounts tests.
Invariants:
- A sign-in that does not mint a session leaves the old session alive in both callers.
- A sign-in that mints one leaves no row for the old session.
- "Old session" means a session the browser holds for ANOTHER player (a session of the deleted player goes
  with the cascade, lib.rs:240), as the existing tests use (tests/epic_browser.rs:153, tests/google_browser.rs:172).
- Tests reproduce each refusal and fail against the current code: on the password path, a stamp refusal
  with the identity row held FOR UPDATE (as admin_tests.rs:715); on the federated path, the player row held
  FOR UPDATE so the callback parks on the insert's foreign-key lock, then the player deleted and committed.
- The delete runs after the insert and the stamp (D1 lock order).
At-risk tests: tests/hosted_login.rs:258, tests/epic_browser.rs:153, tests/google_browser.rs:172, the
`create_browser_session` callers in tests/browser_session.rs.
Verify: `cargo test -p accounts` (live local Postgres, one rollout), `cargo clippy -p accounts --all-targets`.

### Step 2 [core-implementer, opus, high]: no-store on the callbacks, the router `init` builds under test

D2, D3, D4. Files: modules/accounts/src/lib.rs, modules/accounts/src/oauth_flow.rs,
modules/accounts/src/epic_oauth.rs, modules/accounts/src/web/mod.rs if the builder needs it, accounts tests.
Invariants:
- Every response from both callbacks carries `Cache-Control: no-store`, the error ones included.
- `init` serves the same routes and layers as today; the new test goes red if a flow is dropped in the body
  or if a limiter or header layer wraps the builder's result.
- No test sets or removes a process env variable.
- A malformed provider value still fails `App::build()` (conformance T6, tools/conformance/src/policy.rs:216-258),
  and the five `register`/`init`-only gates still pass with no accounts env.
Verify: `cargo test -p accounts`, then `cargo test -p conformancecheck -p routecheck -p topiccheck -p requirecheck
-p checkmodules`, `cargo clippy -p accounts --all-targets`.

### Step 3 [core-implementer, sonnet, medium]: test helpers use the shared bucket constants

D5. Files: modules/accounts/src/tests.rs, tests/hosted_login.rs, tests/epic_browser.rs.
Verify: `cargo test -p accounts`, `cargo clippy -p accounts --all-targets`.

### Step 4 [core-implementer, opus, medium]: one split gateway port constant

D6. After oidc-signing-keys Step 1 has landed. Files: tools/processctl/src/fleet.rs,
tools/processctl/src/fleet_tests.rs, tools/processctl/src/lib.rs, tools/splitproof/src/accounts_browser_checks.rs
and any other processctl or splitproof copy of the origin a fresh grep for `8082` finds (comments, log strings
and demoseed help text are not consumers).
Invariant: the fleet env and the proof constants are byte-identical to today's.
Verify: `cargo test -p processctl`, `cargo clippy -p processctl -p splitproof --all-targets`.

### Step 5 [main session, docs]

Move the fixed items and item 7 (obsolete, with its reason) from docs/todo/accounts.md to
docs/todo/accounts-done.md. Leave `[BG3]` and the Epic binding cookie open. Mark this plan done.

## Not covered

- No split proof run: nothing here changes a route or a fleet env value, and the at-risk behaviour is
  router composition and DB transactions, tested in process. oidc-signing-keys Step 4's split run covers
  the fleet afterwards.
