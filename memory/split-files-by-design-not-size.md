---
name: split-files-by-design-not-size
description: "Large files (splitproof main.rs, golden_tests.rs, module tests.rs) get split only inside a purposeful refactor, never as a mechanical size cut"
metadata:
  node_type: memory
  type: feedback
  originSessionId: d6807c6e-9fc4-4ea8-96cd-ed9826d0a2bf
  modified: 2026-11-18T19:23:04.115Z
---

Large source files are split only as part of a concrete refactor with a design reason (real submodules, a seam), not as a standalone "make the file smaller" pass for token cost.

**Why:** on 2026-11-18 the token report showed big files driving agent context; the owner declined a mechanical split plan: a split must make sense as a refactor, not just shrink a file.

**How to apply:** don't propose size-only split plans. When a plan already touches one of the big files (tools/splitproof/src/main.rs, core/admin/src/golden_tests.rs, gateway/remote lib.rs, module tests.rs), suggest the split that fits that change. Related: [[refactor-churn-was-agent-remediation]].
