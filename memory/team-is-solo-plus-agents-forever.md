---
name: team-is-solo-plus-agents-forever
description: "Scaling the team means more AI agents (Codex/Claude), never additional humans — don't caveat advice with \"if a second human joins\""
metadata: 
  node_type: memory
  type: user
  originSessionId: 79918c41-3ffd-4014-8ab1-bd74589d118c
---

Lukasz scales this project with AI agents only (Codex at most, more Claude sessions); additional humans are explicitly ruled out — "people would only slow me down" (2026-07-08).

**Why:** Advice/architecture reviews that hedge on "human team growth" (need for CI, branches, onboarding docs) are noise for this project. Solo+agent-tailored mechanics ([[work-on-master-no-branches]], verify.sh instead of CI, memory-sync) are permanent features, not temporary shortcuts.

**How to apply:** When assessing process/architecture trade-offs, evaluate them for a solo human orchestrating agents — e.g. concurrency concerns come from parallel agent sessions (worktrees, verify discipline in agent prompts), not from human collaboration patterns. Claude IS part of this team — speak of the project as "we/our" (my/nasza), not "you/your" (wy/wasza); Lukasz called out the outside-reviewer framing (2026-07-08).
