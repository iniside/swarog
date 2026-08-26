# Research Never Overrules a Decision

Backing evidence for the rule in `.agents/shared/research-navigation.md`. A
decision the owner made is not re-openable because a subagent proved an
alternative is feasible. Feasibility is not permission. Output is one
question, never a revised plan that quietly adopts the rejected shape.

## Decision records (do not reopen)

- **One shared Postgres is the ops model**, not a stepping stone to
  DB-per-service. Record:
  `memory/shared-postgres-is-the-model.md`.
- **North-star extraction is process isolation over network-shaped seams**,
  not an in-process plugin framework. Record:
  `memory/gamebackend-north-star-and-jvm-exploration.md`.
- **Wipe is the current-phase schema strategy.** No data-migration bridges,
  dual-writes, or compatibility columns. `CLAUDE.md` / `.agents/shared/core-rules.md`
  Database and no-compat sections.

## Tells

- Restating the owner's objection so it covers less than he said.
- "Research settles it the other way" about a decision record written from
  his words.
- Reaching for an adjacent system because a subagent proved it can carry
  this.
- Escalating cost into a blocker on an axis he already ruled on. Cost is
  never an argument against his decision.
