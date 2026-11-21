---
name: step13-resume-point
description: "Admin redesign resume point 2026-11-21: Step 14a3 landed (live crawls clean); next Step 14 Nt goldens, then 14b"
metadata:
  node_type: memory
  type: project
  originSessionId: 0af9025c-b620-41bb-9924-cba26bbf9324
  modified: 2026-11-21T13:15:19.747Z
---

Run `git log --oneline` first ([[verify-repo-state-before-resuming]]).

State 2026-11-21: Step 13e complete (13e1-13e6 landed and reviewed; last docs commit marks 13e6). Next is Step 14 (frontend) — owner said stop before it; wait for the owner.
Owner checks open: admindiff crawl after 13e6 (mail, players), popover browser check.

**Why:** owner: "kontynuuj po kolei, zatrzymaj sie przed frontendem".
**How to apply:** check whether a 13e5 commit exists; if so review it, then 13e6.


## State 2026-11-21 (latest)
Step 13e complete (+ inline fix a42eb63b). Step 14 decisions recorded (1882289b, checklist proposals for G2/G7/G8/G10).
14a1 landed 6ce76a3f (reviews clean, docs 6e12adb9). Next: 14a3 (list column, EntityLabel{character} + EntityKindDecl, Purges pending 61, G6 accept line in players.txt; gives pending_purges a caller → clippy green), then 14 Nt (goldens), then 14b.
Menu/modal diffs closed: 84fc8662 (refused item: title only), accept files 05294ed1, todo 9c14b942; monolith + split clean except player.characters (crawl follows a 0-character player, point 6) — awaiting owner on fixing the crawl/seed.

## State 2026-11-21 (14a3 landed)
Crawl fix A 56126e2a (verified live). 14a3: 1135d9ab column, 630ffe45 label/kind, 632eed06 purges pending, 43cf3336 accept+docs; monolith + split diff clean on all ported targets. Known limits in docs/todo/admin.md (14a3, 14). Owner open: P1 — purges link copies core.events address; fix in core/controlplane before port 15 (each port copies it). Next: 14 Nt (goldens, test-author opus), then 14b.
