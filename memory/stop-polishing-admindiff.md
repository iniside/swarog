---
name: stop-polishing-admindiff
description: "Don't keep hardening admindiff (counts, pairing, accept-reason precision); fix only what blocks a crawl"
metadata:
  node_type: memory
  type: feedback
  originSessionId: 0af9025c-b620-41bb-9924-cba26bbf9324
  modified: 2026-11-24T10:22:07.251Z
---

Owner 2026-11-22: "tego admindiff nie rzezb dalej bo marnujemy na to absurdalne ilosci czasu" — after proof-auditor proposed counting paired mismatch lines in skeleton.rs and re-kinding the span.hint accept line.

**Why:** admindiff is a dev fidelity aid; the time sunk into perfecting its gate exceeds its value.
**How to apply:** change admindiff only when a crawl/diff is actually blocked (e.g. row selection). Reviewer findings that would make admindiff "more airtight" (counting, reason wording, kind precision) are not raised as owner decisions; at most one line in the report. Owner 2026-11-23, about the pre-existing seed-driven reds (mail queue, friends/inbox counts, split lag): "to jest rzezbienie w gownie, zostawiamy to, ten caly testowy harness jest przekombinowany … trzeba go bedzie zrobic inaczej". Leave such reds; don't propose seed-determinism work for them. A redesign of the fidelity harness is a future owner topic, not something to start unasked.

Owner 2026-11-24, when the crawl was blocked by mockup row menus on compared tabs: "ten caly admin diff zostaw, bo mam wrazenie, ze ten tool jest przekombinowany" — admindiff is left out of the rest of the admin-control-plane-redesign plan (23b dropped; later crawl/accept/diff runs not done unless reopened). Don't propose admindiff fixes or crawls as plan work.

Related: [[dev-tooling-tests-backend-not-hostile-users]].
