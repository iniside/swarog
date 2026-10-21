# Subagent Dispatch — shared core

Cross-cutting invariants that apply to every subagent call, regardless of phase. Research and implementation use subagents differently (topology, review, writes, and commits); this file holds only what is identical across both.

- **Research dispatch** (parallel fan-out per independent question, read-only, synthesize in main model): [research-mode.md](research-mode.md).
- **Implementation dispatch** (sequential per plan step, execution lane, review between tasks, may commit): [implementation-mode.md](implementation-mode.md).

## The six invariants

1. **Tags describe execution, not provider/model identity.** Use `[inline]`, `[independent]`, `[mechanical]`, `[test-author]`, or `[review]`. Runtime adapters map those to concrete tools and models (`.agents/adapters/`). Claude may write `[opus]` / `[fable]` / `[sonnet]`; adapters translate. Do not put provider-specific model names in durable plans. The model/tool choice is always explicit — never rely on inheritance or hidden defaults. Pre-flight every Agent call: confirm `model:` is present before sending.

2. **Effort does not inherit.** Set it with the Agent tool's `effort:` parameter; default `"medium"` unless the owner states a level. Prompt wording does not configure effort.

3. **Code-navigation guidance does not inherit.** Paste the repo-appropriate navigation guidance into every code-touching prompt and require the subagent to report its method; a grep-only sweep is a labelled lower bound; never pre-authorize grep. See [research-mode.md](research-mode.md).

4. **Commit trailer = the EXECUTING model.** When a subagent commits its own work, state the adapter's trailer format in the prompt; the subagent fills in its own identity. After a multi-subagent rollout, audit recent trailers against the intended lanes and surface mismatches immediately. Detail: [commit-format.md](commit-format.md).

5. **Fresh, bounded context by default.** Provide the task-specific facts and source anchors needed to act; include conversation history only when it cannot be conveyed in a self-contained prompt. Reviewers always start fresh.

6. **Keep prompts and handoffs concise.** A prompt states the goal, authority, owned files, navigation chain, verification, and commit boundary, then stops. The result states the outcome, verified source anchors, conclusions, open questions, what was and was not validated, and known gaps. Batch small same-shape tasks only when one owner and one question make that coherent.
