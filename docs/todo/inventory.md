# Inventory

Last updated: 2026-11-24

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

### Known limits (admin-control-plane-redesign step 16)

- [ ] **Plane test replays the shared test database's history**
  `character_owner_follows_created_and_deleted_via_on_tx` (`modules/inventory/src/tests.rs`)
  registers the real `inventory.character-owner.v1` from `Genesis`, so it writes
  `character_owners` rows for other tests' retained events and cleans only its own.
  Assertions filter by a unique player id, so results are unaffected (review of ccf29df3).
- [ ] **An unknown Type in the Inventory tab draws an error block**
  A `type` value outside the item catalog is `Invalid` (`modules/inventory/src/admin.rs`),
  so a stale or hand-edited URL draws the panel's error block instead of an empty list
  (review of b5fff581).
- [ ] **The Inventory tab runs two extra queries per load**
  Each panel load also reads the distinct item kinds (Type select) and the unfiltered tab
  count, besides the page and its filtered count (review of b5fff581).

### Known limits (push-routing-ordering-and-liveness step 9)

- [ ] **Failing sweep-handler test**
  `inventory::sweep_handler_survivor_untouched_and_ignores_foreign_schedule_name`
  (`modules/inventory/src/tests.rs:1225`) failed the blocking `test` stage of the
  Step 9 acceptance run `verify-1792616200-54429-317230a0`; unrelated to the push
  change (docs/roadmap/feature-tracker.md:315-318, 2026-10-21).
