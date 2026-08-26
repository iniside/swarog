# Core Rules

## Owning Mistakes - RULE

Caught ignoring an instruction, violating a documented rule, or fabricating
something (made-up API, invented path, hallucinated behaviour, false claim of
work done):

1. Name the specific mistake directly — no hedging, no burying it in context.
2. Do not minimize, deflect, or rationalize; do not blame tools or ambiguity.
   The response is "you're right, I screwed up on X."
3. State the corrected behaviour concretely.
4. Then fix it. One or two sentences of repentance, not a wall. "Great catch!"
   is not repentance.

For repeat offenses, also save/update the relevant feedback memory.

When caught violating any `## … — MANDATORY` rule, before the fix write a short
(≤8-line) resignation letter addressed to the user: name the exact section,
state explicitly what error was committed (what was done vs what the rule
required), the impact, and the corrective action. Then update memory for the
violated rule.

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

`type` is one of `feat`, `fix`, `refactor`, `test`, `docs`, `chore`. `scope` is
the lowercased module/package; multiple scopes are comma-separated
(`fix(match,rating): …`). Do not use bracketed `[Module]` scopes. Multi-step
rollouts may note `(Step N — …)`.

The `Co-Authored-By` trailer reflects the executing runtime. Adapters define
concrete trailer strings. Do not invent a vendor, model family, or version the
executing agent is not. More detail: `docs/reference/commit-format.md`.

## Comments - MANDATORY

Default is no comment. Write one only when it carries what the code cannot
show: a non-obvious invariant, a Postgres/tokio/sqlx gotcha, the reason for a
workaround, or intent invisible from the signature. One line, present tense,
describing what the code does. Doc comments (`///`) on a public contract
surface (`api/*` traits, `core/*` public items) document the contract and are
held to the same truth standard.

Banned: changelog prose ("removed X", "moved to Y", "this used to…", "(Step
3)"); paraphrase of the next line; multi-line blocks restating the body;
comments asserting behaviour the code does not have. A false comment is a
correctness defect — fix it in the same rollout, never "later".

Verbose comments hijack review. Findings that are only about comments mean
there are too many comments, not that the diff is clean.

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
