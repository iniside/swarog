---
name: fix-the-authority-not-the-symptom
description: "Implementation workflow — fix the deciding authority, never patch the symptom or layer hacks; minimal sufficient closure, recorded deviations, failing-branch proof, sibling sweep"
metadata: 
  node_type: memory
  type: feedback
  originSessionId: fa1a7334-ddfc-4932-9aa9-dcbe7e8ce691
---

Lukasz's correction (2026-07-13): adopt the Codex-remediation workflow instead of
"praca po łebkach" and hacks-on-hacks — shallow fixes cost four waves of bug-fixing
(~130 commits, 2026-07-12/13) instead of shipping a feature.

**Why:** My audit-era fixes patched outcomes while leaving the flawed decision
authority alive (hardcoded 3h beside the configurable interval, green SKIP beside a
mandatory audit stage, per-topic error swallowing beside the liveness stamp). Each
survived review and each was later broken at the boundary it introduced. The
remediation closed the same findings by replacing authorities (one retention
Config, typed Slot<T>, provenance enum) and found adjacent bugs on the way.

**How to apply:** Follow CLAUDE.md `## Fix the Authority, Not the Symptom —
MANDATORY`: (1) name the single deciding place and fix there; (2) a second special
case beside an earlier fix = replace the authority, keep its good invariant;
(3) state the minimal sufficient closure — below is po-łebkach, above is
gold-plating (both burned this repo); (4) record decision reversals/plan deviations
in commit + plan errata in the same rollout; (5) ship a test that executes the
formerly-wrong branch on the at-risk topology; (6) sweep for sibling defects before
leaving the area, fix or record as explicit gaps. Related:
[[adversarial-subagent-review]], [[verify-the-at-risk-path-not-the-safe-one]],
[[scope-claims-to-what-was-verified]].
