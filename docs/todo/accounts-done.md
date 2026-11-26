# Accounts

Last updated: 2026-11-26

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
