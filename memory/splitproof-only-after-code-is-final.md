---
name: splitproof-only-after-code-is-final
description: start a splitproof rollout only when every code change it covers is landed and reviewed
metadata:
  node_type: memory
  type: feedback
  originSessionId: ba09ec16-ad8e-4eff-b973-d5b805fe010b
  modified: 2026-11-24T22:24:56.195Z
---

Do not start a splitproof run (or a split-proof plan step) while earlier code steps still have open review-fixes or owner-decided follow-ups. Finish and review the code first, then run splitproof once against the final tree.

**Why:** 2026-11-24 the owner objected ("po jaka cholere wlaczyles ten caly splitproof jak nie bylo skonczone") when Step 19a's splitproof ran while Step 18 review-fixes and the full-payload copy change were still pending — the run proved a tree that was about to change and blocked cargo for the remaining code work.

**How to apply:** before dispatching any splitproof step, check that every earlier step in the batch is landed, reviewed, and has no pending fix or owner decision; queue the splitproof steps after. Related: [[pgrep-check-must-gate-not-decorate]], [[verify-the-at-risk-path-not-the-safe-one]].
