# Notifications

Last updated: 2026-10-23

## In progress

_None._

## Open

- [ ] **`match.finished` produces no inbox row**
  NOTIFICATIONS-01. modules/notifications/src has no match.finished reference (Grep,
  lower bound); winner and loser are opaque contestant strings. Same root as
  ACCOUNT-DELETION-02. Source: docs/roadmap/feature-tracker.md:244 (2026-08-31); see
  docs/plans/2026-08-31-0032-notifications-inbox-3a-plan.md:558.
- [ ] **Shared pagination grammar**
  NOTIFICATIONS-02. The cursor lives in this module's request body
  (api/notifications/api/src/lib.rs). The authority fix is query args plus `Page<T>`
  in opsapi. Source: docs/plans/2026-08-31-0032-notifications-inbox-3a-plan.md:555
  (2026-08-31).
- [ ] **Command topic for inbox rows**
  NOTIFICATIONS-04. Every module wanting an inbox row edits notifications; mail
  closed this for itself with `mail.send_requested`. Source:
  docs/plans/2026-09-05-1921-friends-module-plan.md:591 (2026-09-05).
- [ ] **`admin_data` must never return `Err`**
  NOTIFICATIONS-05. modules/notifications/src/admin.rs:457-460 maps a build error to
  `opsapi::Error::internal`. Source: modules/notifications/src/admin.rs:454
  (2026-08-31); docs/plans/2026-09-05-1921-friends-module-plan.md:593.
