---
name: plan-reviewer-must-be-session-tier
description: "Plan-review reviewer runs at session tier, never silently demoted — and don't extend a scoped model restriction to a different role"
metadata: 
  node_type: memory
  type: feedback
  originSessionId: 88cdd953-b406-40a0-8ab2-6c7eb07acece
---

The step-5 plan reviewer (and any adversarial reviewer) must be at least as strong as the
author, or it rubber-stamps. CLAUDE.md Plan Writing step 5 already says "at session tier" —
the delta this memory keeps is the incident + the model-assignment decision:

Caught 2026-07-13: dispatched the reviewer on sonnet while the plan was written by fable.
User: "jakim cudem review jest sonnet, jak plan pisał fable". A model-restriction remark
aimed at RESEARCH subagents was wrongly generalized to the reviewer.

**Decision (2026-07-13):** with fable banned for subagents ⇒ reviewer/adversarial-commit-
review = opus, implementation = sonnet; tags name models explicitly in the plan. When the
user restricts models, ASK or use the strongest allowed tier — never quietly extend a scoped
restriction to a different role. See [[team-is-solo-plus-agents-forever]].
