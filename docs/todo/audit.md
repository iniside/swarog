# Audit

Last updated: 2026-10-23

## In progress

_None._

## Open

- [ ] **`admin_data` must never return `Err`**
  AUDIT-02. modules/audit/src/lib.rs:264 does `admin_content().await.map_err(internal)?`,
  which collapses the page to its id (see ADMIN-05). Source:
  modules/audit/src/lib.rs:263 (2026-07-08); docs/plans/2026-09-05-1921-friends-module-plan.md:593.
- [ ] **Analytics and telemetry event pipeline**
  AUDIT-05. Nothing in modules/ implements one (Grep, lower bound). Source:
  docs/roadmap/feature-tracker.md:253 (2026-07-22).
