# Apikeys

Last updated: 2026-11-23

- [x] **Role-policy editor must pre-check the stored policy** (2026-11-23, admin-control-plane-redesign step 21)
  APIKEYS-01. The roles row menu's "Edit policy…" (`role.edit`) prepares a form whose
  "Allowed methods" `Multi` is pre-checked from the stored policy through `Field.default`,
  and saves through the `set_role_policy` CAS on `FormSpec.revision`
  (modules/apikeys/src/admin.rs `role_form`, `edit_role`).
