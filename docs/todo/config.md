# Config

Last updated: 2026-10-23

## In progress

_None._

## Open

- [ ] **Remote config targeting**
  CONFIG-02. Live-reload delivery exists; there is no per-audience targeting
  (`api/config/api/src/lib.rs:69-80` reads take only namespace and key).
  Item-level half of the partial tracker row (docs/roadmap/feature-tracker.md:250 @ fd797b3a,
  2026-07-22); A/B experiments and live events are feature rows :251-252.
- [ ] **Test namespaces left behind by a failed run**
  CONFIG-03. `config.settings` can accumulate `test_*` rows. At HEAD all 14
  tests that call `unique_ns` call `cleanup` as their last step
  (`modules/config/src/tests.rs`), so rows survive only when a test panics before
  it; the survey's "clean some namespaces only" is narrower than it was
  (docs/research/2026-07-14-2058-timing-sensitive-tests-survey.md:105,
  2026-07-14). Low severity.
- [ ] **AdminSubmit for the config admin page**
  CONFIG-04. The remote admin fan-out returns the read-only `admin_content_ro`
  because a remote form cannot marshal its `submit` closure
  (`modules/config/src/lib.rs:522-524`); no `AdminSubmit` exists in
  `modules/config` outside a test name, so the page is read-only in the split
  (docs/research/2026-10-22-1830-admin-redesign-data-sources.md:152, 2026-10-22).
