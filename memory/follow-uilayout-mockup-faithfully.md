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
first and lift values 1:1 — e.g. GameOps uses fonts Public Sans + IBM Plex Mono (Google Fonts),
bg `#0f1116`, sidebar `#13151b`, cards `#161920` border `#232832`, accent amber `#f5a524`, green
`#34d399`, red `#f5604d`, blue `#7aa2f7`, muted `#9aa0ac`/`#6b7180`; 256px sidebar with
MAIN/OPERATIONS/SYSTEM groups; 64px header with search + Production pill + bell + avatar. The
mockup is a spec, not runnable — port it, keeping dynamic data via the template engine.
See [[gamebackend-north-star-and-jvm-exploration]].
