# Friends

Last updated: 2026-11-24

- [x] **Admin removal of a friendship** (2026-11-24, admin-second-pass step 17b)
  FRIENDS-08. The accepted friendship row-menu "Remove friend…" is a danger Confirm naming both
  handles; its invoke deletes the accepted edge inside friends and emits `friend.removed`
  (`operator-removed`). A pending edge is refused and a repeat answers NoOp.

- [x] **admin_recent ordering on the text-cast id** (2026-11-22, admin-control-plane-redesign step 17)
  FRIENDS-06. `admin_recent` and its row type are deleted; the Friends tab reads the keyset
  `Store::admin_page`, whose outer ORDER BY reads the raw `created_at`/`id`
  (`modules/friends/src/store.rs:558-566`).
- [x] **Friends admin page against a live render** (2026-11-22, admin-control-plane-redesign step 17)
  FRIENDS-04. Obsolete: the `/admin/friends` page no longer exists; the player's Friends tab
  replaces it, checked by the admin redesign's own crawl and splitproof parts.
