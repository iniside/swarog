# Core app

Last updated: 2026-11-26

## In progress

_None._

## Open

- [ ] **A hung database answers 408, not 503**
  The production pool uses sqlx's default 30 s acquire timeout (core/app/src/lib.rs:841), the same as the
  30 s HTTP `TimeoutLayer`. So when the database hangs, rather than failing fast, the timeout layer answers
  408 before a module can map the DB error to 503 (for example the accounts JWKS, oidc-signing-keys D5).
  Fix: an app-wide pool acquire timeout shorter than the request timeout, not a per-route one.
  Source: core-reviewer of 1148ffe6 (oidc-signing-keys step 3, 2026-11-26).

- [ ] **Floor or clamp for MODULE_STOP_GRACE_MS**
  CORE-APP-01. `core/app/src/lib.rs:352-357` parses the env var with no floor, so
  a value below a module's inner stop timeout (gateway 2s, scheduler and party
  4s, remote `PROBE_STOP_GRACE`) drops the module's `stop` future and leaves its
  tasks detached. The fix belongs in `core/app` (a floor, or a published
  minimum) (`modules/gateway/src/lib.rs:237-244`, 2026-07-28).
