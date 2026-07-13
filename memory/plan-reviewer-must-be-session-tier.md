---
name: plan-reviewer-must-be-session-tier
description: "Plan-review reviewer runs at session tier (or the user-designated top tier), never silently demoted to a cheaper model"
metadata: 
  node_type: memory
  type: feedback
  originSessionId: be9ca5c9-1143-4863-8952-76e6af8da02c
---

Caught on 2026-07-13: dispatched the Plan Writing Workflow step-5 grumpy reviewer on
sonnet while the plan was written by fable (session tier). User: "jakim cudem review
jest sonnet, jak plan pisal fable". A model-restriction remark aimed at RESEARCH
subagents was wrongly generalized to the reviewer.

**Why:** the independent reviewer must be at least as strong as the plan author, or
the review can't catch the author's mistakes — it rubber-stamps. CLAUDE.md Plan
Writing step 5 says "at session tier" explicitly.

**How to apply:** reviewer model = session tier by default. When the user restricts
models (cost), ASK or use the strongest allowed tier (2026-07-13 decision: fable
banned for subagents ⇒ reviewer/adversarial-commit-review = opus, implementation =
sonnet, tags name models explicitly in the plan). Never quietly extend a scoped
model restriction to a different role. See [[team-is-solo-plus-agents-forever]].
