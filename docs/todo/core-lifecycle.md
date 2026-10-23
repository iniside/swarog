# Core lifecycle

Last updated: 2026-10-23

## In progress

_None._

## Open

- [ ] **Generic bound on Module::start**
  CORE-LIFECYCLE-01. Only `stop` is time-bounded; `App::start` awaits
  `m.start(&self.ctx)` with no timeout (`core/lifecycle/src/app.rs:241`, read at
  HEAD; docs/reviews/2026-07-13-2138-audit-remediation-status.md:68,
  2026-07-13).
