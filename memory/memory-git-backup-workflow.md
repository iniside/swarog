---
name: memory-git-backup-workflow
description: "Agent memory is git-backed via repo memory/ mirror + scripts/memory-sync.sh (push after any memory change, pull after git sync)"
metadata: 
  node_type: memory
  type: feedback
  originSessionId: b86d7424-30cc-4b38-a18a-173c8facf6f9
---

Claude Code project memory (`$HOME/.claude/projects/<mangled>/memory/`, per-machine)
is mirrored into the repo at `memory/` so it syncs across machines through git.

**Why:** memory lives on a per-machine drive outside the repo; without a git-backed
mirror it can't be shared between the user's machines.

**How to apply:**
- After ANY memory change (write/update/delete a `*.md` or `MEMORY.md`), run
  `scripts/memory-sync.sh push` (bash) or `scripts/memory-sync.ps1 push` (PS) — it
  mirrors live→`memory/` and commits `chore(memory): …`. Add `--push`/`-Push` to also
  git push. Handles deletions (mirror is exact).
- After a `git pull`/sync, run `... memory-sync.sh pull` to copy the git copy back to
  this machine's live memory dir BEFORE relying on recall.
- Live dir is derived (repo abspath, every non-alnum char → `-`), so scripts are
  portable; `CLAUDE_MEMORY_DIR` overrides; `... path` prints the resolved dir.

Rule is also in CLAUDE.md ("Agent memory backup — MANDATORY"). See
[[work-on-master-no-branches]] — memory commits land directly on master.
