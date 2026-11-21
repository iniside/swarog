---
name: step13-resume-point
description: "Admin redesign resume point 2026-11-21: Step 15 (wallet) 15a1-15a3 + 15t done; next 15a4 after owner answers Q1-Q3"
metadata:
  node_type: memory
  type: project
  originSessionId: 0af9025c-b620-41bb-9924-cba26bbf9324
  modified: 2026-11-21T20:08:26.426Z
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

## State 2026-11-21 (Step 14 complete)
eae03901 Events page address in controlplane (EVENTS_PAGE_MODULE/EVENTS_PAGE), 5eedd976 clippy fix, Nt dcfa7cb9+873426bf (splitproof CH0-CH4; break = remove peer_face("characters") in cmd/admin-svc/src/lib.rs, NOT main.rs with_peer), 14b clean crawls both topologies, plan bb903d85. Next: port 15 (wallet) — owner approved Step 14 only; ask before starting 15.

## State 2026-11-21 (Step 15 in progress)
Checklist docs/plans/2026-11-21-step15-wallet-checklist.md (§7 decisions). Done: 15a3 ledger panel 473062ec + accept 3b2fd5ec, 15t splitproof [LG1]/[LG1m] f74e2b75 (reviews ACCEPT), todo b0832045. Next: 15a4 (create currency, G13), 15a5 (Embers column/tile, Purges pending 62), 15b (view, crawl both).
Waiting on owner: Q1 Money decoder in controlplane (wallet admin.rs parse_amount duplicates it) a/b; Q2 non-wallet red admindiff targets (players, index, player.wallet, events, mail) diagnose after 15; Q3 archcheck rule 11 EVENTS_ false positive (recommend narrowing to env reads); port 404 wallet splitproof checks (WL3-6, NT4, PH3/6/11, PY7) to panel_grant?
