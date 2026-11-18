# Research And Navigation

## Research Before Planning

Before any plan proposing a new module, service, event, or admin section (or a
replacement), map the overlapping existing systems; the plan's Context gives
each candidate an explicit **"why not extend / depend on X"**. The procedure
is the `research-plan-present` skill.

## Navigation

Research method, the LSP-first fallback chain and the search budget: the
`code-navigation` skill (`.agents/skills/code-navigation/SKILL.md`). Every
grep sweep is a labelled lower bound, never the API.

## Research Never Overrules a Decision

A decision the owner made is not re-openable by a research finding.
Feasibility is not permission — that a rejected system could be reused says
nothing about whether he wants the coupling, and the coupling is what he
rejected. Output is one question, never a revised plan that quietly adopts
the rejected shape.

Tells: restating his objection so it covers less; "research settles it the
other way" about a decision record written from his own words; reaching for
an adjacent system because a subagent proved it can carry this; escalating a
cost or edge case into a blocker on an axis he already ruled on.

Restate the chosen shape in the plan's Context in his words and check each
step against it before dispatch. A step that needs the rejected shape is a
blocking question, not a substitution. Cost is never an argument against his
decision.

Decided examples that stay closed: one shared Postgres is the ops model, not
a stepping stone to DB-per-service
(`memory/shared-postgres-is-the-model.md`); the north-star extraction is
process isolation over network-shaped seams, not an in-process plugin
framework (`memory/gamebackend-north-star-and-jvm-exploration.md`); wipe is
the current-phase schema strategy.
