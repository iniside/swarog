# Subagent Dispatch — shared core

Cross-cutting invariants that apply to every subagent call, regardless of phase. Research and implementation use subagents differently (topology, review, writes, and commits); this file holds only what is identical across both.

- **Research dispatch** (parallel fan-out, count bands, read-only, synthesize in main model): [research-mode.md](research-mode.md).
- **Implementation dispatch** (sequential per plan step, execution lane, review between tasks, may commit): [implementation-mode.md](implementation-mode.md).

## The four invariants

1. **Tags describe execution, not provider/model identity.** Use `[inline]`, `[independent]`, `[mechanical]`, `[test-author]`, or `[review]`. Runtime adapters map those to concrete tools and models (`.agents/adapters/`). Claude may write `[opus]` / `[fable]` / `[sonnet]`; adapters translate. Do not put provider-specific model names in durable plans.

2. **Effort does not inherit.** Whatever effort level the work needs (default / think / think hard / ultrathink) must be embedded in the subagent's prompt. Ask the user when a subagent lane's effort was not fixed with the plan.

3. **Code-navigation guidance does not inherit.** Paste the repo-appropriate navigation guidance into every code-touching prompt and require the subagent to report its method; a grep-only sweep is a labelled lower bound. See [research-mode.md](research-mode.md).

4. **Keep prompts concise; batch small tasks.** A subagent prompt is a hand-off, not a transcript: state the goal, files, navigation guidance, effort, verification, and commit boundary, then stop. Batch several small same-shape tasks into one dispatch rather than one call each.
