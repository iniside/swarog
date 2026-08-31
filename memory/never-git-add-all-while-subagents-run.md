---
name: never-git-add-all-while-subagents-run
description: git add -A while a subagent is editing sweeps its work into my commit — stage by path whenever any agent is live
metadata: 
  node_type: memory
  type: feedback
  originSessionId: 3dcd09ee-8844-4095-a44d-a83c5ff1a9f8
  modified: 2026-08-31T17:27:33.958Z
---

`git add -A` (or `git commit -a`) is unsafe whenever a subagent may be writing to the
tree. On 2026-08-31 a `docs(plans)` errata commit swept a subagent's in-progress
`api/wallet/events/src/lib.rs` doc fix into itself, because the agent happened to save
between my edit and my stage.

**Why:** the commit-boundary rule is per unit of work — "do not include unrelated
pre-existing working-tree changes". A swept file also lands with the wrong
`Co-Authored-By` trailer, which breaks the post-rollout trailer audit
([[specialized-core-agents]]) since the lane that wrote it is no longer recoverable
from history.

**How to apply:** while any agent is live, stage by explicit path
(`git add docs/plans/x.md`) and never `-A`/`-a`. Before committing, `git status` and
confirm every staged path belongs to this unit. If a sweep already happened, record it
rather than rewriting published history — see [[never-stash-to-compare-trees]] for the
same "don't fix tidiness by rewriting the tree" line.
