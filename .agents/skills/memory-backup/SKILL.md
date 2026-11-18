---
name: memory-backup
description: Mirror agent memory between the live Claude Code memory dir and the repo's memory/ folder. Use after ANY write/update/delete of a memory file or MEMORY.md (push), and after a git pull/sync before relying on recall (pull).
---

# Memory Backup

Project memory lives outside the repo (`$HOME/.claude/projects/<mangled-repo-path>/memory/`)
and is mirrored into the repo at `memory/` so it survives across machines via git.
The live path is the one `scripts/memory-sync` derives, overridable with
`CLAUDE_MEMORY_DIR`; `scripts/memory-sync.sh path` prints it. The `.ps1` twin and
the no-exec-bit invocation form are in `docs/reference/platform-notes.md`.

- After ANY memory change run `scripts/memory-sync.sh push` (or `.ps1`): it
  mirrors live -> `memory/` and commits `chore(memory): ...`. Do not hand-copy.
- After a `git pull`/sync run `scripts/memory-sync.sh pull` before relying on
  recall.
