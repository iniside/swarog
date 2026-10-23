# Core app

Last updated: 2026-10-23

## In progress

_None._

## Open

- [ ] **Floor or clamp for MODULE_STOP_GRACE_MS**
  CORE-APP-01. `core/app/src/lib.rs:352-357` parses the env var with no floor, so
  a value below a module's inner stop timeout (gateway 2s, scheduler and party
  4s, remote `PROBE_STOP_GRACE`) drops the module's `stop` future and leaves its
  tasks detached. The fix belongs in `core/app` (a floor, or a published
  minimum) (`modules/gateway/src/lib.rs:237-244`, 2026-07-28).
