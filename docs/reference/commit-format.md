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
  - review-fix commits for a step use the same suffix as the step.
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

## Attribution trailers

Concrete `Co-Authored-By` strings live in the active runtime adapter under
`.agents/adapters/`. The trailer names the executing model/agent, read from
its own runtime identity, never copied from an example. Do not use a Claude
trailer on a Grok/Cursor/Codex commit. Attribution is never a
substitute for the required commit after each completed task.
