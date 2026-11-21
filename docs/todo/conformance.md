# Conformance

Last updated: 2026-11-21

## In progress

_None._

## Open

- [ ] **`Validated { cap }` policy numbers not linked to `CapCase`**
  CONFORMANCE-01. tools/conformance/src/policy.rs:70-80 still carry literal caps (e.g. 320, 1024) with
  prose bases; only `checks::CREDENTIAL_CAPS` binds.
  Source: docs/plans/2026-09-10-1917-account-deletion-purge-plan.md:626 (2026-09-10),
  docs/roadmap/feature-tracker.md:324 @ fd797b3a.
- [ ] **`OPAQUE_REQUEST_TYPES` bare-name bypass**
  CONFORMANCE-02. The check compares `opaque.name == written` (tools/conformance/src/input_inventory.rs:354-356),
  so a foreign type imported under the bare name `Identity` bypasses the inventory.
  Source: docs/plans/2026-09-10-1917-account-deletion-purge-plan.md:635 (2026-09-10).

### Known limits (admin-control-plane-redesign step 15)

- [ ] **Admin form CapCase rows are listed by hand**
  The wallet create-currency rows (`tools/conformance/src/policy.rs:1249-1263`) name each
  capped field of `create_form()` by hand, so a new text field added to the form gets no probe
  and no gate notices. Building the cases from the form's String fields ends it.
