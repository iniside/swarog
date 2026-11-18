# Apikeys

Last updated: 2026-11-18

## In progress

_None._

## Open

- [ ] **Role-policy editor must pre-check the stored policy**
  APIKEYS-01. The old editor (`modules/apikeys/src/admin.rs`, `policy_method_options`)
  no longer exists: the admin was razed by b8435c9a, and `modules/apikeys/src` has no
  `admin.rs`. The requirement stays for its replacement: a policy `Multi` pre-checked
  from the stored policy and a revision-checked save, so saving is not a blind full
  replace. The control plane already supports a pre-checked `FieldKind::Multi` through
  `Field.default` (`core/controlplane/src/lib.rs:436-454`, `core/admin/src/render/form.rs:57-63`).
  It belongs to step 21 (apikeys) of
  docs/plans/2026-10-22-1759-admin-control-plane-redesign-plan.md, whose text does not
  yet name it.
- [ ] **`conformance_key_rejected` executes no production code**
  APIKEYS-06. modules/apikeys/src/conformance.rs:16 is `len > MAX_KEY_BYTES`. Source:
  modules/apikeys/src/conformance.rs:16 (2026-07-12); see
  docs/status/2026-09-05-1416-leaderboard-shared-table-pollution-status.md:89.
