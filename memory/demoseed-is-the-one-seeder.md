---
name: demoseed-is-the-one-seeder
description: "Owner rule 2026-11-17 — no per-module seed tools; demoseed seeds via public API only (realistic); states no API produces stay data-difference accept entries; mockup gaps go to the next iteration, don't polish endlessly"
metadata:
  node_type: memory
  type: feedback
  originSessionId: 6694aaef-849f-495c-bec1-e211bdc7a370
  modified: 2026-11-17T15:35:11.667Z
---

Do not propose a new per-module seed tool such as `mailctl seed-mockup`. `demoseed` is the one seeder, and it writes ONLY through the public API, because that is how real data arrives. Admin redesign plan Step 24b widens it to every kind of data each module's API can create.

A designed mockup state that no public API can produce, such as a mail that retries or parks, stays a `data difference` accept entry. The admin panel does not have to show what never happens. `eventctl seed-mockup` stays as the Event plane's dev fixture.

**Why:** owner, in three statements:
- "nie będziemy ciągle dorabiać seedów"
- "demoseed dalej będzie pisał tylko przez publiczne API, bo tak jest realistycznie, panel admina nie musi pokazywać dosłownie wszystkiego"
- "ewentualne dziury z funkcjonalności z mockupa naprawimy kolejną iteracją, bo inaczej będziemy tu rzeźbić w nieskończoność"

**How to apply:**
- A port adds no seed.
- Close only the contract gaps the checklist decided, then move on.
- A leftover mockup gap goes to the next iteration, not into this plan's fix loop. Don't let a port grow into polishing.

Related: [[cheap-tier-ports-fix-workflow-not-model]].
