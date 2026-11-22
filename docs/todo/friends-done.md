# Friends

Last updated: 2026-11-22

- [x] **admin_recent ordering on the text-cast id** (2026-11-22, admin-control-plane-redesign step 17)
  FRIENDS-06. `admin_recent` and its row type are deleted; the Friends tab reads the keyset
  `Store::admin_page`, whose outer ORDER BY reads the raw `created_at`/`id`
  (`modules/friends/src/store.rs:558-566`).
- [x] **Friends admin page against a live render** (2026-11-22, admin-control-plane-redesign step 17)
  FRIENDS-04. Obsolete: the `/admin/friends` page no longer exists; the player's Friends tab
  replaces it, checked by the admin redesign's own crawl and splitproof parts.
