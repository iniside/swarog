# Splitproof

Last updated: 2026-11-23

- [x] **`[M3b]` posts `create_role` to the razed `/admin/api-keys`** (2026-11-23, admin-control-plane-redesign step 21)
  SPLITPROOF-06. The monolith-local block that asserts `[M3a]` and `[M3b]`
  GETs `/admin/api-keys` for a `_csrf` field and POSTs `_action=create_role` with
  `role_name` and `role_policy`, then checks the `apikeys.roles` row. The admin that served
  that route was razed by b8435c9a and no code outside splitproof references the path, so the
  block can only fail `[M3a]` ("no _csrf field on apikeys page") and never reaches `[M3b]`.
  The other `/admin/api-keys` assertions (`[AD3b]`, `[AD4]` and the later posts,
  main.rs:5177-5499) target the same route. Re-point them with the apikeys step of the
  admin control-plane redesign (docs/plans/2026-10-22-1759-admin-control-plane-redesign-plan.md,
  step 21).
  Closed by `tools/splitproof/src/apikeys_admin_checks.rs`: AD3b, AD4, AD6b-AD6g re-pointed to
  `/admin/p/apikeys/keys` and its actions in both topologies (the monolith ids tagged `m`
  replace `[M3a]`/`[M3b]`), plus AD6h (revoke twice) and AD6i (delete an in-use role).
  AD6b-AD6i prove the split hop through `apikeys` rows only apikeys-svc writes; the break
  run (admin-svc without its `apikeys` peer) proves AD3b and AD4 only.
