---
name: stop-polishing-admindiff
description: "Don't keep hardening admindiff (counts, pairing, accept-reason precision); fix only what blocks a crawl"
metadata:
  node_type: memory
  type: feedback
  originSessionId: 0af9025c-b620-41bb-9924-cba26bbf9324
  modified: 2026-11-24T17:10:50.355Z
---

Owner 2026-11-22: "tego admindiff nie rzezb dalej bo marnujemy na to absurdalne ilosci czasu" — after proof-auditor proposed counting paired mismatch lines in skeleton.rs and re-kinding the span.hint accept line.

**Why:** admindiff is a dev fidelity aid; the time sunk into perfecting its gate exceeds its value.
**How to apply:** change admindiff only when a crawl/diff is actually blocked (e.g. row selection). Reviewer findings that would make admindiff "more airtight" (counting, reason wording, kind precision) are not raised as owner decisions; at most one line in the report. Owner 2026-11-23, about the pre-existing seed-driven reds (mail queue, friends/inbox counts, split lag): "to jest rzezbienie w gownie, zostawiamy to, ten caly testowy harness jest przekombinowany … trzeba go bedzie zrobic inaczej". Leave such reds; don't propose seed-determinism work for them. A redesign of the fidelity harness is a future owner topic, not something to start unasked.

Owner 2026-11-24, when the crawl was blocked by mockup row menus on compared tabs: "ten caly admin diff zostaw, bo mam wrazenie, ze ten tool jest przekombinowany" — admindiff is left out of the rest of the admin-control-plane-redesign plan (23b dropped; later crawl/accept/diff runs not done unless reopened). Don't propose admindiff fixes or crawls as plan work.

Owner 2026-11-24 (later, after I listed stale admindiff accept lines among leftovers of admin-second-pass steps 1-5b): "ZOSTAW TEN ADMINDIFF to idzie do kasacji albo refaktoru". admindiff is slated for deletion or a refactor: do not report stale accept lines or stale admindiff prose as leftovers or owner decisions at all, and tell reviewers/implementers not to list them.

Related: [[dev-tooling-tests-backend-not-hostile-users]].
