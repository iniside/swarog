# Audit

Last updated: 2026-11-24

- [x] **Audit log Export has no download path** (2026-11-24, admin-second-pass step 5a)
  The page-head Export action and its forbidden invoke arm were removed from the portal and
  the mockup (plan D1); the download carrier is a separate plan.

- [x] **`admin_data` must never return `Err`** (2026-11-24, b8435c9a)
  AUDIT-02. Obsolete: admin-control-plane-redesign step 1 (b8435c9a) razed audit's
  `admin_data` face; the Audit log page is now a `controlplane::Provider` panel
  (modules/audit/src/admin.rs), whose error draws as that panel's error block.
