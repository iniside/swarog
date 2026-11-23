# Apikeys

Last updated: 2026-11-23

## In progress

_None._

## Open

- [ ] **Role-policy editor must pre-check the stored policy**
  APIKEYS-01. The old editor (`modules/apikeys/src/admin.rs`, `policy_method_options`)
  no longer exists: the admin was razed by b8435c9a, and `modules/apikeys/src` has no
  `admin.rs`. The requirement stays for its replacement: a policy `Multi` pre-checked
  from the stored policy and a revision-checked save, so saving is not a blind full
  replace. The control plane already supports a pre-checked `FieldKind::Multi` through
  `Field.default` (`core/controlplane/src/lib.rs:810-820`, default at :816;
  `core/admin/src/render/form.rs:72-78`).
  It belongs to step 21 (apikeys) of
  docs/plans/2026-10-22-1759-admin-control-plane-redesign-plan.md, whose text does not
  yet name it.
- [ ] **`conformance_key_rejected` executes no production code**
  APIKEYS-06. modules/apikeys/src/conformance.rs:16 is `len > MAX_KEY_BYTES`. Source:
  modules/apikeys/src/conformance.rs:16 (2026-07-12); see
  docs/status/2026-09-05-1416-leaderboard-shared-table-pollution-status.md:89.
- [ ] **Rotate secret**
  APIKEYS-07. The key row menu draws "Rotate secret…" disabled: `key.rotate` is declared
  `implemented: false` (modules/apikeys/src/admin.rs:408) and answers "not implemented yet"
  (:453). The store has no rotate operation that replaces a key's digest and prefix under
  its revision. Replaced by: a revision-checked rotate that shows the new plaintext once.
- [ ] **Key expiry**
  APIKEYS-08. Keys never expire: `apikeys.keys` has no expiry column, so the create form's
  Expires offers only "Never" (owner decision D4,
  docs/plans/2026-11-23-step21-apikeys-checklist.md:407). Replaced by: a stored expiry the
  verifier enforces, and real Expires options.
