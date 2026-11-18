---
name: adversarial-subagent-review
description: "review = core-reviewer subagent, adversarial against the diff AND the plan; structural findings go to the owner, not into fix loops; prose-only commits get no review (owner 2026-11-18)"
metadata:
  node_type: memory
  type: feedback
  modified: 2026-11-18T15:21:22.305Z
  originSessionId: d6807c6e-9fc4-4ea8-96cd-ed9826d0a2bf
---

Review is always a `core-reviewer` SUBAGENT, never inline (repeat violations
2026-07-14: inline "trivial" reviews missed real defects). Method lives in
`.agents/skills/diff-review/SKILL.md` (dispatch rules: `.agents/shared/planning-dispatch.md` **Adversarial Diff Review**).

**Owner 2026-11-18:** keep review adversarial — hunt implementation gaps and
structural problems, attack the fix's own new seams, and challenge the PLAN
itself; a defect that follows from the plan is a plan defect, never "matches
the plan". What changed: no default-REJECT / must-find pressure; style and
comment findings are not REJECT grounds; a structural finding stops the loop
and goes to the owner with a root-cause proposal instead of a review-fix
round; one round by default, a second only for core/, split/edge wiring and
verify gates; no review for prose-only commits (docs/, .agents/,
.claude/agents/, docs/plans/, memory) except one plan review before approval.

**Why:** 31% of 340 commits (14 days to 2026-11-18) were review fixes, many
self-referential prose fixes; the owner's worry is the opposite failure — a
reviewer that only checks plan conformance lets a flawed plan through and a
week later we are in a fix loop. Origin: the 2026-07-12 audit broke four
accepted fixes at the seams they introduced.

Related: [[specialized-core-agents]], [[core-failure-taxonomy]],
[[scope-claims-to-what-was-verified]].
