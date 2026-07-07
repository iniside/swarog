---
name: work-on-master-no-branches
description: "For GameBackend, commit directly on master — no feature branches (solo, for-fun repo)"
metadata: 
  node_type: memory
  type: feedback
  originSessionId: e2474d37-b06f-41bb-a2ab-76e4e9659478
---

For this repo the user wants work committed **directly on `master`** — do NOT
create feature branches or PRs by default.

**Why:** solo, for-fun project; branching/merging is "więcej zachodu niż jest tego
warte" (more hassle than it's worth) and the commit history alone is enough
traceability. Stated 2026-07-07 after merging feat/config-module, then deleting the
merged branches.

**How to apply:** Skip the "if on the default branch, branch first" step from
CLAUDE.md Git Safety — that default is overridden here. Still: only commit/push when
asked, never force-push or rewrite published history, and keep the [[git-safety]]
destructive-command guards. Push to origin only on explicit request.
