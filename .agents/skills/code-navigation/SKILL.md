---
name: code-navigation
description: How to research and navigate this Rust workspace — LSP-first fallback chain, when to fan out or dispatch code-scout, and the search budget. Use before mapping APIs, callers, trait impls, event wiring or overlap, and in every subagent that reads or searches code.
---

# Code navigation

## Research method

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
reason or do not escalate. Give every code-touching subagent the path of this
file and require it to report the method it used.

## Fallback chain

In order — never skip to grep because it feels faster:

1. **LSP / rust-analyzer** — definition, references, trait implementations,
   call hierarchy, inferred type. Start with one targeted search result to
   establish a file+line+column anchor (the NAME position from
   `workspaceSymbol` or the read line, not `documentSymbol`'s range start),
   then query rust-analyzer. Empty results while rust-analyzer is still
   indexing, or at a range-start line, are not absence.
2. **Targeted read** — small surface, one file end-to-end.
3. **Research subagent** — for broad research, fan out on distinct
   non-overlapping angles (API surface / callers+consumers / event
   publishers+subscribers / config+env wiring). Synthesize in the main agent;
   never write a conclusion off a single subagent. For ONE open search
   question, the main session or an implementation lane may instead dispatch
   `code-scout`. Its answers are leads: the caller verifies every anchor it
   relies on with a targeted read, and a completeness or absence claim counts
   only when labelled LSP-derived, otherwise it is a lower bound. Review and
   proof lanes read evidence themselves and do not delegate it.
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
- A search that takes more than one or two lookups goes to `code-scout`.

On Claude, `.claude/hooks/scout_nav_guard.py` enforces this for
`core-implementer`, `test-author` and `docs-writer`, and
`.claude/hooks/grep_sweep_guard.py` blocks recursive shell greps.
