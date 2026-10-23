# Inventory

Last updated: 2026-10-23

## In progress

_None._

## Open

- [ ] **Item stacks and instances**
  INVENTORY-01. The module holds a static catalog and per-character holdings
  only; no stack or instance model (Grep lower bound over
  `modules/inventory/src/lib.rs` found no stack/instance code). Item-level half
  of the partial tracker row (docs/roadmap/feature-tracker.md:223, 2026-07-22).
- [ ] **Character tombstone table without GC**
  INVENTORY-03. One permanent row per deleted character, no retention or
  watermark; acceptable only while wipe is the migration. Writer and growth
  note in `modules/inventory/src/projection.rs:85-90,159-162`
  (docs/reference/module-reference.md:151, 2026-07-14; read at HEAD).
- [ ] **Silent truncation of the holdings list at HOLDINGS_HARD_LIMIT**
  INVENTORY-04. An owner with more than 1000 distinct items has the surplus
  dropped from list views, with no cursor or `has_more`
  (`modules/inventory/src/store.rs:6-11`, 2026-07-14; docs/reference/module-reference.md:159).
