# Accounts

Last updated: 2026-11-26

## In progress

_None._

## Open

- [ ] **Single failure-rendering policy for the Epic OAuth callback**
  ACCOUNTS-01. `handle_callback` renders a Postgres outage as a 503 body on the
  state lookup but as a `/?epic=error` redirect on the link-flow
  `player_by_session`. Source: modules/accounts/src/epic_oauth.rs:356 (2026-07-28).
- [ ] **Facebook and console login providers**
  ACCOUNTS-03. Not present in modules/accounts/src (Grep, lower bound). Each is a
  new credential verifier, not a new op. Source: docs/roadmap/feature-tracker.md:140 @ fd797b3a
  (2026-08-30).
- [ ] **GDPR data export**
  ACCOUNTS-08. No export exists in modules/accounts/src (Grep, lower bound).
  Source: docs/roadmap/feature-tracker.md:145 @ fd797b3a (2026-09-10).
- [ ] **Profile fields beyond the handle**
  ACCOUNTS-09. Avatar, language and an editable display name do not exist in
  modules/accounts/src (Grep, lower bound); the display name is immutable.
  Source: docs/roadmap/feature-tracker.md:146 @ fd797b3a (2026-09-06); see
  docs/plans/2026-09-05-1921-friends-module-plan.md:589.
- [ ] **`player.promoted` `from_provider` is the constant `guest`**
  ACCOUNTS-14. modules/accounts/src/lib.rs:562 sets `from_provider: GUEST`; only
  guest accounts are promotable today. Source: modules/accounts/src/lib.rs:562
  (2026-08-29); docs/roadmap/feature-tracker.md:150 @ fd797b3a.
- [ ] **Open registration (1b)**
  `ACCOUNTS_OPEN_REGISTRATION` stays (default OFF) until this lands.
  Source: docs/plans/2026-11-24-2128-accounts-browser-login-plan.md, owner decision 1 (2026-11-25).
- [ ] **Google server-side OIDC redirect flow on the hosted login page**
  The next phase after the accounts-browser-login plan.
  Source: docs/plans/2026-11-24-2128-accounts-browser-login-plan.md, owner decision 4 (2026-11-25).
- [ ] **UILayout mockup of the hosted login page**
  Add a `UILayout/` mockup that roughly matches the shipped page, then port the page to it.
  Source: docs/plans/2026-11-24-2128-accounts-browser-login-plan.md, owner decision 8 (2026-11-25).
- [ ] **No single browser-session revoke for operators**
  An operator cannot end one browser session; only Revoke all ends them.
  Source: docs/plans/2026-11-24-2128-accounts-browser-login-plan.md, owner decision 11 (2026-11-26).

### Known limits (accounts-browser-login step 8)

- [ ] **`login_submit` deletes the old browser session outside the transaction**
  Source: docs/plans/2026-11-24-2128-accounts-browser-login-plan.md, deferred review leftover (2026-11-26).
- [ ] **`browser_sign_in` and `login_submit` delete then create without one transaction**
  Source: docs/plans/2026-11-24-2128-accounts-browser-login-plan.md, deferred review leftover (2026-11-26).
- [ ] **The old `/accounts/epic` binding cookie can coexist for up to 10 minutes**
  Source: docs/plans/2026-11-24-2128-accounts-browser-login-plan.md, deferred review leftover (2026-11-26).
- [ ] **The boot refusal on an old `oauth_states` table has no test**
  Source: docs/plans/2026-11-24-2128-accounts-browser-login-plan.md, deferred review leftover (2026-11-26).
- [ ] **Sign-in limiter test helpers repeat 5.0/20**
  modules/accounts/src/tests.rs:1850, modules/accounts/src/tests/hosted_login.rs:24 and
  modules/accounts/src/tests/epic_browser.rs:60 should use `accountsapi::LOGIN_LIMIT_PER_SEC` and
  `accountsapi::LOGIN_LIMIT_BURST`.
  Source: docs/plans/2026-11-24-2128-accounts-browser-login-plan.md, deferred review leftover (2026-11-26).
