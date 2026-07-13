---
name: refactor-churn-was-agent-remediation
description: "The big strangler refactors in git history were largely cleanup after agent mistakes, not a deliberate iteration style — don't cite churn speed as a language-fit argument"
metadata:
  node_type: memory
  type: project
  originSessionId: df367cfa-2fb8-48f2-aac5-11559e2ce7f6
---

User stated (2026-07-08): the large multi-step refactors and heavy iteration in this
repo's history (durable-plane strangler, unified-transport A1–G2, api/ hoist) happened
largely because agents made mistakes — remediation, not a preferred fast-iteration
workflow.

**Why:** I had argued "Go's fast compile suits your refactor churn" in a Go-vs-Rust
discussion; user corrected that the churn was error-correction. That reframes
language-fit debates (relevant to [[decision-quarkus-as-final]]): the question is
which language catches agent mistakes earliest, not which tolerates churn. The
`verifyctl` gauntlet is partly a hand-built substitute for a stricter compiler.

**How to apply:** In Go-vs-Rust/Kotlin discussions, don't count refactor velocity as
a Go advantage. Distinguish mechanical agent errors (a stricter compiler catches
these) from overclaiming/verification failures (no type system catches these — see
[[verify-the-at-risk-path-not-the-safe-one]],
[[scope-claims-to-what-was-verified]]).
*** Delete File: C:/Users/lukas/.claude/projects/G--Projects-GameBackend/memory/refactor-churn-was-opus-remediation.md
