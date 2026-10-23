# Characters

Last updated: 2026-10-23

## In progress

_None._

## Open

- [ ] **Idempotency key for characters create**
  CHARACTERS-02. `create(identity, name, class)` has no contract-level key, so a
  client retry after an ambiguous timeout can double-create
  (`modules/characters/src/service.rs:106`; docs/reference/module-reference.md:162,
  2026-07-14). `match::report`'s required `ReportId` is the exemplar.

## Notes

- `LIST_HARD_LIMIT` truncation (module-reference "Do NOT copy" #3) is closed:
  `modules/characters/src/service.rs:150-159` clamps the per-player cap to the
  list limit. The `KNOWN GAP` wording in `modules/characters/src/store.rs:8` is
  stale.
