---
name: step13-resume-point
description: "Admin redesign resume point 2026-11-24: Step 24 audit — a1/a2 done, 24t splitproof running; then 24.a3, 24.t, 24.a4, 24.b"
metadata:
  node_type: memory
  type: project
  originSessionId: 0af9025c-b620-41bb-9924-cba26bbf9324
  modified: 2026-11-24T13:17:45.108Z
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
- F1 review-fix 34e284de (route drops ONLY empty; FilterView.value Option; wallet/inventory copies removed) — ACCEPT (owner: "ok niech odrzuca").
- 21t DONE: b5019978 + review-fix 2f6e00b1 (cleanup on every exit, AD3b searched row, AD4 csrf body, AD6g outcome pairs). Splitproof green both topologies (279 pass / 35 expected-red). core-reviewer ACCEPT, proof-auditor sound. Comment nits left: apikeys_admin_checks.rs:39-52 hop/break-run wording, :41 long line; view_tests.rs:99 "filter at any".
- Owner question pending: role-delete 409 shows generic portal conflict banner (core/admin/src/routes.rs:1297/1326) not the provider message — bug?
- Next: 21b (mockup-implementer sonnet: crawls monolith+split, accept files) — ask owner go.

## State 2026-11-23 (Step 21 complete)
- 21b: 91013217 accept files (4 apikeys targets + #ecbb9a nav line re-derived on 13 pages), review-fixes e7013d98 + ef313be8 (core-reviewer round 2 last finding = sample-key count, fixed and checked by hand; review cap reached).
- All 4 apikeys targets clean monolith (crawl-1795457986) + split (crawl-1795458889). Reds left, owner Q2 leave: index Mail queue tile, mail empty Outbox, player.friends tr#dd6dc9 (monolith only), player.inbox li#8e97b6, events split-only lag/Max lag (seed data).
- Owner questions still open: (1) role-delete 409 generic banner routes.rs:1297/1326; (2) filter chip "any" for both real value and no filter; (3) comment nits (apikeys_admin_checks.rs:39-52,:41; view_tests.rs:99).
- Accept nit, pre-existing: #ecbb9a reason files "Events" under Lore (registry.js:20-24 says Lore = Permissions, Server instance, Billing).
- Next: Step 22 (scheduler) — ask owner before starting.

## State 2026-11-23 (Step 22 complete)
- Checklist docs/plans/2026-11-23-step22-scheduler-checklist.md (§11 owner: S1-S13 as recommended), plan e2743dd5.
- 22a1 37b962b6 + F1 a2aed472 (near-ceiling interval renders "—", TIMESTAMP_MAX_EPOCH_SECONDS bound in store.rs SCHEDULES_SQL); 22a2 2bc920a3 (attention.runs moved to scheduler provider) + baseline 6f533ce4 + regenerated 1fef6de8 (public-api baseline was missing controlplane::ago since step 19 — gate is advisory/opt-in, drift went unseen); 22t 3a45e0ac splitproof SH0-SH3; 22b 1005629d accept files (scheduler.txt + nav #ecbb9a re-count in 14 files).
- Crawls: monolith target/admindiff/crawl-1795466014, split crawl-1795466435 — scheduler clean both. Reds left = pre-existing seed/timing (owner: leave).
- Open for owner: public-api gate opt-in (run --bless-public-api per step touching core/controlplane/api?), SH2 refusal told apart only by letter case "Not implemented yet" (scheduler admin.rs:408, same idiom storage_admin_checks.rs:135), Recent fires cap 100 unnoted, checklist template should list public-api baseline.
- Next: Step 23 (config) — ask owner before starting.

## State 2026-11-24 (Step 23 in progress — owner: stop after 23a2)
- Checklist docs/plans/2026-11-23-step23-config-checklist.md (D1-D7, C1-C9 as recommended). Landed: 23s a425c936 + d3de6666 (per-key revision, lock order), 23r0 6856ea04 (FormSpec reason) + 9e8577eb (_reason only on forms with a note), 23a1 b0e8e377 + review-fix 0007e9e5 (timeline "rev N by" via summary+badge) + todo 2466be7f/e6e0328a. All reviews ACCEPT.
- Owner: 23t splitproof DROPPED (9ded4d66; split writes a known limit in docs/todo/config.md). Value field not required for String; multi-key writes lock in (namespace,key) order.
- 23a2 (config edit/create invoke, conformance probe, EXCLUDED_MODALS) dispatched 2026-11-24; owner said stop after it — do NOT start reviews/23b without the owner.
- Owner open: Confirm still accepts _reason on a None ConfirmSpec (actions.rs:389) — recommend same rule as forms; 8 admindiff PAGE_MAP failures from redesign mockup commits 4fa3b2e0/0390ec07/b4303e23 (todo admin.md).
- Next: core-reviewer + proof-auditor on 23a2 (conformance touched), then 23b (accept files, nav #ecbb9a re-count) — ask owner.

## State 2026-11-24 (Step 23 complete)
- 23a2 aaf1ab3b + review-fix f58d4b2d (core-reviewer + proof-auditor ACCEPT); todo bccd2cd6 (replay safety, duplicate-create revision gap, no redirect after create → config.md; Confirm _reason leak → admin.md).
- fdc97507 declares 5 new mockup screens Pending in admindiff; crawl still blocked (character/group row menus on compared player tabs). Owner: leave admindiff entirely — 23b dropped, admindiff out of the plan (b6219934 errata).
- Owner: "konczymy ten plan" — finish the plan. Next: Step 24 (audit) checklist; 24.a4 lane is an owner decision.

## State 2026-11-24 (Step 24 in progress)
- Checklist docs/plans/2026-11-24-step24-audit-checklist.md (owner: A1-A15 as recommended, 24t kept, a1 split a1a/a1b, 24.a4 = code-scout then sonnet).
- Landed: 24.a1a 05037093, 24.a1b 9398d2c4, 24.a2 ebbaa19f, review-fixes 195ff826 (round 2 PASS) + c16b37cb; todo fixes 122cce79 (config heading), f7395f93 (groups purge test fails on foreign tombstones in the shared DB).
- cargo test --workspace: all green except admindiff (out of plan) and groups one_sweep_fire_stamps_at_most_64_players (todo).
- Owner open (structural): outcome-prefix constants in controlplane::events (+ does conflict: count as failure), drawn_orders test not in admincheck --strict, no core test for empty-Timeline hide, operator login "system" collides with system actor.
- 24t (test-author opus, splitproof AU3 re-point + X5 + dashboard + break run) dispatched; then proof-auditor, 24.a3, 24.t, 24.a4, 24.b.

## State 2026-11-24 (Step 24 code complete)
- 24t 99463190 (+ doc 606dca1a), 24.a3 c9a9f128 (reverse peer finding; review PASS), 24.t 9f691d5a + review-fix 9ffe4b6d (proof-auditor gaps B/C), 24.a4 1287dee6 (no dead_code was left; fixed 3 clippy errors; review PASS). Workspace clippy -D warnings exit 0.
- 24.b: no template change unless the owner's live look (H3/H13) shows one; not run.
- Owner open (new): admincheck self-dial (admin-svc dialling "admin") passes silently; duplicate dials vanish in the BTreeSet; audit error-block branch unproven; export has no splitproof.
- Next: report Step 24 to the owner, then Step 24a/25 on the owner's go.

## State 2026-11-24 (Step 24a done, Step 25 started)
- Step 24 todo 1b999b4b (24.b closed, no commit). 24a 8ac46dab (VerbError: NotFound/Retired/Healthy/NothingToSkip/Db; state_of pre-read gone); core-reviewer ACCEPT.
- Owner open (24a review): S1 VerbError Display carries eventctl wording ("eventctl: no such subscription" now shown in portal); L1 prepare_skip NotFound says "no subscription" (second text); L3 poison_event mixes "asyncevents:"/"eventctl:" prefixes. L1/L3 depend on S1.
- 25a (migrate race + release undecodable claimed row) dispatched to core-implementer opus. Next: 25b (pageshow, mockup-implementer), 25c1-3, 25d1-4, 25e (owner lane decision before 25e2), 25f, 25g, 25h (owner approves per-group assertion lines before 25h1).
- Owner 2026-11-24 ("ten split proof to zostaw ... nie duplikuj"): Step 25h DROPPED (c73b119e, groups in docs/todo/splitproof.md); backfill adds only uncovered items, never duplicates. Owner also impatient with Step 25 length — keep it moving.
- Landed: 25a c2383d34 + a39c0962, 25b 0e679031 + c5cb42c4 (all ACCEPT), 25c3 1e9636d5 (review pending). 25c1/25c2 running.


## 2026-11-24 later: Step 25/26 state
- 25a c2383d34+a39c0962; 25b 0e679031+c5cb42c4; 25c 9a16b3dc+4314c825, 9479d4fb, 1e9636d5; 25d1 0ad28c24; 25d2 7bbcda4f+c3311d72; 25d3 95d7f638+6c944209; 25d4 e39f6ff1, 7ae5022b (ACCEPT); 25e1 55a81cb2; 25f1 b4eee17e+eed9ecf5; 25f2 deaf5bb0+8a8585f4; 25g1 38a51fdf+48ad61fc; 25g2 13a3201a+295a41dd; 25h DROPPED (c73b119e). Step 26 done (471666ea, 348cb795, f018531f, memory).
- Remaining: 25e2 (owner lane sonnet/opus pending; recommended opus), Step 27 (verifyctl --all --strict + owner browser check of 25b; admindiff skipped). Workspace suite not re-run since Step 24.
- Owner questions open: 24a S1 VerbError eventctl wording; 25f1 mail counters need admin.rs pure fns; 25g2 typed Finding enum; 25g1 conformance zero-writer floor; panels cache is fallback-only (plan text implied cache hit skips provider).
