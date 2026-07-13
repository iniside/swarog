---
name: didnt-forget-scripts-must-self-check
description: "Any tool whose correctness rests on a hand-maintained \"I didn't forget X\" list must mechanically verify that assumption and die with a log naming exactly what drifted"
metadata: 
  node_type: memory
  type: feedback
  originSessionId: 1da53a56-7eba-43b0-83f8-0aaadb50a274
---

Lukasz's rule (2026-07-10, from the split-proof fleet-drift discussion): every
tool that works on the basis of "nie zapomniałem" (a hand-maintained list that
must track some source of truth — files on disk, crates, processes) must CHECK
that assumption itself and, on mismatch, die BEFORE doing work, printing WHY it
died: exactly which entries are missing, which are stale, and what to do about
each.

**Why:** a hand-maintained list silently drifts; a checker that just exits 1 (or
worse, proceeds with a weaker guarantee) turns a forgotten entry into an
invisible coverage hole. The failure must be loud, early, and self-explanatory —
a to-do, not a puzzle.

**How to apply:** preflight before doing work: derive the actual set
(e.g. `cmd/*-svc` dirs), diff against the hand list, and on drift print one line
per missing/stale entry with the corrective action, then exit non-zero.
Current reference implementation: `processctl::FleetSpec::validate_disk`, called by
the `tools/splitproof` preflight, compares the centralized typed fleet with the
`cmd/*-svc` directories. Related: [[never-monolith-only-features]],
[[verify-the-at-risk-path-not-the-safe-one]].
