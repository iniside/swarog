# Notifications

Last updated: 2026-11-23

- [x] **`admin_data` must never return `Err`** (2026-11-23, admin-control-plane-redesign step 20)
  NOTIFICATIONS-05. Obsolete: the `admin_data` page it named is gone; the new control-plane
  provider (modules/notifications/src/admin.rs) answers `Provider::panel`, whose `Err` the
  portal draws as that one panel's error state (core/admin/src/engine/panels.rs:265-277).
  Original: modules/notifications/src/admin.rs:457-460 maps a build error to
  `opsapi::Error::internal`. Source: modules/notifications/src/admin.rs:454
  (2026-08-31); docs/plans/2026-09-05-1921-friends-module-plan.md:593.
