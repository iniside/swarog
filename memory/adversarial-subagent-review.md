---
name: adversarial-subagent-review
description: Review subagent diffs (and own fixes) adversarially — try to break each fix at its OWN new boundaries; a plausibility read is not a review
metadata: 
  node_type: memory
  type: feedback
  originSessionId: fa1a7334-ddfc-4932-9aa9-dcbe7e8ce691
---

Lukasz's correction (2026-07-12, verbatim spirit: review like "a Pole who nitpicks
everything" — most shipped bugs passed because my subagent reviews were stoned-level
lax): reviewing a diff means attempting to break it, not confirming it matches the
plan step.

**Why:** The 2026-07-12 external audit broke four of my accepted fixes exactly at the
boundary each fix introduced — retention sweep swallowing per-topic errors while
stamping success (`7ca0b51`), hardcoded 3h stall threshold shadowing the configurable
interval, cargo-audit network failure as green SKIP (`b78444f`), scheduler budget
starvation (`addc824`). None needed new information — only hostility during review.
The complementary failure is Codex-style overcorrection (gold-plating tooling), so
the review question is always "what is the MINIMAL closure of the concrete defect,
and what breaks it".

**How to apply:** Follow CLAUDE.md `## Adversarial Subagent Review — MANDATORY`:
(1) per change, name the input/state/ordering/partial-failure that makes it wrong;
(2) attack the fix's own new seams first (partially-failing loops, constants
shadowing knobs, errors folded into success, wrong-scope ownership); (3) verify
against code, never the subagent's summary — confirm negative-path tests hit the
failing branch; (4) if I can't state the fix's failure mode + pinning test, the
review isn't done; (5) bounce findings as a punch list, never silently absorb.

**Refined 2026-07-13:** it is ONE pass by a different method. A clean verdict IS valid
when it enumerates the classes attacked — do NOT mandate re-review on zero findings; that
manufactures findings and recreates the 46-commit carousel. Rigor = the class list, not a
loop. **This applies to my OWN freshly-authored scaffolding too** (agent files, docs,
config) — Lukasz caught me committing 3 agent files without the required pass, and they
carried duplicated-authority (routing table + rules copied into prompts). Review your own
tooling before commit; don't defer to "next task". Related:
[[scope-claims-to-what-was-verified]], [[verify-the-at-risk-path-not-the-safe-one]],
[[specialized-core-agents]].
