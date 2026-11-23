---
name: step13-resume-point
description: "Admin redesign resume point 2026-11-23: Step 21a1 (apikeys reads) landed; owner decisions pending, then 21a2"
metadata:
  node_type: memory
  type: project
  originSessionId: 0af9025c-b620-41bb-9924-cba26bbf9324
  modified: 2026-11-23T16:35:04.414Z
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

## State 2026-11-22 (Step 18 complete)
18b b4eb2a5d (accept files, both crawls clean) + review-fix 5a3b7085; core-reviewer PASS, proof-auditor no defect. Split crawl needs `demoseed --base-url http://127.0.0.1:8082` (devctl up wipes the DB; the skill §7 line saying demoseed is monolith-only is stale). Next: Step 19 storage — ask the owner first.

## State 2026-11-22 (Step 19 nearly done)
Checklist docs/plans/2026-11-22-step19-storage-checklist.md (owner: all §6 as recommended). Landed: 19a1 9c406c7f, 19a2 c16ae73f, 19a3 e596a80d, 19t 360cb180 + 619dd0ec + review-fix b3daae06 (splitproof SA1-SA3 both topologies, break = remove peer_face("storage") cmd/admin-svc/src/lib.rs:38), 19b 8834bb39 accept files (monolith crawl+diff clean). All reviews ACCEPT. OPEN: split crawl fails "EXCLUDED_MODALS lists player.groups groups/membership.promote, which player.groups does not draw" (crawled player's group row requested/admin, data-dependent = 18b leftover) -> no split diff ran. Owner decisions asked: crawl row choice for groups, paired-mismatch lines uncounted in skeleton.rs (disabled menu-item line now hides inventory Grant), span.hint has no carrier (13e1 premise false), pending_purges day in session TZ in 7 modules, splitproof main-thread stack overflow (main.rs:701). Next after owner: close split for 19b, then ask before Step 20.

## State 2026-11-22 (Step 19 complete)
Owner chose option a: crawl fix 7e4462fd (tab follower picks a row that draws its EXCLUDED_MODALS claims; review ACCEPT). Split crawl clean, all 20 targets diff clean except index/mail (Q2). Owner: stop hardening admindiff ([[stop-polishing-admindiff]]). Open owner questions: pending_purges day in session TZ (7 modules), splitproof stack (main.rs:701), "now" vs "s ago", meta cost. Next: Step 20 notifications (20a2 needs the owner's Notify form fields) — ask first.

## State 2026-11-22 (post-19 owner answers)
Owner: 1 fix, 2 todo, 3 yes, 4 todo. Done: 70aac810 pending_purges UTC day (7 modules), ab74dacf controlplane::ago ("now" <60 s), todo 4a8a1c87 (splitproof stack, storage meta). core-reviewer PASS. Open to report: flaky notifications/groups timing tests; mockup "yesterday" vs "1 d ago". Next: Step 20 — still awaiting owner go + Notify fields.

## State 2026-11-23 (Step 20 complete)
Checklist docs/plans/2026-11-22-step20-notifications-checklist.md (owner: Notify fields title+message; rest as recommended). Landed: 20a1 17518ece, 20a2 7840756b (+ bless f11899c4), 20a3 232adec5, 20t b7f3cfc8 (splitproof NA1-NA3, 259 passed/44 expected) + review-fix edd0110a, 20b 4572f506 (accept files, both crawls clean). All reviews ACCEPT. Proof-auditor on 20b: STRUCTURAL — tab follower (crawl.rs:221-244) picks the newest player, demoseed seeds in parallel, so player.friends tr#dd6dc9 and player.inbox entries pin one seed and go STALE on reseed; lists_rows never matches the Inbox timeline (no tbody tr). Reported to owner (stop-polishing-admindiff applies). Next: Step 21 apikeys — ask owner.

## State 2026-11-23 (Step 21 started)
Checklist docs/plans/2026-11-23-step21-apikeys-checklist.md committed a95748c8 (owner: D1-D5 + all K items as recommended; K7: stop and report if prepare on head/tool fails). 21a1 (opus core-implementer, reads) dispatched; then 21a2, 21t, 21b.

## State 2026-11-23 (21a1 landed)
21a1 commits: ef55f591 store reads, d8f6c456 checklist fix, 4a0cbc14 admin.rs reads, 2ea885d8 tie test, 4bd7ba7b admindiff map (interim Absent rows for createKey/revokeKey — 21a2 deletes), 794fe9ff role filter (unknown role = zero rows), 5a7c81aa todo ids (APIKEYS-09 rotate, -10 expiry). Tests: apikeys 38, admindiff 237, clippy clean. Review of 794fe9ff: REJECT structural — core/admin/src/render/table.rs filter_views drops a current Select value not in options, template shows "Role: all" over the empty table (all Select filters). Asked owner: fix in core/admin (renderer + test) or todo; revoked-key FAKE "now" fix. Next: owner answers, then 21a2.

## State 2026-11-23 (21a1f done)
Owner: "1a, 2 tez popraw". ccf270e3 core/admin filter_views draws an undeclared Select value selected; dbaf8764 revoked key never "now"; review-fix 25a0fb4d (review ACCEPT). Open for owner: "any" = no-filter rule duplicated (apikeys filter_value admin.rs:100, core events_provider.rs:444, template :329) vs core filter_views empty-only -> ?role=any shows option "any" selected. Next: owner answer, then 21a2.

## State 2026-11-23 (21a2 part 1 done, part 2 running)
a9094615 ("any" = no filter in core filter_views) review REJECT structural S1: "any" decided in 6 places, wallet treats it as a value -> owner asked a) strip empty/"any" in the core/admin panel route (recommended), b) "any" means nothing, c) revert + todo. 21a2 part 1: 587af4de store D1 + review-fix 7e9ab020 (one-snapshot revoke, delete_role retries when count is 0) — review ACCEPT. Owner open: shared KeyRow column list (key() vs keys_page), set_key_role allows a revoked key. 21a2 part 2 (key.create/change-role/revoke + K17 probe) dispatched; then part 3 (roles), part 4 (P2-9, goldens, pages.rs), 21t, 21b.

