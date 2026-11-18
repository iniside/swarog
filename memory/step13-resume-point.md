---
name: step13-resume-point
description: "Admin redesign Step 13 (accounts) handoff 2026-11-17 — part a landed (1bea67f8, 6f2de49f) UNREVIEWED; next = core-reviewer on part a, then opus test-author gate tests, then sonnet part b; owner said do only Step 13, not 14+"
metadata:
  node_type: memory
  type: project
  originSessionId: 6694aaef-849f-495c-bec1-e211bdc7a370
  modified: 2026-11-18T10:37:53.198Z
---

State at the end of session 2026-11-17. The owner stopped here to continue in a new session.

Step 13 progress:
- **Done:**
  - checklist `docs/plans/2026-11-17-step13-accounts-checklist.md`;
  - owner decisions G1–G15, recorded and reviewed to PASS (0de70e03, 46033634, 84872ce3);
  - part a, core-implementer opus:
    - 1bea67f8 (contract, fan-out engine `core/admin/src/engine/columns.rs`, templates, baselines);
    - 6f2de49f (accounts provider `modules/accounts/src/admin.rs`, admindiff `Route::FirstRow`, todo ADMIN2-21..34, plan errata "Step 13 (part a)").
- **Review of part a CLOSED (2 rounds, 2026-11-17):** fixes 183fedc6 (TableRow binds row entity else panel entity; EntityHeader.crumb G10; admindiff EMPTY fails crawl unless --allow-empty), d4bbc7f6 (SKILL loop close diffs FirstRow), plus stale-comment drop. Proof-auditor test guidance is in the round-1 audit (ask for it in the test-author prompt: fan-out via real route, affordance pair with writes:false, 404 by declared column id + call count 0, ColumnCells with cold cache, admindiff FirstRow/EMPTY crawl tests). Golden 'inline' mode cases must be DELETED, not re-blessed. Also tools/admindiff/src/tests.rs now fails to compile (allow_empty).
- **Accounts provider SQL tests (players_page, search, count, live_families):** owner 2026-11-17: separate step — already Step 25's per-module `modules/accounts` `admin_tests.rs` ([sonnet] test-author against the mail reference). Not in Step 13.
- **Tests done (2026-11-17):** 0dd57f9b + ed41431f (review-fix); workspace `cargo test` green on 0dd57f9b; proof audit 2 rounds. OPEN after round cap: P6 — SEEN_ENUMS (golden_tests.rs ~2227) is a hand list of 4 of ~23 `pub enum`s in controlplane; recommended fix = exact-partition test SEEN_ENUMS ∪ NOT_SEEN(name,reason) vs parsed PRIMITIVE_SOURCE, Align moved to SEEN. Owner OK'd a 3rd round → 9ce5eb84 (partition 14 SEEN / 9 NOT_SEEN, 65/65). Its audit: 3 items, nothing false-green today but future-variant wrong-reason passes: (1) ColumnCells case counted though render draws Empty; (2) Then/Emphasis counted from case.actions incl. disabled/unlisted row actions/untitled panels — collect from rendered tools; (3) NOT_SEEN reasons unexecuted — tripwire scan for InvokeOutcome/Point/ActionPoint/FilterKind in render.rs+templates; Role reason misleading (it IS drawn via role_name). Owner said fix → 97df9b4c (70/70). Final audit: items 1-2 PASS; item 3 leftovers L1 (VIEW_SOURCES doc falsely says render.rs builds every view model; routes.rs builds ActionItem from ActionPoint → drop ActionPoint from UNREAD_ENUMS + reword doc), L2 name-only scan misses Debug/serde reads, L3 `//` skip inside string literal, L4 Form/Confirm draw identically, L5 ColumnCells hand-listed exception; plus Point variants unguarded. Owner 2026-11-17: structural problem (test mirrors template rules) → added plan Step 28 (variant coverage observed from render, mirror removed). Stop patching the walker; L1–L5 are superseded by Step 28. Next: part b.
- **Next, in order:**
  3. `[opus]` test-author. Fix the tests that no longer compile:
     - `core/admin/src/golden_tests.rs`: 8 missing-field errors;
     - `tools/admindiff` `crawl_tests.rs`: a non-exhaustive match on `Route::FirstRow`.

     Then add the gate tests:
     - fan-out: ok, failing and denied columns, one call per page;
     - `has_write_affordance` ignores `implemented: false`;
     - the panel URL of a list column → 404;
     - `ColumnCells` from a page panel → error.
  4. Part b on sonnet mockup-implementer. Accept files `players.txt` and `player.txt` don't exist yet.
  5. The skill §7 live sequence, then the Step 13 post-mortem.
