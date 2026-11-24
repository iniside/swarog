# Audit

Last updated: 2026-11-24

## In progress

_None._

## Open

- [ ] **Analytics and telemetry event pipeline**
  AUDIT-05. Nothing in modules/ implements one (Grep, lower bound). Source:
  docs/roadmap/feature-tracker.md:253 @ fd797b3a (2026-07-22).

### Known limits (admin-control-plane-redesign step 24)

- [ ] **Audit filters and totals scan unindexed rows**
  Only `at` is indexed (`audit.log_at_idx`); the topic, actor and search filters, the page
  total `count(*)` and the actor options all scan the retained rows (`modules/audit/src/store.rs:216`,
  `:294-296`, `:310`, `:330-331`). An index on `topic` and on `payload->>'actor'` would serve them.
- [ ] **Outcome prefixes copied from core/admin text**
  `FAILED_DETAIL_PREFIXES` (`modules/audit/src/admin.rs:39`: `failed:`, `unknown:`,
  `rejected:`) and the `login-`/`logout` literals copy text core/admin writes into
  `admin.action` detail; a wording change in core silently breaks the failure marking. Proposal:
  public constants in `controlplane::events`. Open owner question: does `conflict:` count as a failure.
- [ ] **Operator login "system" collides with the system actor**
  Non-operator rows show the actor `SYSTEM_ACTOR` = "system" (`modules/audit/src/store.rs:17`,
  `:121`), so `?actor=system` also returns an operator whose login is "system", and the actor
  options list "system" once.
- [ ] **Activity panel meta reads "target · —" without detail**
  An operator row with an empty detail draws its meta line as "target · —" in the dashboard
  "Recent operator activity" panel instead of the target alone.
- [ ] **Audit page and its total can disagree for a moment**
  The page rows and the "N entries" total are two separate queries, so a row appended between
  them makes the total off by one until the next load.
- [ ] **Audit panel error block unproven in the split**
  The split break run (24t, 99463190) removed the audit peer, which 404s the page and drops the
  dashboard slot; the error block (audit-svc down after boot, with a cached manifest and a
  failing panel call) was never observed. AU3 also checks the topic-filtered page, not the
  unfiltered one (`tools/splitproof/src/audit_admin_checks.rs`).
