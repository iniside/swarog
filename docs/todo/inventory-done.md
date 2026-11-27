# Inventory

Last updated: 2026-11-27

- [x] **archcheck fails on inventory's DDL** (2026-11-27, verify-reds-fix step 2)
  The SQL comment no longer words a foreign-schema reference, and the two secondary indexes are
  built by `Ddl::index` in `migrate` (same names and columns, so existing databases keep them).

- [x] **Admin grant and revoke of items** (2026-11-24, admin-second-pass step 14)
  INVENTORY-05. Inventory exposes `item.grant` (player Inventory tab tool and row menu, with a
  request id so a retry does not double the stack) and `holding.revoke` (a prepared danger
  Confirm that removes the whole stack); the mockup draws both, on the player and character tabs.
