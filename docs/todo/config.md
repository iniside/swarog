# Config

Last updated: 2026-11-26

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

- [ ] **archcheck fails on config's DDL**
  `cargo run -p archcheck` is red at HEAD with six rule-22 findings in `modules/config/src/lib.rs`:
  - :114, `CREATE SCHEMA` text outside a `*_DDL` const (`SCHEMA_DDL_TEMPLATE` is not named `*_DDL`);
  - :115, :123 and :129, `CREATE TABLE` text;
  - :139 and :159, `CREATE OR REPLACE FUNCTION` text.
  The file was last changed in admin-second-pass (2026-11-24). Found while verifying the
  oidc-signing-keys Step 2 review fix (309d8708, 2026-11-26).

### Known limits (admin-control-plane-redesign step 23)

- [ ] **Re-saving a float rewrites its digits**
  The edit decodes a number through f64 (`modules/config/src/admin.rs`, aaf1ab3b), so
  saving a key whose stored value is `0.1000000000000000055511` writes `0.1`, and a u64
  above 2^53 comes back as a rounded float (`…616.0`). Found by the 23a2 review.
- [ ] **Create refuses an empty string value that edit accepts**
  `setting.create` cannot make a String key whose value is `""`, while `setting.edit`
  saves `""` on an existing key. Found by the 23a2 review.
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
- [ ] **Byte-order default-key test is weak on non-C collations**
  `no_key_param_selects_the_first_key_in_byte_order` (`modules/config/src/admin_tests.rs`,
  0007e9e5) expects `a_z, ab, k`; ICU and Windows collations also sort `a_z` first, so
  dropping `COLLATE "C"` (`modules/config/src/store.rs`) stays green there. Fix: add keys
  `0x` and `_x` and expect `0x, _x, a_z, ab, k`, with a literal expected key instead of the
  test's own ORDER BY.
- [ ] **Revisions entry order not checked on rendered HTML**
  The config tests assert the `TimelineEntry` carriers (summary "rev N by", one User badge),
  not the drawn text, so a reorder of the `timeline` macro (`core/admin/src/templates/primitives.html.tmpl:264`)
  would stay green. Fix: a golden of a summary-plus-badge timeline entry in core/admin.
- [ ] **500-key read bound proven only in memory**
  `the_type_filter_sees_past_the_drawn_rows` proves the store read goes past 100 rows, not
  that it reaches `MAX_READ` (500); a SQL limit of 101 passes it. The unit test pins the
  in-memory side only.
- [ ] **Values over the portal input cap cannot be edited from the portal**
  `ConfigAdmin::invoke` (`modules/config/src/admin.rs`) refuses every input value over
  `controlplane::MAX_INPUT_VALUE_BYTES` (4096) as its first check, so a key whose value is
  longer (a raw-SQL or `Service::set` write can store one) cannot be saved or re-created from
  the Config & flags page. Checklist C8 (docs/plans/2026-11-23-step23-config-checklist.md).
- [ ] **Config admin writes are not proven in the split**
  The config page's edit and create go through `cp.invoke` from admin-svc to config-svc
  (part 23a2), but no splitproof drives them: part 23t was dropped (owner 2026-11-24). Not
  proven in the split: the page render, the edit and its `admin.action` change note, the
  stale-revision 409, the duplicate-key 422, the refusals, and admin-svc without its config
  peer. Spec: docs/plans/2026-11-23-step23-config-checklist.md §9 (CF1-CF5).
- [ ] **Config admin writes are not replay-safe**
  `setting.edit` and `setting.create` (`modules/config/src/admin.rs`, aaf1ab3b) ignore
  `request_id`: a replayed edit returns a stale-revision Conflict and a replayed create the
  `key: "already exists"` field error, not the first call's success.
- [ ] **A duplicate create still advances the global revision**
  `store::create` (`modules/config/src/store.rs`) inserts with ON CONFLICT DO NOTHING; the
  `settings_stamp` BEFORE trigger bumps `config.revision` before the conflict check, so a
  refused duplicate leaves a gap in the numbering (no row change, no event). Fix: check for
  the key first, or catch the unique violation instead.
- [ ] **Create does not land on the new key**
  A successful `setting.create` returns no redirect, so the page does not select the key
  just created.
