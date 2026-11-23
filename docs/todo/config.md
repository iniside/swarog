# Config

Last updated: 2026-11-24

## In progress

_None._

## Open

- [ ] **Remote config targeting**
  CONFIG-02. Live-reload delivery exists; there is no per-audience targeting
  (`api/config/api/src/lib.rs:69-80` reads take only namespace and key).
  Item-level half of the partial tracker row (docs/roadmap/feature-tracker.md:250 @ fd797b3a,
  2026-07-22); A/B experiments and live events are feature rows :251-252 @ fd797b3a.
- [ ] **Test namespaces left behind by a failed run**
  CONFIG-03. `config.settings` can accumulate `test_*` rows. At HEAD all 14
  tests that call `unique_ns` call `cleanup` as their last step
  (`modules/config/src/tests.rs`), so rows survive only when a test panics before
  it; the survey's "clean some namespaces only" is narrower than it was
  (docs/research/2026-07-14-2058-timing-sensitive-tests-survey.md:105,
  2026-07-14). Low severity.

### Known limits (admin-control-plane-redesign step 23)

- [ ] **Multi-row config writes can deadlock**
  The `settings_stamp` BEFORE trigger (`modules/config/src/lib.rs`, d3de6666)
  bumps `config.revision` per row, so a statement or `set_tx` batch that has
  written one row takes its next row's lock after the revision lock; two
  concurrent multi-row INSERT/UPDATE statements or batches over overlapping
  existing rows can deadlock (40P01). Single-row writers lock the row first.
  It predates the per-key revision. A writer batching keys should lock them in
  (namespace, key) order.
- [ ] **Lock-order doc overstates the deadlock case**
  `modules/config/src/lib.rs:93-97` says any two concurrent multi-row
  statements can deadlock; a multi-row DELETE locks all its rows before its
  AFTER bumps and cannot. Comment only.
