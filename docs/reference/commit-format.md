# Commit Message Format

Detail for the **Commit Message Format — MANDATORY** rule in [CLAUDE.md](../../CLAUDE.md). The Conventional-Commits rule + the `Co-Authored-By` trailer rule stay in CLAUDE.md; this file holds the scope conventions and examples.

## Conventional Commits

`<type>(<scope>): <imperative description>` — this is the established format, keep using it.

- `type` ∈ `feat`, `fix`, `refactor`, `test`, `docs`, `chore`.
- `scope` is the lowercased module/package (`accounts`, `match`, `rating`, `leaderboard`, `webui`, `admin`, `core`). Multiple scopes comma-separated: `fix(match,rating): …`.
- Multi-step rollouts may note the step in the description: `(Step 1 — A+B+C)`.
- Do **NOT** use bracketed `[Module]` scopes — that is the wrong format.

## Examples

```text
feat(accounts): add Epic OIDC verifier behind EPIC_CLIENT_ID
fix(leaderboard): create schema in Migrate, not Init
test(core): cover cycle detection in the module registry
refactor(admin): move section rendering behind adminapi.Slot
fix(match,rating): assert rating service to a local consumer interface
```

## Co-Authored-By trailer

Reflects the **executing model**, overriding the harness default (which hardcodes `Co-Authored-By: Claude Opus 4.8`). Stamp the model that actually authored the commit:

- Sonnet subagent → `Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>`
- Opus → `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`
- Fable → `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`

When dispatching a code-writing subagent, put **its model's** trailer in the prompt. This is what the trailer audit ("confirm trailers match the intended lane") checks.
