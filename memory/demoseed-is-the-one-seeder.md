---
name: demoseed-is-the-one-seeder
description: "Owner rule 2026-11-17 — no per-module seed tools (no mailctl etc.); demoseed seeds every module's data; until then missing designed data is a data-difference accept entry"
metadata:
  node_type: memory
  type: feedback
  originSessionId: 6694aaef-849f-495c-bec1-e211bdc7a370
  modified: 2026-11-17T15:31:34.819Z
---

Do not propose a new per-module seed tool, such as `mailctl seed-mockup`, when a port's page needs data that a fresh fleet lacks. `demoseed` is the one seeder. It will seed every kind of data each module can create (admin redesign plan, Step 24b). `eventctl seed-mockup` folds into it there.

**Why:** owner: "nie będziemy ciągle dorabiać seedów, mamy demoseed i on powinien po prostu zaseedować bazę wszystkimi danymi z każdego modułu, które może tworzyć. Tak będzie prościej." For now: "zróbmy część wizualną i dodajmy brakujący kontrakt".

**How to apply:**
- A port adds no seed. A designed state that a fresh fleet lacks becomes a `data difference` accept entry, and Step 24b removes those entries.
- Contract gaps are still closed in part `a`.
- Step 24b itself must decide how demoseed reaches states that have no public API (mail retry and park, event pause), because today it writes only through the gateway.

Related: [[cheap-tier-ports-fix-workflow-not-model]].
