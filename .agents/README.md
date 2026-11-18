# Swarog Instruction Map

`AGENTS.md` is the mandatory core and action router. Read it plus
`adapters/claude.md` first, then load only the authorities for the next
concrete action.

- `shared/core-rules.md`: owning mistakes, plans/status docs, git safety,
  agent effort, commit-after-task, commit format, comments, wipe / no
  dual-write / topology-blind, memory backup.
- `shared/research-navigation.md`: research before planning, decisions are
  final, pointer to the `code-navigation` skill.
- `shared/planning-dispatch.md`: the binding plan, dispatch and review rules
  (threshold, review always a fresh subagent, rounds, structural findings),
  Fix the Authority, refactor safety.
- `shared/gamebackend.md`: architecture (seams, hard constraints, commands,
  one test rollout at a time, database).
- `../.claude/rules/`: area rules loaded by path (domain modules, dev tooling
  scope).
- `adapters/claude.md`: Claude Code agents, models, how effort is set,
  trailers, and artifacts.
- `skills/`: shared skills, cited by skill name; the how-to for planning
  (`research-plan-present`), dispatch (`implementation-dispatch`), review
  (`diff-review`) and navigation (`code-navigation`) lives there.

Shared authorities win except where the adapter translates runtime mechanics.
