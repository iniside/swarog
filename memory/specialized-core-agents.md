---
name: specialized-core-agents
description: "three project subagents for core/cross-seam work — which to use when, and why they exist instead of generic reviewers"
metadata: 
  node_type: memory
  type: project
  originSessionId: 88cdd953-b406-40a0-8ab2-6c7eb07acece
  modified: 2026-11-18T14:20:36.122Z
---

`.claude/agents/` holds five specialized personas — three built 2026-07-13 from
[[core-failure-taxonomy]], plus **mockup-implementer** (2026-07-15, built after the
mockup-fidelity failures in [[follow-uilayout-mockup-faithfully]]) and
**test-author** (2026-07-22, ported from ArcGame — see [[split-impl-and-tests]]) —
to close the gaps generic agents missed:

- **core-implementer** — authority-first implementation of a fully-specified step/fix.
  Locate the deciding place BEFORE writing; STOP on hack-on-hack; prove the failing
  branch on the at-risk topology (split, not just monolith). Use for [opus]/[fable]
  implementation lanes on core/* or cross-seam work — NOT mechanical rename sweeps.
  **Scope corrected (2026-07-15, recydywa caught by Lukasz during Weles M0):** ALL
  substantive implementation lanes in this repo go through core-implementer — also
  NEW standalone crates (weles platform containment counted), not only literal
  `core/*` paths; `general-purpose` agents are for research only, never a
  code-writing lane. "It's not core/" is not an exemption.
- **core-reviewer** — adversarial review of code commits and the one plan review;
  see [[adversarial-subagent-review]] (owner 2026-11-18 policy).
- **proof-auditor** — audits the PROOF not the code (coverage-gap/false-pass/
  notapplicable — the verify-net class that ships bugs green). Use on diffs touching
  tests or verify stages, or that claim "proven".
- **mockup-implementer** — UI work against `UILayout/redesign/` (built on `UILayout/design-system/`): mockup = exact spec
  for layout AND data shape; data gaps are decided in the checklist, never by the
  implementer; deviations declared as `admindiff` accept entries. Tier per the UI rule
  in `.agents/shared/planning-dispatch.md` **Implementation Mode** (a port's view part
  runs at the cheap tier). Orchestration lives in the `mockup-implementation` skill
  (`.claude/skills/mockup-implementation/SKILL.md`) — invoke it for any "make it
  look like the mockup" task.
- **test-author** — only for verify gates, splitproof assertions and new test
  harnesses; ordinary tests are written by the implementer (owner 2026-11-18,
  [[split-impl-and-tests]]).
- **code-scout** (2026-11-18) — haiku, read-only search returning file:line
  anchors; LSP-first enforced by .claude/hooks/scout_nav_guard.py.
- **docs-writer** (2026-08-31) — the ONLY lane that writes documentation or
  comment prose: root guidance, `docs/reference`, `docs/roadmap`, plan errata,
  Rust doc-comments. Runs LAST, against landed code; deletes false prose as much
  as it adds true prose. Added because the guardrail hook refused
  core-implementer for doc work three times in one rollout and its other roles
  are read-only or UI-only, so the docs step was being done inline with no lane
  and no review. It immediately caught a false op count I had committed as a
  "fix" — see [[grep-counts-are-lower-bounds]]. Default `model:"sonnet"`.

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
  `split-topology-debugger`, `safe-verification`. Each agent pins `effort:` in frontmatter (medium; code-scout low,
  2026-11-18); the dispatcher still passes model: per call.
