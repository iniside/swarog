# Research And Navigation

## Research Before Planning

This is a modular monolith built on Open/Closed — it grows by addition (new
modules, contract traits, and subscriptions), with no module importing another's
implementation. Before any plan proposing a new module, service,
event, or admin section (or a replacement), first map the overlapping existing
systems. The three seams (module registry, service registry
`registry::provide` / `registry::require`, event bus) plus the contribution
slots mean a capability you want often already exists or has a near-twin. For
each candidate, document in the plan's Context: what it does, how it differs,
and an explicit **"why not extend / depend on X"**. A plan that adds a module
without that rationale is incomplete — lead with evidence, not enthusiasm for
new code.

## Research / Search Mode

Ask "how should I research this?" only when the method changes the answer — a
wide API-surface map, an all-callers sweep, a data-flow trace across modules,
locating wiring, surveying overlap. Not for a lookup that can just be run:
pick the method, run it, and say which one produced the answer.

State the unresolved question and give it one owner; the main agent does not
repeat a delegated search. Reuse finished research unless it is missing,
stale, contradicted, or insufficient. A handoff provides verified source
anchors, conclusions, and open questions.

Stop when the evidence supports the decision or change. Expand only for a
concrete dependency, contradiction, or failing case; if the available methods
cannot resolve a point, state the uncertainty instead of re-searching.

Never default to grep. One pass misses trait implementations, macro-generated
RPC glue, typed event wiring, and shared registry keys / contribution slots.
Every grep sweep is a labelled lower bound, never the API.

Research subagents are read-only, do not commit, and are synthesized in the
main agent. Listing-only research runs on the cheapest model; escalating a
wide fan-out to the top tier multiplies its cost by its width, so name the
reason or do not escalate. Paste the navigation chain into every
code-touching subagent's prompt and require it to report the method it used —
it does not inherit.

## Navigation

Fallback chain, in order — never skip to grep because it feels faster:

1. **LSP / rust-analyzer** — definition, references, trait implementations,
   call hierarchy, inferred type. Preferred for "where is X defined / who
   calls Y / what implements this trait". Start with one targeted search
   result to establish a file+line+column anchor (the NAME position from
   `workspaceSymbol` or the read line, not `documentSymbol`'s range start),
   then query rust-analyzer. Empty results while rust-analyzer is still
   indexing, or at a range-start line, are not absence.
2. **Targeted read** — small surface, one file end-to-end.
3. **Research subagent** — for broad research, fan out on distinct
   non-overlapping angles (API surface / callers+consumers / event
   publishers+subscribers / config+env wiring). Read-only. Synthesize in the
   main agent; never write a conclusion off a single subagent. For ONE open
   search question, the main session or an implementation lane may instead
   dispatch the read-only search lane. Its answers are leads: the caller
   verifies every anchor it relies on with a targeted read, and a
   completeness or absence claim counts only when labelled LSP-derived,
   otherwise it is a lower bound. Review and proof lanes read evidence
   themselves and do not delegate it.
4. **Grep/Glob** — only when nothing else fits, and a labelled lower bound.
   Once grep locates a symbol, re-escalate with the now-known file+symbol.

Never hand symbol navigation to a grep-sweeping agent, and never pre-authorize
grep in a handoff. Dispatch one subagent per independent question; there is no
fixed count.

## Search budget

Every search result and file view stays in the agent's context and is re-read
on every later turn, so its cost grows with the length of the task.

- Non-Rust text and strings: grep scoped to a folder. List first (`rg -l` /
  `rg -c`), then read matches with a head limit and at most `-C 2`. A
  repo-wide check for a deleted symbol or plain text (LSP cannot find a
  deleted symbol) is a Bash `rg` carrying `# nav-ok`. Use `rg`, not a
  recursive `grep -r`.
- File views: about 100 lines around the anchor; a whole file only when the
  step changes most of it. Never view a range already in context unless the
  file was edited since; no repeated `sed -n` / `head` / `tail` slices of one
  file.
- A search that takes more than one or two lookups goes to the search lane.

On Claude, `.claude/hooks/scout_nav_guard.py` enforces this for
`core-implementer`, `test-author` and `docs-writer`, and
`.claude/hooks/grep_sweep_guard.py` blocks recursive shell greps.

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
