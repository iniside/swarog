# Storage

Last updated: 2026-11-24

## In progress

_None._

## Open

- [ ] **Admin download carrier for cloud saves**
  `Then` is Invoke/Form/Confirm/Link and nothing serves bytes. The Download all, per-row
  download and row-menu "Download cloud saves" buttons were removed from portal and mockup
  (admin-second-pass step 8b, ADMIN2-53..55 in `docs/todo/admin-second-pass-done.md`); they
  return with a download carrier (owner, 2026-11-24, D1; O-1..O-9 in
  `docs/research/2026-11-20-2230-admin-ports-contract-gaps.md`).
- [ ] **Misleading 409 at the object cap for put(expected_version = 0)**
  STORAGE-09. At 10,000 objects a create-style `put` to an address the owner
  already holds returns the "maximum of N objects" 409 instead of the
  address-taken one; same status, wrong remedy. The cap check runs before the
  insert (`modules/storage/src/service.rs:265-272`, comment states it);
  positive-version updates stay available
  (docs/roadmap/feature-tracker.md:320 @ fd797b3a, 2026-09-17; read at HEAD).

### Known limits (admin-control-plane-redesign step 19)

- [ ] **The Cloud saves tab meta re-sums the owner's objects on every open**
  `Store::admin_owner_totals` (`modules/storage/src/store.rs:533`) counts and sums
  `octet_length(value::text)` over every object the owner holds, up to
  `storageapi::MAX_OBJECTS_PER_OWNER` (10,000), detoasting each value, on each tab load
  (`modules/storage/src/admin.rs:62`); nothing caches it and no counter holds a per-owner
  total. Owner 2026-11-22: deferred.
