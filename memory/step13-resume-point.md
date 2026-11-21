---
name: step13-resume-point
description: "Admin redesign resume point 2026-11-22: Step 15 (wallet) complete incl. 15c/15d; next Step 16 (inventory) after owner says go"
metadata:
  node_type: memory
  type: project
  originSessionId: 0af9025c-b620-41bb-9924-cba26bbf9324
  modified: 2026-11-21T23:20:17.113Z
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
eae03901 Events page address in controlplane (renamed EVENT_LOG_PAGE_MODULE/EVENT_LOG_PAGE in 139a2a9b, archcheck rule 11), 5eedd976 clippy fix, Nt dcfa7cb9+873426bf (splitproof CH0-CH4; break = remove peer_face("characters") in cmd/admin-svc/src/lib.rs, NOT main.rs with_peer), 14b clean crawls both topologies, plan bb903d85. Next: port 15 (wallet) — owner approved Step 14 only; ask before starting 15.

## State 2026-11-21 (Step 15 in progress)
Checklist docs/plans/2026-11-21-step15-wallet-checklist.md (§7 decisions). Done: 15a3 ledger panel 473062ec + accept 3b2fd5ec, 15t splitproof [LG1]/[LG1m] f74e2b75 (reviews ACCEPT), todo b0832045. Next: 15a4 (create currency, G13), 15a5 (Embers column/tile, Purges pending 62), 15b (view, crawl both).
Owner answered: Q1 done fccafdf8 (controlplane::decode_money, review PASS); Q2 leave; Q3 renamed constants 139a2a9b (archcheck OK); wallet splitproof 404 checks re-pointed only when the whole admin port is complete. Next: 15a4.

## State 2026-11-21 (Step 15 complete)
15a4 + 15a5 (515de79a, a7b922ec, 025f33b7), when_empty row restored 5e70dfe7 (when_empty = drawn in EVERY state plus empty, panel.rs:151), 15b accept files 129ea189 + 6f29ad32 + 3f8ed968 (proof-auditor fixes). Monolith + split crawls clean on all wallet targets; index red only on Mail queue (owner Q2: leave).
Owner open: purge-backlog helper in core/controlplane before Step 16 (copied per module); spent tile cache_ttl_s; tests.rs:888 vacuous EXCLUDED_MODALS count; date-slice in pending_purges. Next: ask owner before Step 16.

## State 2026-11-22 (15c/15d done)
Owner: 1 tak, 2 dodaj, 3 usun, 4 ok. 15c 28552db9 controlplane::purge_backlog_body (ports 16-24 call it; review ACCEPT). 15d da950074 spent tile cache 60s, vacuous count removed, characters Known limit (review ACCEPT). Next: Step 16 inventory — ask owner.
