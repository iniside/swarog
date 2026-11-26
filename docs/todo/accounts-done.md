# Accounts

Last updated: 2026-11-26

- [x] **The browser callback's non-browser-intent 500 has no test** (2026-11-26, accounts-browser-fixes step 5)
  `browser_router` answers 500 for a state row with a non-browser intent
  (modules/accounts/src/oauth_flow.rs:332-333); no test reaches that arm.
  Source: docs/plans/2026-11-26-1352-oauth-drift-fixes-plan.md D5, owner answer ("ok 1,2 zrobic reszta do todo", 2026-11-26).
  Closed as obsolete: the arm is unreachable. The `oauth_states_epic_only` CHECK refuses any
  non-`browser` intent for a provider other than Epic and `browser_router` serves only Google; and
  `take_state` filters on `provider`, while nothing writes a Google login or link state.
  Source: docs/plans/2026-11-26-1440-accounts-browser-fixes-plan.md item 7.
- [x] **Proof redirect URIs repeat the gateway port by hand** (2026-11-26, accounts-browser-fixes step 4)
  The split proof's `http://127.0.0.1:8082/...` callback URIs are written out at
  tools/processctl/src/fleet.rs:999 and :1017, tools/processctl/src/fleet_tests.rs:335 and :374, and
  tools/splitproof/src/accounts_browser_checks.rs:73, apart from the gateway port set at fleet.rs:863.
  Source: docs/plans/2026-11-26-1352-oauth-drift-fixes-plan.md D5, owner answer ("ok 1,2 zrobic reszta do todo", 2026-11-26).
- [x] **Sign-in limiter test helpers repeat 5.0/20** (2026-11-26, accounts-browser-fixes step 3)
  modules/accounts/src/tests.rs:1850, modules/accounts/src/tests/hosted_login.rs:24 and
  modules/accounts/src/tests/epic_browser.rs:60 should use `accountsapi::LOGIN_LIMIT_PER_SEC` and
  `accountsapi::LOGIN_LIMIT_BURST`.
  Source: docs/plans/2026-11-24-2128-accounts-browser-login-plan.md, deferred review leftover (2026-11-26).
- [x] **`google.oauth` is checked twice** (2026-11-26, accounts-browser-fixes step 2)
  The init body logs "google OAuth enabled" from the flow `GoogleConfig::browser_flow` built.
- [x] **The Google and Epic callbacks send no `Cache-Control: no-store`** (2026-11-26, accounts-browser-fixes step 2)
  Each callback route sets `no-store` on every response, errors included
  (`oauth_flow::no_store`); the pages' other headers stay off the callbacks.
- [x] **`init` alone decides which flows reach the hosted-browser router** (2026-11-26, accounts-browser-fixes step 2)
  `init` reads env, then calls `Accounts::init_with`; `tests/init_router.rs` runs `register` and
  the body with both flows from `ProviderConfig::from_vars` and serves `ctx.take_router()`. It goes red
  when a flow is dropped or a limiter or header layer wraps the builder's result.
- [x] **`login_submit` deletes the old browser session outside the transaction** (2026-11-26, accounts-browser-fixes step 1)
  The old session is deleted in the mint's transaction, after the insert and the identity stamp,
  so a refused sign-in keeps it.
  Source: docs/plans/2026-11-24-2128-accounts-browser-login-plan.md, deferred review leftover (2026-11-26).
- [x] **`browser_sign_in` and `login_submit` delete then create without one transaction** (2026-11-26, accounts-browser-fixes step 1)
  Both callers mint, then delete the old session, in one transaction; tests
  `a_sign_in_refused_at_the_stamp_keeps_the_browsers_session` (hosted_login) and
  `a_sign_in_refused_at_the_mint_keeps_the_browsers_session` (epic_browser) reproduce each refusal.
  Source: docs/plans/2026-11-24-2128-accounts-browser-login-plan.md, deferred review leftover (2026-11-26).

- [x] **Google server-side OIDC redirect flow on the hosted login page** (2026-11-26, google-browser-login step 5)
  `POST /accounts/oidc/google` and `GET /accounts/oidc/google/callback`, enabled by
  `GOOGLE_CLIENT_SECRET`. Sign-in only; scope `openid`.
  Source: docs/plans/2026-11-24-2128-accounts-browser-login-plan.md, owner decision 4 (2026-11-25).
- [x] **The boot refusal on an old `oauth_states` table has no test** (2026-11-26, google-browser-login step 1)
  `tests::oauth_flow::boot_refusal_fires_on_oauth_states_without_provider` runs the
  schema-parameterized refusal against a scratch schema whose `oauth_states` lacks `provider`.
  Source: docs/plans/2026-11-24-2128-accounts-browser-login-plan.md, deferred review leftover (2026-11-26).
