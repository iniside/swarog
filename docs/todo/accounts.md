# Accounts

Last updated: 2026-11-26

## In progress

_None._

## Open

- [ ] **Link more identities to one player**
  A signed-in player can attach further identities (Google, Epic, others) from the hosted page
  whenever they want. Today Google is sign-in only and Link is Epic's JSON flow.
  Source: docs/plans/2026-11-26-1123-google-browser-login-plan.md, owner answer 2 (2026-11-26).

- [ ] **Single failure-rendering policy for the Epic OAuth callback**
  ACCOUNTS-01. The Epic callback renders a Postgres outage as a 503 body on
  `handle_callback`'s state lookup but as a `/?epic=error` redirect on
  `epic_oauth::complete`'s link-flow `player_by_session`. Source: modules/accounts/src/oauth_flow.rs:345 (state lookup)
  and modules/accounts/src/epic_oauth.rs:178 (link-flow lookup) (2026-07-28).
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
- [ ] **UILayout mockup of the hosted login page**
  Add a `UILayout/` mockup that roughly matches the shipped page, then port the page to it.
  Scheduled after roadmap Phase A (Steps 2-4) of docs/plans/2026-11-24-0004-lore-integration-roadmap-plan.md
  (owner, 2026-11-26).
  Source: docs/plans/2026-11-24-2128-accounts-browser-login-plan.md, owner decision 8 (2026-11-25).
- [ ] **No single browser-session revoke for operators**
  An operator cannot end one browser session; only Revoke all ends them.
  Source: docs/plans/2026-11-24-2128-accounts-browser-login-plan.md, owner decision 11 (2026-11-26).

### Known limits (google-browser-login step 4)

- [ ] **`[BG3]` has never been watched go red**
  No split run with a deliberately broken `provider = $3` predicate has shown
  tools/splitproof/src/accounts_browser_checks.rs `[BG3]` (:368) failing.
  Source: docs/plans/2026-11-26-1352-oauth-drift-fixes-plan.md D5, owner answer ("ok 1,2 zrobic reszta do todo", 2026-11-26).

### Known limits (accounts-browser-login step 8)

- [ ] **The old `/accounts/epic` binding cookie can coexist for up to 10 minutes**
  Source: docs/plans/2026-11-24-2128-accounts-browser-login-plan.md, deferred review leftover (2026-11-26).
