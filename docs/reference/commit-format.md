# Commit Message Format

Detail for the **Commit Message Format** rule in
[`.agents/shared/core-rules.md`](../../.agents/shared/core-rules.md). This file holds scope conventions and examples.

## Conventional Commits

`<type>(<scope>): <imperative description>` — this is the established format, keep using it.

- `type` ∈ `feat`, `fix`, `perf`, `refactor`, `test`, `docs`, `chore`.
- `scope` is the lowercased module/package (`accounts`, `match`, `rating`, `leaderboard`, `webui`, `admin`, `core`). Multiple scopes comma-separated: `fix(match,rating): …`.
- A commit that implements a plan step ends its description with `(<plan-slug> step <id>)`:
  - `<plan-slug>` is the plan file name without the `YYYY-MM-DD-HHMM-` date prefix and the `-plan.md` suffix: `2026-10-21-2252-arcgame-workflow-port-plan.md` → `arcgame-workflow-port`;
  - `<id>` is the step id exactly as in the plan heading (`3`, `4b`);
  - one commit covering several steps uses a range: `(<plan-slug> steps 3-4)`;
  - review-fix commits for a step use the same suffix as the step and add the `Kind: review-fix` trailer (below);
  - every commit that closes a step ends with the suffix, including docs commits.
- Do **NOT** use bracketed `[Module]` scopes — that is the wrong format.

## Examples

```text
feat(accounts): add Epic OIDC verifier behind EPIC_CLIENT_ID
fix(leaderboard): create schema in migrate, not init
perf(asyncevents): batch checkpoint writes per delivery quantum
refactor(admin): move section rendering behind adminapi::SLOT
fix(match,rating): require a ReportId on match.report
docs(agents): port Owning Mistakes rules (arcgame-workflow-port step 1)
```

## Enforcement

`.claude/hooks/commit_format.py` is the single source of the rule. It runs as the git `commit-msg` hook through the
shim `.claude/hooks/git-commit-msg.sh`, so it checks the final message of every committer. Install once per clone:

```text
cp .claude/hooks/git-commit-msg.sh .git/hooks/commit-msg
```

The shim tries `python`, then `python3`; if neither exists it warns and exits 0 (message unchecked).
`.claude/hooks/git_safety_hook.py` denies `--no-verify`, its abbreviations (`--no-veri…`) and `-n` on `git commit`,
also inside short-option clusters (`-an`, `-nm`) and after a glued-value `-u`/`-S`, so agents cannot skip the hook.

Checked on the message:

- Subject shape `<type>(<scope>[,<scope>]): <text>`; scope is `[a-z0-9][a-z0-9._-]*`, no spaces, no brackets.
- A subject containing `step <id>` or `steps <id>` must END with `(<slug> step <id>)` or `(<slug> steps <a>-<b>)`, and
  `<slug>` must match a `docs/plans` file. Prose "step" with no id is allowed.
- A message whose first line starts with `#` is rejected (git keeps it as the subject under `-m`); a subject with
  leading whitespace is rejected. Only column-0 `#` lines are comments; a scissors line (`# ---- >8 ----`) ends the message.
- Exempt subjects (prefix only): `Merge `, `Revert "`, `fixup! `, `squash! `, `amend! `.

Rejected:

```text
docs: reorganize docs by type                                    (no scope)
feat(Accounts): add Epic verifier                                (uppercase scope)
fix(match): require a ReportId (trackable-commit-format step 1) and more   (suffix not last)
fix(core): drop the stale cache (no-such-plan step 2)            (slug matches no docs/plans file)
```

Accepted:

```text
feat(accounts): add Epic OIDC verifier behind EPIC_CLIENT_ID
refactor(admin,core): move section rendering behind adminapi::SLOT
fix(hooks): reject #-led subjects (trackable-commit-format step 1)
```

Known gaps (not checked):

- A clone without the installed hook.
- `core.hooksPath` overrides: `git -c core.hooksPath=…`, `--config-env`, `GIT_CONFIG_COUNT/KEY/VALUE` env, persistent `git config`.
- Git aliases and nested shells (`bash -c`).
- `git.exe` and git global options the guard's parser does not strip (e.g. `--literal-pathspecs`, `-p`).
- Exempt prefixes are prefix-only: anything after them is unchecked.
- The slug lookup reads the working tree, so an untracked plan file also validates.

## Kind trailer

A commit that fixes review findings of a plan step, or of a named fix, carries `Kind: review-fix`. It is the only
allowed value, at most one per commit; commits without it are ordinary. A review-fix commit of a plan step keeps the
step suffix.

- It sits in the final trailer paragraph together with `Co-Authored-By`, no blank line between. Each `-m` is its own
  paragraph, so pass all trailers in ONE `-m`.
- The `commit-msg` hook rejects an unknown value, more than one `Kind:`, and any `Kind:` line outside the final trailer
  paragraph. It ignores `#` lines and everything below the scissors line. Exempt subjects skip the check.

```text
fix(match): require a ReportId on match.report (trackable-commit-format step 1)

Kind: review-fix
Co-Authored-By: Claude <Family> <Version> <noreply@anthropic.com>
```

## Attribution trailers

The active runtime adapter under `.agents/adapters/` defines the
`Co-Authored-By` trailer format. The executing model/agent fills in its own
identity from its runtime identity, never copied from an example. Do not use a Claude
trailer on a Grok/Cursor/Codex commit. Attribution is never a
substitute for the required commit after each completed task.
