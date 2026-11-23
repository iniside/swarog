# Apikeys

Last updated: 2026-11-23

## In progress

_None._

## Open

- [ ] **`conformance_key_rejected` executes no production code**
  APIKEYS-06. modules/apikeys/src/conformance.rs:16 is `len > MAX_KEY_BYTES`. Source:
  modules/apikeys/src/conformance.rs:16 (2026-07-12); see
  docs/status/2026-09-05-1416-leaderboard-shared-table-pollution-status.md:89.
- [ ] **Rotate secret**
  APIKEYS-09 (part of APIKEYS-03). The key row menu draws "Rotate secret…" disabled: `key.rotate` is declared
  `implemented: false` (modules/apikeys/src/admin.rs:845) and answers "not implemented yet"
  (:919). The store has no rotate operation that replaces a key's digest and prefix under
  its revision. Replaced by: a revision-checked rotate that shows the new plaintext once.
- [ ] **Key expiry**
  APIKEYS-10 (part of APIKEYS-03). Keys never expire: `apikeys.keys` has no expiry column, so the create form's
  Expires offers only "Never" (owner decision D4,
  docs/plans/2026-11-23-step21-apikeys-checklist.md:407). Replaced by: a stored expiry the
  verifier enforces, and real Expires options.
