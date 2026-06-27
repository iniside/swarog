# Implementation Mode

Detail for the **Implementation Mode — MANDATORY** rule in [CLAUDE.md](../../CLAUDE.md). The core (the lanes, "every subagent passes explicit `model:`", the trailer audit) stays in CLAUDE.md. Cross-cutting Agent-call rules (explicit `model:` + the map, effort/chain don't inherit, trailer, concise prompts) live in [subagent-dispatch.md](subagent-dispatch.md). This file holds the lane heuristic, the implementation-specific dispatch shape, and refactor safety.

## Lanes — concrete models, not a tier alias

Dispatch is decided **per plan step at plan-writing time**, not per session. Tags name a **concrete model** so you can pick the cheapest model that's strong enough — there is no tier-relative `[session]` alias.

- `[inline]` — main model writes in this context. **No independent review.** Reserved for genuine mid-edit judgment that **can't be handed off**: the decision depends on context the main model is holding live (an in-flight design it's actively shaping, a call that hinges on something it just read and can't cheaply re-pack into a subagent prompt). Default complex work to a subagent lane, not `[inline]`; choose `[inline]` only when the hand-off itself would lose the needed context.
- `[fable]` — Fable 5 subagent. The top tier and the default for complex/correctness-critical work (new API design, the bus/registry seams, lifecycle ordering, cross-module context) **once Fable is the session model**. A subagent runs in a **separate context**, so the main model reviews its diff from the outside instead of grading its own homework — the review boundary is the whole point.
- `[opus]` — Opus 4.8 subagent. Substantive implementation where Opus suffices — strong but lighter/cheaper than Fable; use when the step is real work but doesn't need the top tier. **While the session is Opus (Fable not active), `[opus]` doubles as the top-tier independent-review lane** — same-tier-as-inline-in-a-separate-context, the role `[session]` used to fill.
- `[sonnet]` — Sonnet subagent. Mechanical work: rename sweeps, scaffolding, N-similar edits, applying a fully-specified plan step, compile fixes, tests from an existing pattern, JSON/config. **Never burn a higher-tier model on a rename** — if a step is mechanical it is `[sonnet]` even when the surrounding steps are higher.

Visual/UI design (the admin theme, layout, match-a-mockup work) is never `[sonnet]`.

The user approves the tags together with the plan (call them out at ExitPlanMode) — that approval replaces the old blanket "inline or subagents?" question. Ask it only for untagged/ad-hoc work (no plan), and if any step is a subagent lane, also ask **"what effort level?"** (effort does NOT inherit — embed it in the prompt; see [subagent-dispatch.md](subagent-dispatch.md)). Mid-rollout, do not re-litigate a tag: a tagged step that turns out to need different handling gets a follow-up question, not a silent lane switch.

## How implementation dispatch differs from research

Implementation subagents run **sequentially per plan step** (no parallel fan-out for sequential steps), on the **lane's model** (top tier or `[sonnet]`, not the cheap research tiers), are **read-write**, may **commit their own work**, and get a **diff review between tasks** instead of a synthesis pass. (Research is the opposite: parallel fan-out, cheap models, read-only, synthesized in the main model — [research-mode.md](research-mode.md).)

## Dispatch rules (implementation-specific)

The explicit-`model:` rule and the trailer audit are cross-cutting — see [subagent-dispatch.md](subagent-dispatch.md). On top of those, implementation adds:

1. **Review between tasks.** Main model reviews each diff against the plan step (did what the plan said? touched out-of-scope files? introduced conflicting patterns — a module importing another module's package, a cross-module foreign key, a raw `e.Data.(T)` assert, an event publish used where a sync service was needed?) before dispatching the next. No parallel fan-out for sequential plan steps.
2. **Trust but verify.** Read the actual edits — self-reports describe intent, not result.
3. **Commit after each task.** Granular history beats per-commit-compiling — `git add` + `git commit` right after a task verifies. **Subagents MAY commit their own work** (before main-model review) — we want full granular history. The main-model review still runs afterward and catches problems; a bad subagent commit is fixed with a follow-up commit, never by discarding history.

## Refactor safety

- **Verify after dep/wiring changes with a real build:** `go build ./...` then `go vet ./...`, and `go test ./...` for the registry/lifecycle tests. Don't trust a grep "no consumers found" — confirm it compiles.
- **The Go compiler rejects import cycles.** That backstops constraint #2 (modules never import each other) — if reuse would close a cycle, you've violated the architecture, not hit a tooling limit. Resolve it through the bus (async) or a consumer-defined service interface from the registry (sync), never by importing the other module's package.
- **Delete through dying chains.** When a file depends on a dying type, delete the file too — don't shim the survivor around the dying API. Ask "is the consumer still meaningful?", not "can it survive?".
