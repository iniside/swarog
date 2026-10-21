# Core Rules

## Owning Mistakes - RULE

If you ignore a rule or fabricate an API, path, behaviour, or claimed work,
name the mistake, state the correction, and fix it without excuses or praise.
Use two sentences; for a MANDATORY violation name the section and the mismatch.
Do not create memory copies of rules; propose a hook when enforcement is needed.

## Plans And Status Docs - RULE

Store all planning/design/status/progress/summary docs inside the repo — never
on a scratch drive or temp path. The repo is the single source of truth.

- Plans: `docs/plans/YYYY-MM-DD-HHMM-<kebab-topic>-plan.md`
- Status/progress/fix/summary:
  `docs/<subdir>/YYYY-MM-DD-HHMM-<kebab-topic>-<status|progress|fix|summary>.md`
- Reference (durable knowledge): `docs/reference/<topic>.md`

The `-HHMM` suffix is mandatory so files sort chronologically by listing. Never
put plan/status files at repo root or in a temp dir.

Work lands directly on `master`; do not create a branch unless the user
explicitly requests one. Do not use git worktrees.

## Git Safety - MANDATORY

Never `git stash`, `git checkout -- <file>`, `git restore`, or anything that
discards/overwrites uncommitted working-tree changes without the user's
say-so. To inspect old contents use `git show <sha>:<path>`.

Only ever `git reset --soft HEAD~1` to undo a commit *you* just created *this
turn*, and only when nothing else has committed since. Never `git reset` past
commits you did not make this turn. Never `git push --force` or rewrite
published history without explicit instruction. Push only when the user asks.

## Commit After Every Task - MANDATORY

After completing every task — or each independently reviewable, verified part
of a larger task — create a git commit containing only the changes made for
that unit. Do not wait for a long multi-part rollout to finish, and do not
include unrelated pre-existing working-tree changes. If a task produces no
repository changes, no commit is required. A request to commit is implicit in
every task; pushing still requires an explicit user request.

## Commit Message Format - RULE

Use Conventional Commits:

```text
<type>(<scope>): <imperative description>
```

`type` is one of `feat`, `fix`, `perf`, `refactor`, `test`, `docs`, `chore`.
`scope` is the lowercased module/package; multiple scopes are comma-separated
(`fix(match,rating): …`). Do not use bracketed `[Module]` scopes. A commit
implementing a plan step, including its review fixes, ends the description with
`(<plan-slug> step <id>)` (`steps 3-4` for a range); the slug is the plan file
name without the date prefix and `-plan.md`.

The `Co-Authored-By` trailer identifies the model/agent that executed the work.
The active adapter defines the format; the executing model fills in its own
identity from its runtime identity, never copied from an example. More detail:
`docs/reference/commit-format.md`.

## Comments - MANDATORY

Default is no comment. Write one only when it carries what the code cannot
show: a non-obvious invariant, a Postgres/tokio/sqlx gotcha, the reason for a
workaround, or intent invisible from the signature. One line, present tense,
describing what the code does. Doc comments (`///`) on a public contract
surface (`api/*` traits, `core/*` public items) document the contract and are
held to the same truth standard.

Banned: changelog prose ("removed X", "moved to Y", "this used to…", "(Step
3)"); paraphrase of the next line; multi-line blocks restating the body;
comments asserting behaviour the code does not have; exclusivity claims ("the
only place…"). A false comment is a correctness defect — fix it in the same
rollout, never "later". A claim about library or runtime behaviour (tokio,
sqlx, Postgres, quinn) requires reading that code this session and a
`file:line` citation.

Verbose comments hijack review. Findings that are only about comments mean
there are too many comments, not that the diff is clean. A comment flagged in
review is deleted, not reworded. Comment-only findings end the review round and
the orchestrator fixes them inline; no prose-fixing agent is dispatched.

Every implementation dispatch says `comments: default NONE` and names the one
or two things that earn a line in that file.

## No Dual-Write, No Topology Branch, Wipe - MANDATORY

Wipe is the migration strategy (current phase). When a schema or event-contract
change would need a data migration, DROP the affected schemas (or the whole
DB) and boot fresh. Do not build bridges, dual-writes, backfills, versioned
data-migration machinery, or "kept for compatibility" fields. Module `migrate`
stays idempotent DDL (`CREATE … IF NOT EXISTS`), nothing more. If losing dev
data hurts, the answer is a seed script, not a migration.

Modules are topology-blind. No `Option<transport>`, no `if split`, no env
topology branches in domain code. Edge exposure goes through `EDGE_SLOT`;
remote resolution through the registry swap; durable delivery through the bus.
`cmd/*` mains differ only in module list + which QUIC planes the process
serves.

Never keep two rails for the same behaviour. When a step introduces the new
thing, that same step deletes the old one. Two sources of truth are worse than
a broken intermediate build.

## Agent Memory Backup - RULE

Project memory lives outside the repo under the Claude Code scheme
(`$HOME/.claude/projects/<mangled-repo-path>/memory/`) and is mirrored into
the repo at `memory/` so it survives across machines via git. The live path is
the one `scripts/memory-sync` derives, overridable with `CLAUDE_MEMORY_DIR`.
`… path` prints it. The `.ps1` twin and the no-exec-bit invocation form are in
`docs/reference/platform-notes.md`.

- After ANY change to memory (write/update/delete a memory file or
  `MEMORY.md`), run `scripts/memory-sync.sh push` (or `.ps1`) — it mirrors
  live → `memory/` and commits `chore(memory): …`. Do not hand-copy.
- After a `git pull`/sync, run `scripts/memory-sync.sh pull` before relying on
  recall.
