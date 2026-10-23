# Accounts

Last updated: 2026-10-23

## In progress

_None._

## Open

- [ ] **Single failure-rendering policy for the Epic OAuth callback**
  ACCOUNTS-01. `handle_callback` renders a Postgres outage as a 503 body on the
  state lookup but as a `/?epic=error` redirect on the link-flow
  `player_by_session`. Source: modules/accounts/src/epic_oauth.rs:323 (2026-07-28).
- [ ] **Facebook and console login providers**
  ACCOUNTS-03. Not present in modules/accounts/src (Grep, lower bound). Each is a
  new credential verifier, not a new op. Source: docs/roadmap/feature-tracker.md:140
  (2026-08-30).
- [ ] **GDPR data export**
  ACCOUNTS-08. No export exists in modules/accounts/src (Grep, lower bound).
  Source: docs/roadmap/feature-tracker.md:145 (2026-09-10).
- [ ] **Profile fields beyond the handle**
  ACCOUNTS-09. Avatar, language and an editable display name do not exist in
  modules/accounts/src (Grep, lower bound); the display name is immutable.
  Source: docs/roadmap/feature-tracker.md:146 (2026-09-06); see
  docs/plans/2026-09-05-1921-friends-module-plan.md:589.
- [ ] **`player.promoted` `from_provider` is the constant `guest`**
  ACCOUNTS-14. modules/accounts/src/lib.rs:562 sets `from_provider: GUEST`; only
  guest accounts are promotable today. Source: modules/accounts/src/lib.rs:562
  (2026-08-29); docs/roadmap/feature-tracker.md:150.
