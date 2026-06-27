# Subagent Dispatch — shared core

Cross-cutting invariants that apply to **every** `Task`/`Agent` call, regardless of phase. Research and implementation use subagents *differently* (topology, model tier, review, commits — see the per-phase docs below); this file holds only what is identical across both, so it lives in one place instead of drifting across copies.

- **Research dispatch** (parallel fan-out, cheap models, count bands, read-only, synthesize in main model): [research-mode.md](research-mode.md).
- **Implementation dispatch** (sequential per plan step, lane→model, review between tasks, may commit): [implementation-mode.md](implementation-mode.md).

## The five invariants

1. **`model:` is always explicit.** Tags name a concrete model; there is no "inherit" path you should rely on. (Unlike the ArcGame repo this style comes from, this project has no `PreToolUse` hook enforcing it — so it is on discipline. Pre-flight every Agent call and confirm the field is present.) Map:
   - `[fable]` → `model:"fable"` (top-tier complex, when Fable is the session model)
   - `[opus]` → `model:"opus"` (substantive complex; also top-tier while session is Opus)
   - `[sonnet]` → `model:"sonnet"` (mechanical)
   - listing-only research (enumerate exported funcs, dump a package, zero reasoning) → `model:"haiku"`

2. **Effort does NOT inherit through the Agent tool.** Whatever effort level the work needs (default / think / think hard / ultrathink) must be **embedded in the subagent's prompt** — it does not carry from the parent. Ask the user the level for any subagent lane when it isn't already fixed by the plan.

3. **The code-navigation guidance does NOT inherit.** Any code-touching subagent has a system prompt that defaults to grep/glob. Paste the nav guidance (`gopls/LSP first — definition/references/implementations; grep/glob is a labelled lower bound`) into its prompt and **require it to report which method it used**; never pre-authorize grep. See [research-mode.md](research-mode.md).

4. **Commit trailer = the EXECUTING model.** `[fable]`/inline-Fable → `Claude Fable 5`; `[opus]`/inline-Opus → `Claude Opus 4.8`; `[sonnet]` → `Claude Sonnet 4.6` (all `<noreply@anthropic.com>`). When the subagent commits its own work, put **its** trailer in the prompt. After a multi-subagent rollout, before "done": `git log -<N> --format="%h %B" | grep "Co-Authored"` and confirm trailers match each lane — surface mismatches immediately. Detail: [commit-format.md](commit-format.md).

5. **Keep prompts concise; batch small tasks.** A subagent prompt is a hand-off, not a transcript — state the goal, the files, the nav guidance (invariant 3), the trailer (invariant 4), and the effort (invariant 2), then stop. Batch several small same-shape tasks into one dispatch rather than one call each.
