---
name: specialized-core-agents
description: "three project subagents for core/cross-seam work — which to use when, and why they exist instead of generic reviewers"
metadata: 
  node_type: memory
  type: project
  originSessionId: 88cdd953-b406-40a0-8ab2-6c7eb07acece
---

`.claude/agents/` holds three specialized personas, built 2026-07-13 from
[[core-failure-taxonomy]] to close the two gaps generic agents missed:

- **core-implementer** — authority-first implementation of a fully-specified step/fix.
  Locate the deciding place BEFORE writing; STOP on hack-on-hack; prove the failing
  branch on the at-risk topology (split, not just monolith). Use for [opus]/[fable]
  implementation lanes on core/* or cross-seam work — NOT mechanical rename sweeps.
- **core-reviewer** — class-keyed adversarial review routed by files-touched to the
  taxonomy classes; attacks the fix's OWN new seam. The reliable local "second
  independent reviewer" (Codex was flaky, ~70%). Use after any core/cross-seam diff.
- **proof-auditor** — audits the PROOF not the code (coverage-gap/false-pass/
  notapplicable — the verify-net class that ships bugs green). Use on diffs touching
  tests or verify stages, or that claim "proven".

**Why:** the remediation showed double hostile review WORKED but was costly (46 commits);
the real disease was authorless multi-commit chains (lock/lease 8x) + gates going green.
These target the source, not more review rounds — so they must NOT re-create the carousel.

**How to apply (design corrected 2026-07-13 after Lukasz caught two defects in v1):**
- Agents are THIN: role + routing + output format + ONE pointer to the single source of
  each rule. Classes/routing/attacks live only in [[core-failure-taxonomy]]; the six
  Fix-the-Authority rules + review method + hard constraints live only in CLAUDE.md.
  Copying either into an agent prompt is duplicated-authority (a taxonomy class) — banned.
- Review is ONE independent pass (core-reviewer), a different method than the implementer.
  A **clean verdict is valid** when it enumerates the classes attacked — do NOT mandate
  re-review on zero findings (that manufactures findings = the carousel). proof-auditor runs
  ONLY for diffs touching a verify stage or where the test/gate is the risk, not every fix
  that adds a unit test.
- They compose with — never duplicate — `architecture-review` (seam law),
  `split-topology-debugger`, `safe-verification`. All omit frontmatter `model` so they
  inherit session tier (keeps reviewer >= author tier, per
  [[plan-reviewer-must-be-session-tier]]); dispatcher passes model:/effort per call.
