---
name: split-impl-and-tests
description: "who writes tests — the implementer in the same step by default; [test-author] only for verify gates, splitproof assertions and new harnesses (owner 2026-11-18)"
metadata:
  node_type: memory
  type: feedback
  modified: 2026-11-18T14:20:35.526Z
  originSessionId: d6807c6e-9fc4-4ea8-96cd-ed9826d0a2bf
---

**Current rule (owner, 2026-11-18, agent-workflow-simplification plan):** the
implementer writes the tests in the same step, to the bar "revert the fix and
the test must fail". A separate `[test-author]` step only for verify gates,
splitproof assertions and new test harnesses. Tests are reviewed together with
their step.

**Why it changed:** the old absolute split (2026-07-22, ported from ArcGame:
"tests always a separate later step, no trivial carve-out") cost a fresh agent
re-reading the landed diff plus its own review rounds for every step; measured
14 days to 2026-11-18: ~50 test-author dispatches, part of the 31% review-fix
commit share. Official Claude Code guidance shows the same session writing the
failing test then the fix; nothing recommends a separate test agent.

**How to apply:** don't plan a separate test step for ordinary code; do plan
one for gates/splitproof/harnesses. Still prove the at-risk topology (split) —
see [[verify-the-at-risk-path-not-the-safe-one]], [[splitproof-is-proof-surface]].
