# NavEntry

A page a module adds to the sidebar under a section, with an optional live counter; the shell greys it and adds a red dot when its module is down.

## When to use

`nav.section` (declared once, with an order) and `nav.entry` (a page). Counters are for things the operator acts on: open locks, queued mail, paused subscriptions.

## What the consumer provides

Section `{ id, label, order }`; entry `{ section, label, icon, href, order, count?, countTone?: "warn" | "alert" }`. Markup: `.nav-section > .nav-title` and `a.nav-item` (`.active`, `.down`, `.count`).

## Do and don't

- Do keep labels to two or three words.
- Don't show a counter that is not actionable.
