# Agent tooling

Last updated: 2026-10-23

## In progress

_None._

## Open

### Known limits (arcgame-workflow-port step 9)

- [ ] **`git_safety_hook` false denies and path-resolution gaps**
  AGENT-TOOLING-02. `git -C <dir>` path checks resolve against the payload cwd;
  `echo 'git' 'stash'` is a false deny; `git reset <deleted-file>` is denied; a file
  named like a branch is ambiguous; `git checkout :/msg` is not treated as a path; `git reset --soft HEAD~`
  without a digit is denied (.claude/hooks/tests/test_hooks.py:339); existing-path
  checkout is denied when `git rev-parse` cannot run. The owner chose one final fix and
  no third review round.
  Source: docs/plans/2026-10-21-2252-arcgame-workflow-port-plan.md:369 (2026-10-22).
- [ ] **`git_safety_hook` unguarded forms**
  AGENT-TOOLING-03. Not guarded: `git rm`/`git mv`, git aliases, git via `sh -c` or
  `Invoke-Expression`, a lone `&` separator; `git checkout --track origin/x` creates a
  branch silently (.claude/hooks/tests/test_hooks.py:343).
  Source: docs/plans/2026-10-21-2252-arcgame-workflow-port-plan.md:374 (2026-10-22).

### Known limits (commit-format hook)

- [ ] **Hook bypasses: aliases, nested shells, `git.exe`, unparsed global options**
  AGENT-TOOLING-15. The commit-msg hook does not see these forms.
  Source: docs/reference/commit-format.md:79-80 (2026-10-23).
- [ ] **Prefix-only exempt subjects**
  AGENT-TOOLING-16. Anything after an exempt prefix is unchecked.
  Source: docs/reference/commit-format.md:56 (2026-10-23).
- [ ] **Plan slug lookup reads the working tree**
  AGENT-TOOLING-17. An untracked plan file also validates.
  Source: docs/reference/commit-format.md:47-48 (2026-10-23).
- [ ] **Commit cleanup modelled only from `GIT_EDITOR`**
  AGENT-TOOLING-18. Explicit `--cleanup=` and `commit.cleanup` are invisible; an
  exported `GIT_EDITOR=:` gets no-editor checking. Fail-closed one way, accepts a
  `#`-led subject git keeps the other.
  Source: docs/reference/commit-format.md:49-52 (2026-10-23).
- [ ] **Content after a scissors line is never checked**
  AGENT-TOOLING-19. Fail-open: the hook stops at the scissors line, git cuts there only
  under `--cleanup=scissors` or `commit -v`.
  Source: docs/reference/commit-format.md:52-53 (2026-10-23).

### Deferred

- [ ] **Concurrent-rollout guard hook**
  AGENT-TOOLING-06. Deny a second `cargo test`/`verifyctl`/`devctl up` while cargo/rustc
  runs. Enforces a MANDATORY rule but was not requested; no such hook in .claude/hooks
  (re-checked by listing the directory and Grep of the two hooks that mention
  rustc/concurrency).
  Source: docs/plans/2026-10-21-2252-arcgame-workflow-port-plan.md:384-385 (2026-10-22).
- [ ] **`rust-nav` LSP daemon, `arcsem` index, Codex agent definitions**
  AGENT-TOOLING-07. Not started; listed under the plan's Deferred section.
  Source: docs/plans/2026-10-21-2252-arcgame-workflow-port-plan.md:386 (2026-10-22).
- [ ] **Repoint "CLAUDE.md hard constraint N" references to `gamebackend.md`**
  AGENT-TOOLING-08. Grep over `*.rs` for `CLAUDE\.md[^\n]{0,20}constraint`
  plus case-insensitive "hard constraint" (lower bound) finds 20 lines in 7 files:
  tools/archcheck/src/main.rs (14), tools/csharp-client-gen/src/scrape_tests.rs,
  tools/topiccheck/src/tests.rs, modules/match/src/tests.rs, modules/config/src/lib.rs,
  modules/gateway/src/lib.rs, core/remote/src/resolve.rs (1 each). The plan says about 40
  including scripts and weles. Repointing needs a cargo build.
  Source: docs/plans/2026-10-21-2252-arcgame-workflow-port-plan.md:380-382 (2026-10-22).
- [ ] **Give "Historical docs are archives" a `core-rules.md` home**
  AGENT-TOOLING-09. The rule lives only in .claude/agents/docs-writer.md and memory;
  Grep of .agents/ for "archives" finds nothing. Owner decides.
  Source: docs/plans/2026-10-21-2252-arcgame-workflow-port-plan.md:378-379 (2026-10-22).
