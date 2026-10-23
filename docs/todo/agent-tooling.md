# Agent tooling

Last updated: 2026-10-23

## In progress

_None._

## Open

### Known limits (arcgame-workflow-port step 9)

- [ ] **`git_safety_hook` false denies and path-resolution gaps** (AGENT-TOOLING-02)
  `git -C <dir>` path checks resolve against the payload cwd; `echo 'git' 'stash'` is a
  false deny; `git reset <deleted-file>` is denied; a file named like a branch is
  ambiguous; `git checkout :/msg` is not treated as a path; `git reset --soft HEAD~`
  without a digit is denied (.claude/hooks/tests/test_hooks.py:339); existing-path
  checkout is denied when `git rev-parse` cannot run. The owner chose one final fix and
  no third review round.
  Source: docs/plans/2026-10-21-2252-arcgame-workflow-port-plan.md:369 (2026-10-22).
- [ ] **`git_safety_hook` unguarded forms** (AGENT-TOOLING-03)
  Not guarded: `git rm`/`git mv`, git aliases, git via `sh -c` or `Invoke-Expression`, a
  lone `&` separator; `git checkout --track origin/x` creates a branch silently
  (.claude/hooks/tests/test_hooks.py:343).
  Source: same plan, :374 (2026-10-22).

### Known limits (commit-format hook)

Source for all five: docs/reference/commit-format.md:76-92 (2026-10-23).

- [ ] **commit-msg hook bypassed by aliases, nested shells, `git.exe` and unparsed global options** (AGENT-TOOLING-15)
  Listed under "Known gaps (not checked)" (:79-80).
- [ ] **Exempt subject prefixes are prefix-only** (AGENT-TOOLING-16)
  Anything after an exempt prefix is unchecked (:81).
- [ ] **Plan slug lookup reads the working tree** (AGENT-TOOLING-17)
  An untracked plan file also validates (:82).
- [ ] **Commit cleanup modelled only from `GIT_EDITOR`** (AGENT-TOOLING-18)
  Explicit `--cleanup=` and `commit.cleanup` are invisible; an exported `GIT_EDITOR=:`
  gets no-editor checking. Fail-closed one way, accepts a `#`-led subject git keeps the
  other (:83-88).
- [ ] **Content after a scissors line is never checked** (AGENT-TOOLING-19)
  Fail-open: the hook stops at the scissors line, git cuts there only under
  `--cleanup=scissors` or `commit -v` (:89-92).

### Deferred

Source for the first four: docs/plans/2026-10-21-2252-arcgame-workflow-port-plan.md,
Deferred section (:366-390, 2026-10-22).

- [ ] **Concurrent-rollout guard hook** (AGENT-TOOLING-06)
  Deny a second `cargo test`/`verifyctl`/`devctl up` while cargo/rustc runs. Enforces a
  MANDATORY rule but was not requested; no such hook in .claude/hooks (re-checked by
  listing the directory and Grep of the two hooks that mention rustc/concurrency).
- [ ] **`rust-nav` LSP daemon, `arcsem`-style index, Codex `.codex` agent definitions** (AGENT-TOOLING-07)
  Not started; listed under the plan's Deferred section.
- [ ] **Repoint "CLAUDE.md hard constraint N" references to `gamebackend.md`** (AGENT-TOOLING-08)
  Case-insensitive Grep (lower bound; misses wrapped comments) for "hard constraint" in
  `*.rs` finds 9 lines in 6 files: tools/archcheck/src/main.rs (4), tools/topiccheck,
  tools/csharp-client-gen, modules/match, modules/gateway, core/remote. The inventory
  counted about 19 sites; the plan says about 40 including scripts. Repointing needs a
  cargo build.
- [ ] **Give "Historical docs are archives" a `core-rules.md` home** (AGENT-TOOLING-09)
  The rule lives only in .claude/agents/docs-writer.md and memory; Grep of .agents/ for
  "archives" finds nothing. Owner decides.
