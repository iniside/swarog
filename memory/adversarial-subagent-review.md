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
task".

**Reinforced 2026-07-14 (MANDATORY violation, repeat):** the pass is ALWAYS a `core-reviewer`
SUBAGENT — never an inline self-review, and "the diff is trivial/mechanical" is NOT an
exemption. I did Step 1 (a 2-line SQL `LIMIT`) inline, declared "failure-mode: none", and
called dispatching a subagent "manufacturing findings" — a rationalization. The subagent then
found F1 (the comment asserted a `create()` per-player cap that did not exist yet → false
authority + silent row-stranding) that my inline read missed. Inline ≠ the independent-reviewer
boundary (same session context = zero independence). **Lukasz's rule:** the subagent does the
review, and ALSO run a second audit (`proof-auditor`) checking whether BOTH the implementer AND
the reviewer cheated/hand-waved — re-derive test soundness from code, distrust the summary and
the self-review. A "failure-mode: none" on a test-bearing diff is exactly the case that goes to
proof-auditor.

**Repeat again 2026-07-14 (fortress-no-exceptions rollout):** ran the ENTIRE 6-step rollout
(3 test dev-dep swaps, an archcheck verify-gate change, a durable-plane negative test, doc
edits) reviewing every subagent diff INLINE — read the diff, verified claims against code,
called it done. Same violation at rollout scale: green `verifyctl --fast` is NOT the review
(the rule literally says green-tests-that-look-like-the-plan is what it exists to catch). Two
compounding errors: (1) no per-diff `core-reviewer`/`proof-auditor` pass until the user forced
it; (2) when I finally dispatched, I reached for `general-purpose` — user rejected it: "teraz
poprawnego sub agenta". **Rule:** the pass is the REGISTERED specialized subagent
(`core-reviewer`; + `proof-auditor` when the diff touches a verify stage or the test/gate IS
the risk surface). If the specialized agent_type looks unavailable, that is a signal to
check/enable it — NEVER a license to substitute general-purpose or inline. The boundary must
exist BEFORE the commit, routed per-diff, not retrofitted after challenge ("it came back CLEAN
anyway" is not the point). Related: [[scope-claims-to-what-was-verified]],
[[verify-the-at-risk-path-not-the-safe-one]], [[specialized-core-agents]],
[[core-failure-taxonomy]].
