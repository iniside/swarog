---
name: research-before-every-step
description: "Before every implementer/test-author dispatch, run a thorough research pass that verifies each assumption with path:line; also list at-risk test boundaries in the brief"
metadata:
  node_type: memory
  type: feedback
  originSessionId: 0af9025c-b620-41bb-9924-cba26bbf9324
  modified: 2026-11-22T08:46:18.856Z
---

Before each next step (implementation, test step, live crawl), do a thorough research pass first: every assumption the step relies on (a param reaches the panel, an event is in history, admincheck accepts a shape, the seed creates the data the crawl needs) is checked in code and cited path:line; an unconfirmed assumption stops the dispatch. The brief then names the at-risk test boundaries up front (page-size boundary, cursor crossing groups, both filter halves, DB state after a contract change).

**Why:** owner 2026-11-22: "jak to jest, ze ani raz nie bylo implementacji ktora by zadzialala za pierwszym razem" → "lepiej odrazu zrob research dokladny przed kolejnym krokiem". In Step 16 every part needed a second pass: plan assumed tab params pass through (they did not), Genesis replay vs 7-day retention missed, local DB contract drift, proof-auditor test gaps that were predictable.
**How to apply:** researcher (or code-scout fan-out) before every dispatch; feed its verified map into the brief. Related: [[prose-about-code-is-not-evidence]], [[verify-the-at-risk-path-not-the-safe-one]], [[cite-a-precedent-only-after-reading-it]].
