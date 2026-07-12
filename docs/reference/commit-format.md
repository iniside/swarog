# Commit Message Format

Detail for the **Commit Message Format — MANDATORY** rule in [AGENTS.md](../../AGENTS.md). This file holds scope conventions and examples.

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

## Attribution trailers

Do not require or invent model-specific `Co-Authored-By` trailers. If active tooling adds an attribution trailer, it must name the actual contributing tool or agent without a fabricated provider, model family, or version. Attribution is never a substitute for the required commit after each completed task or independently reviewable part.
