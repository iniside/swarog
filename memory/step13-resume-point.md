---
name: step13-resume-point
description: "Admin redesign resume point 2026-11-22: Step 18 through 18t landed+reviewed; next 18b (crawls + accept files)"
metadata:
  node_type: memory
  type: project
  originSessionId: 0af9025c-b620-41bb-9924-cba26bbf9324
  modified: 2026-11-22T18:09:27.699Z
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

## State 2026-11-22 (Step 16 in progress)
Checklist docs/plans/2026-11-22-step16-inventory-checklist.md §8 decisions (G1 b owner table, G4 a disabled Revoke, G5 disabled Grant, G3 "—"), plan parts 16a1/16a2/16b (ca685c8d). 16a1 ccf29df3: inventory.character_owners + sub inventory.character-owner.v1 (Genesis), reader character_ids_of; core-reviewer NOT run yet. Clippy red on unused store methods until 16a2. Owner said stop after 16a1; next: core-reviewer on ccf29df3, then 16a2 when owner says go.

## State 2026-11-22 (Step 16a complete)
16a0 c88143c0 (EntityTab params), 16a1b 9e21e950 (character.created KeepForever; local PG history_contracts row deleted), 16a2 b5fff581 (Inventory tab, disabled grant/revoke, INVENTORY-05). core-reviewer ACCEPT on all; proof-auditor on 16a2: local test gaps (search id/name vacuous, panel paging >25 never run, keyset char->player boundary untested) + structural: 16b split crawl accepting "data difference" would hide missing character rows -> need a never-accepted check / splitproof for a character-owned row. Reported to owner; next: owner decisions, then test fixes + 16b (wipe DB first).

## State 2026-11-22 (Step 16 complete)
16a2f + 16t (681fdb75 unit tests, a2510cf3 splitproof IV1-IV4, 78972735), 16b 722fff78 accept files + e9d5cc6e wording fix. Monolith + split crawls clean on all 10 player targets; index/mail/events red as before (Q2). Proof-auditor on 16b: admindiff merges identical lines (a set, tools/admindiff/src/skeleton.rs Lines BTreeSet), so dropping the Owner/Type select, the Characters tab or every row of one owner kind stays green. Structural, so it goes to the owner: put counts in the lines. Local option: assert select[name=owner]/[name=type] in a test. Next: report to the owner, ask before Step 17 (friends).

## State 2026-11-22 (Step 18 through 18t)
Done since Step 16: 16c-16h, Step 17 (friends), 13f, 13f2, mail dead-code fix. Step 18 (groups, checklist docs/plans/2026-11-22-step18-groups-checklist.md): 18a1 0cebe481, f10e0f22, 7941f936; 18a2 1a049939 + review-fix 9b1d216e; 18t splitproof GA0-GA4 5e2d6e21 + review-fix 4ea08a6c (247 passed, 44 expected). Next: 18b [sonnet] mockup-implementer — monolith + split crawls, checklist §7 accept files, §12 admindiff rows (drop player.groups from EXCLUDED_TABS, PAGE_MAP tab, EXCLUDED_MODALS row for groups/membership.promote). Crawls need a fleet: send cargo START/DONE to swarog-8a. Port 8190 can be held by the owner's flux2 Python server — check before splitproof/fleet.
