# Instruction Map

`AGENTS.md` is the mandatory core and action router. Read it plus the active
runtime adapter first, then load only the authorities for the next concrete
action. If that action is unclear, keep it read-only and use the research row
until it is classified. Reuse already-read authorities while they are unchanged.

- `shared/core-rules.md`: owning mistakes, plans/status docs, git safety,
  commit-after-task, commit format, comments, wipe / no dual-write /
  topology-blind, memory backup.
- `shared/research-navigation.md`: research before planning, search mode, Rust
  navigation, decisions are final; routes the full research method.
- `shared/planning-dispatch.md`: plan writing, implementation lanes,
  adversarial diff review, Fix the Authority, refactor safety.
- `shared/gamebackend.md`: architecture (seams, hard constraints, modules,
  commands, dev tooling scope, one test rollout at a time, database, layout).
- `adapters/<runtime>.md`: runtime tools, models, effort, trailers, and artifacts.
- `skills/`: shared skills, cited by skill name.
- `docs/reference/*.md`: full action-specific contracts linked by the router.

Shared authorities win except where an adapter translates runtime mechanics.
