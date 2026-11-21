---
name: split-implementer-tasks-small
description: Implementer briefs must be short and steps split into smaller tasks so a subagent never runs out of context
metadata:
  node_type: memory
  type: feedback
  originSessionId: 0af9025c-b620-41bb-9924-cba26bbf9324
  modified: 2026-11-21T11:55:27.851Z
---

Give each implementer subagent a short brief and a small task; split a plan step into several smaller tasks (each its own commit) rather than one long dispatch.

**Why:** owner, 2026-11-21, after the Step 14a1 implementer ran out of context mid-step ("to rob krotsze i rozbijaj kroki na mniejsze taski").
**How to apply:** before dispatching, cut the step into parts that fit one context (one file group, one behaviour); keep the prompt to the decisions and invariants ([[plans-specify-invariants-not-prose-code]]); send the next part after the previous one's commit and review.
