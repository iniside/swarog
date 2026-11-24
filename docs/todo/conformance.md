# Conformance

Last updated: 2026-11-24

## In progress

_None._

## Open

- [ ] **A CapCase label is not tied to the probe it runs**
  CONFORMANCE-03. A case's `writer` field (tools/conformance/src/model.rs CapCase) and the
  `CREDENTIAL_CAPS` case names (tools/conformance/src/checks.rs:118-200) are labels. Nothing ties a
  label to the function its probe calls: a case marked `mail` that runs wallet's probe stays green,
  and so does the google credential case if it runs the epic probe (both share the 65 536 cap).
  Fix: derive the cases instead of declaring them. For cp writers, one constructor takes the
  Provider, sets `writer` from `manifest().module` and drives `invoke` with an oversized value.
  For credentials, build one case per `credential_caps()` entry through
  `accounts::conformance::registry_credential_rejected` (modules/accounts/src/conformance.rs:238-252).
  Use one typed link on CapCase, not a second field.
  Source: proof-auditor review of the typed-writer-link change, admin-second-pass step 13 (2026-11-24).
- [ ] **`Validated { cap }` policy numbers not linked to `CapCase`**
  CONFORMANCE-01. tools/conformance/src/policy.rs:70-80 still carry literal caps (e.g. 320, 1024) with
  prose bases; only `checks::CREDENTIAL_CAPS` binds.
  Source: docs/plans/2026-09-10-1917-account-deletion-purge-plan.md:626 (2026-09-10),
  docs/roadmap/feature-tracker.md:324 @ fd797b3a.
- [ ] **`OPAQUE_REQUEST_TYPES` bare-name bypass**
  CONFORMANCE-02. The check compares `opaque.name == written` (tools/conformance/src/input_inventory.rs:354-356),
  so a foreign type imported under the bare name `Identity` bypasses the inventory.
  Source: docs/plans/2026-09-10-1917-account-deletion-purge-plan.md:635 (2026-09-10).

### Known limits (admin-control-plane-redesign step 25)

- [ ] **The writer check passes with zero writers**
  The conformance check over admin writers (`tools/conformance/src/main.rs` ~296) is green
  when it finds no writer at all; a floor that fails the phase on zero would catch a scan that
  silently stopped matching (proof audit of 38a51fdf).

### Known limits (admin-control-plane-redesign step 15)

- [ ] **Admin form CapCase rows are listed by hand**
  The wallet create-currency rows (`tools/conformance/src/policy.rs:1249-1263`) name each
  capped field of `create_form()` by hand, so a new text field added to the form gets no probe
  and no gate notices. Building the cases from the form's String fields ends it.
