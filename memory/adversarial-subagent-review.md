---
name: adversarial-subagent-review
description: Review subagent diffs (and own fixes/scaffolding) adversarially — try to break each at its OWN new boundaries; a plausibility read is not a review
metadata: 
  node_type: memory
  type: feedback
  originSessionId: 88cdd953-b406-40a0-8ab2-6c7eb07acece
---

Reviewing a diff means attempting to BREAK it, not confirming it matches the plan step. The
method is CLAUDE.md `## Adversarial Subagent Review — MANDATORY` (read it, don't restate it):
route by class, attack the fix's own new seam first, verify against code not the summary,
state the failure mode + pinning test, bounce as a punch list.

**Why:** the 2026-07-12 external audit broke four accepted fixes exactly at the boundary
each fix introduced (retention `Ok`-while-per-topic-fails `7ca0b51`, hardcoded 3h stall
threshold, cargo-audit green SKIP `b78444f`, scheduler budget starvation `addc824`). None
needed new information — only hostility. The complementary failure is gold-plating (Codex
overcorrection), so the question is always "what is the MINIMAL closure, and what breaks it".

**Refined 2026-07-13:** it is ONE pass by a different method. A clean verdict IS valid when
it enumerates the classes attacked — do NOT mandate re-review on zero findings; that
manufactures findings and recreates the 46-commit carousel. Rigor = the class list, not a
loop. **Applies to my OWN freshly-authored scaffolding too** (agent files, docs, config) —
Lukasz caught me committing 3 agent files without the pass, carrying duplicated-authority
(rules copied into prompts). Review your own tooling before commit; don't defer to "next
task". Related: [[scope-claims-to-what-was-verified]],
[[verify-the-at-risk-path-not-the-safe-one]], [[specialized-core-agents]],
[[core-failure-taxonomy]].
