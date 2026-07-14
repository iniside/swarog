---
name: mockup-implementation
description: Implement or fix admin/UI views from a UILayout/*.dc.html mockup without fidelity drift. Use whenever the task is "make it look like the mockup", a new admin view/modal, a restyle, or a user report that the UI diverges from the mockup. Orchestrates spec extraction → data-gap checklist → mockup-implementer dispatch → live render-vs-mockup smoke → declared deviations.
---

# Mockup Implementation

The mockup under `UILayout/*.dc.html` is the EXACT spec — layout, styling AND data
shape. This skill exists because two documented failures (2026-07-14) came from
resolving spec conflicts silently: mockup elements were dropped/substituted when the
backend lacked their fields, and "done" was claimed without a live render comparison.

## Standing decisions (user-settled — do not re-ask)

- **Data gap ⇒ deterministic decorative fake** by default: pure function of the
  entity id (explicit-width hash `u64`/`u32`, no `usize`, no clock, no rand), value
  formats copied from the mockup, commented as fake. Real fields always win when
  they exist. Only ask (ONE question listing all gaps, fake-or-drop) when fake seems
  actively wrong for an element.
- **Never silently drop or substitute** a mockup element. Undeclared deviation =
  violation.
- **Dispatch lane:** UI work goes to the `mockup-implementer` agent at `[opus]` or
  higher — NEVER a mechanical/sonnet lane, never inline (ad-hoc work still requires
  the user's lane choice; propose `mockup-implementer [opus]`).

## The workflow

1. **Extract the spec.** For a new view or a non-trivial change, one research
   subagent extracts from the `.dc.html`: token table, per-component computed values
   (including the `renderVals()` JS at the bottom — `modalAvStyle`, `rBadge`, `B()`,
   fake-data shapes/formats), state/transition map, verbatim content with line
   numbers. For a small fix, read the relevant mockup lines directly instead.
2. **Build the data-source checklist BEFORE code.** Element-by-element for the view:
   `element → source: real <field> | fake (formula) | drop (needs user)`. Unsettled
   gaps go to the user as one question. This table goes into the implementer's
   prompt and later anchors the review.
3. **Dispatch `mockup-implementer`** (`subagent_type: "mockup-implementer"`,
   explicit `model:` ≥ opus, effort embedded in the prompt) with: the mockup file +
   line ranges, the checklist, exact files to touch, and the repo invariants it must
   keep (it re-reads CLAUDE.md constraints itself). It returns the diff + the
   data-source table + declared deviations.
4. **One adversarial review pass** (`core-reviewer`, model ≥ implementer's): include
   the checklist and mockup line ranges in the prompt so fidelity is a reviewed
   dimension, not just correctness. Findings bounce back to the implementer.
5. **Close the loop with a LIVE render comparison — green tests are not a smoke.**
   `safe-verification` pre-flight → `cargo run -p devctl -- up monolith` → login
   (admin/admin) → fetch every changed view (full page AND `?partial=modal` with
   `HX-Request: true`) → compare element-by-element against the mockup section.
   Templates/CSS are `include_str!`-embedded: edits after the boot need a rebuild
   before they are visible. Stop the fleet when done.
6. **Report with declared deviations.** The final message lists every
   render-vs-mockup difference explicitly ("none" is a claim to be made, not a
   default), the data-source table, and what the live smoke actually checked.

## Repo mechanics cheat-sheet

- Visual layer = `modules/admin/src/theme.css` + `admin.html.tmpl`/`modal.html.tmpl`
  only; modules emit `adminapi` widgets. New visual need ⇒ additive widget-vocabulary
  change (`#[serde(default)]`), rendered via the shared macro in BOTH templates.
- Mockup DSL (`sc-if`/`sc-for`) → minijinja; CSP unchanged (no inline JS, no
  `hx-on:`); vendored htmx + `admin.js` data-* delegation.
- `api/admin/api` changes ⇒ `--bless-public-api` (additive), check contract-golden,
  grep `tools/splitproof` + `tools/admincheck` for pinned strings.
- Terminal gate for the whole rollout: ONE `verifyctl` manifest (usually
  `--all --strict`), after the review and smoke, per `safe-verification`.