- **Open for the owner:**
  - G10 crumb: DECIDED by owner 2026-11-17 — add a contract `crumb` field (accounts sets it to the handle). Lands with the part-a review-fix round on core-implementer opus; record G10 in the checklist in that commit.
  - ADMIN2-18 (muted-mono cell): not closed, deferral recommended.
- **Post-mortem item:** this session's orchestrator wrongly blamed part a for a `devctl up monolith` started at 18:05 from a separate PowerShell. The agent says it was the owner's terminal. Part a waited about 55 minutes for it, and the orchestrator stopped that fleet. Lessons:
  - before stopping a running fleet, ask whose it is;
  - a pre-flight that finds a fleet up should report back, not wait silently.

**Why:** the owner said to do only Step 13, not 14 and later, and to continue in a new session.

**How to apply:**
- Run `git log --oneline` first ([[verify-repo-state-before-resuming]]).
- Resume at item 1.
- Don't start Step 14.

**Update 2026-11-17 ~20:06:** Part b done: no template change; accept files 8b5f4775 + review-fix 8cd11e8c (review PASS after prose fix). Live §7: monolith + split crawl clean, diff clean for index/events/players/player/mail. Search width → todo ADMIN2-35 (owner, low prio). Agent rules ported (c68277e4, 0160f0ed; UnrealPluginDev 0a2b863ae8cd, 28c9303b147d, eb62e023ba4e). Plan Step 28 added (4ac9146a). REMAINING: Step 13 post-mortem only. Candidate items: devctl wait must key on "running" not "N/N healthy" (I aborted a split start with devctl down); demoseed 404 on split (no ACCOUNTS_DEV_AUTH) — split crawl relied on monolith seed; admindiff blind to attributes/text (search width, D9 sort arrow, crumb/foot) and mock harness never runs P.players.meta.after (contributed th>span shape uncompared → Step 14 risk); fleet-ownership lesson; enum-guard spiral (done via Step 28 + agent rules).

**Step 13 CLOSED 2026-11-17 ~20:30:** post-mortem 2ed237a1 + review-fix 8a2507b0 (round 2 PASS). Owner questions open: (Q5) move devctl starting/running + "ask whose fleet" into gamebackend.md One test rollout at a time (MANDATORY) — recommended yes; extend skill Standing decision "fixed or accept entry" with "or a hand-check row for what admindiff cannot see" — recommended yes. Step 14 (characters) checklist research dispatched (general-purpose opus, read-only, returns draft) — owner asked to investigate the next step.

**Steps 13c/13d planning (2026-11-17 ~21:10):** research f1cb8d71 (docs/research/2026-11-17-ports-contract-gaps.md), plan 0597d0ad + review-fix a45700b9. Plan review cap (2 rounds) reached, round 2 REJECT. Waiting for owner: O1 (Step 28 before 13c), O2 (refs cap + label batch enforced once in LocalFace, anchor core/controlplane/src/lib.rs LocalFace impl ~:163-181), P1 refs bound design (rec: portal splits distinct refs per kind into cap-sized calls; LocalFace refuses over-cap as backstop), then local fixes P2 (EntityLabel splitproof → first port using it, Step 16/17), P3 (O2 anchor), P4 (core/admin/src/fakes.rs hash also dies), P5 (decline branches or re-cut after answers). Also open: Step 13 post-mortem Q5 fleet rule → gamebackend.md; skill standing-decision hand-check route. Step 14 checklist 31ccba77 waits for 13c/13d.

**Owner 2026-11-17 ~21:30:** O1 yes, O2 yes (P1 refs-split design folded into O2); plan re-cut in a9e4d883. "Popraw plan, ale na razie nie implementuj" — NO implementation of 28/13c/13d until the owner says so. Plan-review cap was reached before this re-cut; a9e4d883 is unreviewed (offer a fresh review if owner wants). Execution order when approved: Step 28 → 13c → 13d → 14.

**Refs seam (2026-11-18):** owner approved consolidating the refs/EntityLabel seam into one "Refs seam" block in Step 13c → 37de4621, 43ec41bf, 9a1c7861 (2 review rounds; round-2 REJECT items F1 time-observable deadline test + F2 one manifest per kind/admincheck cross-manifest check applied after the cap, unreviewed). Still NO implementation of 28/13c/13d. Open owner questions: Q5 fleet rule → gamebackend.md; skill hand-check route; X7 ordering vs purge-race-locks plan.
