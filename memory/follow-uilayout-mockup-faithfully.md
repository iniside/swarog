---
name: follow-uilayout-mockup-faithfully
description: "When building UI that has a UILayout/ mockup, translate it verbatim — never improvise visuals"
metadata: 
  node_type: memory
  type: feedback
  originSessionId: eb8d3819-1e67-4a42-9058-589f90144fc1
---

When reproducing any admin/UI screen, the visual source of truth is the repo's mockup
(`UILayout/*.dc.html` — a Claude Design export with the EXACT HTML + inline CSS: colours, fonts,
spacing, sidebar/header structure, SVG icons). CLAUDE.md says "Visual direction comes from
`UILayout/`". Do NOT invent layout/colours/fonts.

**Why:** caught improvising a whole GameOps theme (own palette/fonts/layout) for the
jvm-kotlin-sketch admin panel while the exact spec sat in `UILayout/GameOps Admin.dc.html`. The
user's words: they'd understand the clumsiness if I were working from a screenshot, but I had the
HTML and CSS. Inventing when the spec exists reads as incompetence, not creativity.

**How to apply:** before writing any theme.css / template, READ the relevant `UILayout/*.dc.html`
first and lift values 1:1 — never quote palette values from this memory; the mockup file is the
only source (it gets replaced wholesale: 2026-07-10 a NEW `GameOps Admin.dc.html` landed with a
changed colour scheme — darker bg `#0a0b0e`/cards `#0e0f12`, borders `#1b1e24`/`#22262e` — and the
extensibility baseline: player-row `⋯` menu → View Characters / View Inventory, player-scoped
Characters/Inventory sub-pages with "‹ Players" back header, character-card `⋯` menu → View/
View Inventory modals). Adopting the new colour scheme is IN SCOPE of the admin-extensibility
work. The mockup is a spec, not runnable — port it, keeping dynamic data via the template engine.

**Dispatch (user-mandated, emphatic):** UI translation of this mockup must NEVER go to a
`[sonnet]` lane — Fable (or Opus) only. This is a hard per-user rule on top of CLAUDE.md's
"Visual/UI design is never [sonnet]".
See [[gamebackend-north-star-and-jvm-exploration]].
