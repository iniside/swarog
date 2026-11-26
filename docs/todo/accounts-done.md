# Accounts

Last updated: 2026-11-26

- [x] **The boot refusal on an old `oauth_states` table has no test** (2026-11-26, google-browser-login step 1)
  `tests::oauth_flow::boot_refusal_fires_on_oauth_states_without_provider` runs the
  schema-parameterized refusal against a scratch schema whose `oauth_states` lacks `provider`.
  Source: docs/plans/2026-11-24-2128-accounts-browser-login-plan.md, deferred review leftover (2026-11-26).
