# Inventory

Last updated: 2026-11-22

## In progress

_None._

## Open

- [ ] **Item stacks and instances**
  INVENTORY-01. The module holds a static catalog and per-character holdings
  only; no stack or instance model (Grep lower bound over
  `modules/inventory/src/lib.rs` found no stack/instance code). Item-level half
  of the partial tracker row (docs/roadmap/feature-tracker.md:223 @ fd797b3a, 2026-07-22).
- [ ] **Character tombstone table without GC**
  INVENTORY-03. One permanent row per deleted character, no retention or
  watermark; acceptable only while wipe is the migration. Writer and growth
  note in `modules/inventory/src/projection.rs:85-90,159-162`
  (docs/reference/module-reference.md:151, 2026-07-14; read at HEAD).
- [ ] **Silent truncation of the holdings list at HOLDINGS_HARD_LIMIT**
  INVENTORY-04. An owner with more than 1000 distinct items has the surplus
  dropped from list views, with no cursor or `has_more`
  (`modules/inventory/src/store.rs:6-11`, 2026-07-14; docs/reference/module-reference.md:156).
- [ ] **Admin grant and revoke of items**
  INVENTORY-05. The player's Inventory tab draws its "Grant item" tool, the player
  row-menu "Grant item…" and the holding row-menu "Revoke item…" disabled
  (`implemented: false`, `modules/inventory/src/admin.rs:330-339`): `api/inventory` has
  no admin write, only the identity-bound `Holdings::grant`
  (`api/inventory/api/src/lib.rs:80`). Needs an admin grant/revoke operation inside
  inventory: the holding row id names the owner, and the grant form takes the player's
  characters from `inventory.character_owners` (checklist G4/G5,
  docs/plans/2026-11-22-step16-inventory-checklist.md §8, 2026-11-22).

### Known limits (admin-control-plane-redesign step 16)

- [ ] **Plane test replays the shared test database's history**
  `character_owner_follows_created_and_deleted_via_on_tx` (`modules/inventory/src/tests.rs`)
  registers the real `inventory.character-owner.v1` from `Genesis`, so it writes
  `character_owners` rows for other tests' retained events and cleans only its own.
  Assertions filter by a unique player id, so results are unaffected (review of ccf29df3).

### Known limits (push-routing-ordering-and-liveness step 9)

- [ ] **Failing sweep-handler test**
  `inventory::sweep_handler_survivor_untouched_and_ignores_foreign_schedule_name`
  (`modules/inventory/src/tests.rs:1225`) failed the blocking `test` stage of the
  Step 9 acceptance run `verify-1792616200-54429-317230a0`; unrelated to the push
  change (docs/roadmap/feature-tracker.md:315-318, 2026-10-21).