## State 2026-11-23 (21a2 done)
3b67dd76 part 2 (key.create/change-role/revoke, probe), 0e34a361 part 3 (role.create/edit/delete, APIKEYS-01 closed), b9204689 test review-fix (refusal messages pinned), d682a01d part 4 (form foot no span.muted P2-9, pages.rs create-key + revoke modal rows, apirole/role/change-role exclusions, ADMIN2-61..63). All reviews ACCEPT. Owner open: S1 any a/b/c; Multi required:false (typed-only policy impossible from portal); canonical validate_policy (gateway vs admin read "full" differently, latent widen); admindiff test fixtures restate prod rules (claim_inventory built from PAGE_MAP) — stop-polishing applies. Next: 21t (test-author opus, splitproof D5, fleet runs — say count/time) then 21b (mockup-implementer sonnet, crawl + accept files).


## State 2026-11-23 (post-21a2 fixes)
- F1 8f8f67b1 (route drops empty+"any" filter params): review REJECT — (1) local: wallet admin.rs:193 + inventory admin.rs:121 still drop empty themselves, wallet admin_tests.rs:581 empty_select pins it → remove; (2) structural, ASKED OWNER: "any" collides with operator values (wallet currency/inventory kind "any"); recommendation = route drops only empty, no "any" sentinel. Fix both in one review-fix after owner answers.
- F2 35fc40e0 canonical validate_policy + Multi required:false — ACCEPT. Note: old non-canonical rows in dev DBs → DROP SCHEMA apikeys + fresh boot if it matters.
- F3 ed050bae revoked re-role guard + shared KEY_COLUMNS/ROLE_COUNTS; review-fix cf17e7b6 (same-role revoked test) — ACCEPT.
- Next: F1 review-fix (owner decision), then 21t (announce run count/time), then 21b.
