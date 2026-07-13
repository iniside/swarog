---
name: core-failure-taxonomy
description: "evidence base for what actually breaks core and why double hostile review didn't help — the disease is symptom-first fixes, not too few review rounds"
metadata: 
  node_type: memory
  type: reference
  originSessionId: 88cdd953-b406-40a0-8ab2-6c7eb07acece
---

`docs/reference/core-failure-taxonomy.md` — synthesis of a 6-slice retrospective over the
2026-07-11..13 remediation (~217 commits, mined by 6 parallel subagents cutting by core
layer).

**The reframe (answers "podwójny wrogi review dalej nie pomaga"):** fix-on-fix DOMINATED the
window — processctl 16/18 fixes were chains against one authority (lock/lease 8×, RPC-retry
4×, devctl teardown 6×). Hostile review WAS working (round-2 fixes exist because a review
caught round-1's hole). The disease is upstream: fixes authored symptom-first, each creating a
new seam the next review broke. More review rounds = a symptom, not the cure. The lever is
**locate the authority before writing** + **class-keyed review attacking the fix's own new
seam**, not generic hostility.

The doc ranks ~10 recurring failure classes (error-folded-into-success and unbounded-operation
dominate; then ordering-not-structural, resource-owned-by-wrong-scope, constant-shadows-config-
knob, coverage-gap/false-pass, notapplicable-hides-gap, hand-maintained-list-drift, topology-
blind-violation) each with the review attack + the authority-locating fix, plus a
files-touched review checklist. It is the source the planned `core-implementer` /
`core-reviewer` agents derive their checklists from. Keep it current from real commits.

Builds on [[fix-the-authority-not-the-symptom]] and [[adversarial-subagent-review]] — this is
the empirical catalog those rules operate on. Split-only landmark: INV-01 (INVENTORY_DEV_GRANT
gated only monolith call-site, live in split) reinforces [[verify-the-at-risk-path-not-the-safe-one]].
