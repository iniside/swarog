# Research And Navigation

## Research Before Planning - MANDATORY

This is a modular monolith built on Open/Closed — new features are *new code*,
not edits to existing code. Before any plan proposing a new module, service,
event, or admin section (or a replacement), first map the overlapping existing
systems. The three seams (module registry, service registry
`registry::provide` / `registry::require`, event bus) plus the contribution
slots mean a capability you want often already exists or has a near-twin. For
each candidate, document in the plan's Context: what it does, how it differs,
and an explicit **"why not extend / depend on X"**. A plan that adds a module
without that rationale is incomplete — lead with evidence, not enthusiasm for
new code.

## Research / Search Mode - RULE

Ask "how should I research this?" only when the method changes the answer — a
wide API-surface map, an all-callers sweep, a data-flow trace across modules,
locating wiring, surveying overlap. Not for a lookup that can just be run:
pick the method, run it, and say which one produced the answer.

State the unresolved question and give it one owner. Reuse finished research
unless it is missing, stale, contradicted, or insufficient. A handoff provides
verified source anchors, conclusions, and open questions.

Stop when the evidence supports the decision or change. Expand only for a
concrete dependency, contradiction, or failing case; if the available methods
cannot resolve a point, state the uncertainty instead of re-searching.

Never default to grep. One pass misses trait implementations, macro-generated
RPC glue, typed event wiring, and shared registry keys / contribution slots.
Every grep sweep is a labelled lower bound, never the API.

Method menu: `docs/reference/research-mode.md`. Agent-call invariants:
`docs/reference/subagent-dispatch.md`. Paste the navigation chain into every
code-touching subagent's prompt and require it to report the method it used —
it does not inherit.

## Navigation - RULE

Fallback chain, in order — never skip to grep because it feels faster:

1. **LSP / rust-analyzer** — definition, references, trait implementations,
   call hierarchy, inferred type. Preferred for "where is X defined / who
   calls Y / what implements this trait". Start with one targeted search
   result to establish a file+line+column anchor, then query rust-analyzer.
   Empty results while rust-analyzer is still indexing are not absence.
2. **Targeted read** — small surface, one file end-to-end.
3. **Research subagent** — fan out on distinct non-overlapping angles (API
   surface / callers+consumers / event publishers+subscribers /
   config+env wiring). Read-only. Synthesize in the main agent; never write
   a conclusion off a single subagent.
4. **Grep/Glob** — only when nothing else fits, and a labelled lower bound.
   Once grep locates a symbol, re-escalate with the now-known file+symbol.

Never hand symbol navigation to a grep-sweeping agent, and never pre-authorize
grep in a handoff. Dispatch one subagent per independent question; there is no
fixed count.

## Research Never Overrules a Decision - MANDATORY

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
