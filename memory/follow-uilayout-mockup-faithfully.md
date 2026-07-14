---
name: follow-uilayout-mockup-faithfully
description: "When building UI that has a UILayout/ mockup, translate it verbatim — never improvise visuals; UI translation is [subagent-complex], never mechanical"
metadata: 
  node_type: memory
  type: feedback
  originSessionId: 88cdd953-b406-40a0-8ab2-6c7eb07acece
---

When reproducing any admin/UI screen, the visual source of truth is the repo's mockup
(`UILayout/*.dc.html` — a Claude Design export with the EXACT HTML + inline CSS). Do NOT
invent layout/colours/fonts.

**Why:** caught improvising a whole GameOps theme (own palette/fonts/layout) for the
jvm-kotlin-sketch admin panel while the exact spec sat in `UILayout/GameOps Admin.dc.html`.
User: they'd understand clumsiness from a screenshot, but I had the HTML and CSS. Inventing
when the spec exists reads as incompetence, not creativity.

**How to apply:** before writing any theme.css / template, READ the relevant
`UILayout/*.dc.html` and lift values 1:1 — never quote palette values from this memory; the
mockup is the only source (it gets replaced wholesale, e.g. a new darker scheme landed
2026-07-10). It's a spec, not runnable — port it, keeping dynamic data via the template
engine.

**Dispatch (user-mandated):** UI translation is substantive, judgment-heavy — use
`[subagent-complex]`, never `[subagent-mechanical]`. For mockup-file rewrites Opus
suffices ("do przepisywania pliku opus starczy") — don't burn the top tier.

**Proven workflow (admin restyle 2026-07-14, worked first-try):**
1. One research subagent extracts the FULL spec from the `.dc.html` (token table,
   per-component inline-CSS values, state/transition map from the JS at the bottom,
   menu contents verbatim with line numbers) — this becomes the plan's Context and
   the implementer never improvises.
2. The mockup's `sc-if`/`sc-for`/`{{ }}` DSL is translated to minijinja, never copied.
3. Admin portal visual layer = ONLY `modules/admin/src/theme.css` (tokens in `:root`)
   + `admin.html.tmpl` (+ `modal.html.tmpl`); modules never see HTML/CSS — restyles
   touch zero module Rust.
4. Interactivity under CSP `default-src 'self'`: same-origin script FILES are already
   allowed (only inline is blocked) — vendored `htmx.min.js` (no npm) + ~60 lines
   vanilla `admin.js` (data-* delegation), NO inline handlers, NO `hx-on:` (eval).
5. **Data-gap default (user-settled 2026-07-15, after I got it wrong):** the mockup
   is the spec for the DATA SHAPE too. When the backend lacks a field the mockup
   shows (RARITY/POWER/LEVEL/icons), the default is a DETERMINISTIC decorative fake
   (pure hash of the entity id — no clock/rand, identical across topologies),
   clearly commented as fake. If unsure, ask ONE question listing all gaps
   ("fake or drop?") BEFORE implementing. NEVER silently drop a mockup element and
   NEVER substitute my own filler (the "Character ID"/"Created" KPIs I invented for
   the character modal were exactly this failure — "copy the mockup" means copy the
   mockup, not creative invention).
6. Every render-vs-mockup deviation (element added, dropped, or data substituted)
   must be explicitly declared in the report — an undeclared deviation is treated
   as a violation, same class as fabricating.
7. The loop is closed only by a LIVE render comparison against the mockup section
   (boot, fetch the changed views full + fragment, element-by-element) — green
   tests are not a visual smoke. Mind `include_str!`: templates/CSS are embedded,
   a running fleet needs a rebuild to show edits.
