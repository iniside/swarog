# Apikeys

Last updated: 2026-10-23

## In progress

_None._

## Open

- [ ] **Role-policy editor is a blind full replace**
  APIKEYS-01. `policy_method_options` renders every checkbox unchecked
  (modules/apikeys/src/admin.rs:152), so saving replaces the whole policy. Source:
  modules/apikeys/src/admin.rs:121 (2026-07-14); see
  docs/plans/2026-07-14-1421-apikeys-configurator-plan.md:316.
- [ ] **`conformance_key_rejected` executes no production code**
  APIKEYS-06. modules/apikeys/src/conformance.rs:16 is `len > MAX_KEY_BYTES`. Source:
  modules/apikeys/src/conformance.rs:16 (2026-07-12); see
  docs/status/2026-09-05-1416-leaderboard-shared-table-pollution-status.md:89.
