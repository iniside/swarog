# Admin redesign plan — step-size audit (2026-11-18)

Audits `docs/plans/2026-10-22-1759-admin-control-plane-redesign-plan.md` at 855ec505.

**Re-cut proposal for the unlanded steps of the admin-control-plane-redesign plan**

Read-only audit of `docs/plans/2026-10-22-1759-admin-control-plane-redesign-plan.md` against HEAD 855ec505. Today's two steps put this codebase at about 30 calls per file: 13c.1a touched ~14 files in 432 calls. The rule's "60 calls ≈ 8 files" assumes about 7.5 calls per file. So for `core/controlplane` + `core/admin` contract work (re-blessing goldens, public-api baseline), the realistic cap is **one mechanism over about 4–5 files**, which is what the cuts below aim for.

**Fix this before using the new ids.** `.claude/hooks/commit_format.py:16` is `STEP_ID = \d+[a-z]?\d*(?:\.\d+[a-z]?)?`. It accepts `15a1`, `25d3`, `13d2` and `13c.1h`, but rejects `13c.2a1` (it allows no digit after the dotted letter). There are two options:
- **Recommended:** widen the hook to `(?:\.\d+[a-z]?\d*)?`, owner-approved. The ids below assume this.
- **Fallback:** use `13c.2a`, `13c.2c`, `13c.2d` for the Refs seam and `13c.2e`, `13c.2f` for today's 13c.2b.

**Id collision.** Port 24's parts `24a`/`24b` collide with the existing Steps 24a (typed errors) and 24b (demoseed). The port-24 parts below are named `24.a1`…, and the hook accepts that form.

## 1. Audit table

| Step | Mechanisms / decisions | Files (real tree) | Est. calls | Verdict |
|---|---|---|---|---|
| 13c.1g | 1 (goldens) but ~20 cases: 15 fields + 5 variants | `golden_tests/cases.rs`, `build.rs`, ~20 testdata files, bless | 120–150 | TOO BIG |
| 13c.2a | 5: chunked fan-out, MAX_PANEL_REFS + LocalFace cap, label pass, owner-only lookup in admincheck, accounts producer | controlplane `lib.rs`, `engine/columns.rs`, new `engine/labels.rs`, `routes.rs`, `render/cell.rs`, `render/panel.rs`, `engine/panels.rs`, admincheck `checks.rs`, conformance `policy.rs`, accounts `admin.rs`/`store.rs`/new `admin_tests.rs`, `tests.rs`, public-api baseline | 200+ | TOO BIG |
| 13c.2b | 3: K17 destination, K20 params, FilterKind::Select | controlplane `lib.rs`, `engine/search.rs`, `render/shell.rs:240`, `routes.rs:688`, `render/table.rs:76`, `primitives.html.tmpl:198`, mail/accounts/events_provider search, `tests.rs` (3 fakes), baseline | 100–130 | TOO BIG |
| 13c.3 | 3 independent gate items | `cases.rs`, `checks_tests.rs`, `tools/splitproof/src/main.rs` | 50–70 | OK, but split for pipelining (below) |
| 13c.4 | prose | 2 files | 20 | OK |
| 13d | 7: T1 tabs, T2 modals, T3 menus, T4 force, T5 option collapse, T6 prelude + portal param, G6; plus tests | `tools/admindiff/src/{pages,crawl,mock,skeleton,lib,accept}.rs`, `accept/*`, SKILL.md, `core/admin` (T6 route param, unlisted) | 200+ | TOO BIG |
| Port template part a | 6–9: provider skeleton, panels, store readers, actions + probe + CapCase, request_id, X1 attention, tiles/columns, pages.rs, todo | 7–11 per port | 100–250 (accounts' `admin.rs` is 749 lines, mail's 603) | TOO BIG for every writer; small read-only ports borderline |
| 14 characters | tab, fact, count, list column, EntityLabel{character}, X1 | ~8 | 110 | TOO BIG |
| 15 wallet | tab, grant/revoke + idempotency, Economy page, create-currency, column, tile, X1, splitproof label | ~11 | 250+ | TOO BIG |
| 16 inventory | tab, unimplemented grant item, menu group, X1 | ~6 | 60 | OK as one a |
| 17 friends | tab, pending meta, X1, uses EntityLabel{player} | ~6 | 55 | OK as one a |
| 18 groups | tab via `page_mine`, promote (REAL) + restored probe, X1 | ~8 | 110 | TOO BIG |
| 19 storage | tab (`writes:false`), tile scan, FAKE checksum/quota, X1 | ~6 | 65 | OK as one a |
| 20 notifications | inbox tab, Notify header action + row action, probe, X1 | ~8 | 110 | TOO BIG |
| 21 apikeys | page, Roles panel, search + kind, create/reveal/revoke/role, probe | ~9 | 140 | TOO BIG |
| 22 scheduler | page (writes disabled), Next fire, storage.gc fake moves from `core/admin` | ~7 (incl. `core/admin/src/fakes.rs`, `portal_provider.rs`) | 65 | OK as one a |
| 23 config | page, revisions timeline, K20 selection + T6 prelude, CAS edit form, probe | ~9 | 150 | TOO BIG |
| 24 audit | page, dashboard panel, admincheck peer finding, crate-wide dead-code sweep, X9 accept sweep | 15+ | 200+ | TOO BIG |
| 24a | 1 typed error | 3 | 45 | OK |
| 24b | 1 mechanism × ~12 modules, plus accept pruning | `tools/demoseed/src/lib.rs` (691 lines), `tests.rs`, `accept/*` | 150+ | TOO BIG, and under-specified |
| 25a | 2 fixes | 2 | 40 | OK |
| 25b | 1 | 1 | 10 | OK |
| 25c | 4 crates' backfill | 4 crates (asyncevents needs `--test-threads=1`) | 120 | TOO BIG |
| 25d | ~35 claim/form tests | `core/admin/src/tests.rs` (1864 lines) | 250+ | TOO BIG |
| 25e | engine list + auth + adminctl + weles | 4 crates | 100 | TOO BIG |
| 25f | 2 providers | mail `admin_tests.rs` (exists, 42 lines), accounts (new) | 90 | TOO BIG (split, parallel) |
| 25g | conformance + admincheck fixtures (~20) | `tools/conformance`, `checks_tests.rs` (186 lines) | 100 | TOO BIG (split, parallel) |
| 25h | ~16 groups rewritten + 4 helpers + ~10 additions in a 7479-line file | `main.rs`, `party_checks.rs` | 300+ | TOO BIG |
| 26 | ~15 doc targets in 4 kinds | ~30 files | 100 | TOO BIG (split, parallel) |
| 27 | orchestration | none | n/a | OK (inline) |

## 2. Re-cuts

### 13c.1g goldens (test-author [opus]; sequential, both edit `cases.rs`/`build.rs`)
- **13c.1g — the 13c.1a surface.**
  - Cases for `Column.wrap`, `Cell.row`, `a.mono`, the singular noun, `MonoMuted`, `Toggle`, `Strong`, `Tone::Accent`.
  - Reuse `table_noun_*` (`cases.rs:377-383`) where it already draws the singular.
  - Bar: reverting the field's draw changes the golden.
- **13c.1h — the 13c.1b surface.**
  - Cases for `PanelBody.foot`/`tag`, a `ContributionDecl.readonly` text, the tab tiles row, `Layout::Rows`, `TimelineEntry.title/badges/actor`, `days` and `date_only`, `Field.mono`/`placeholder`, `FormSpec.title`, `SecretReveal.extra`, and `kpi_tiles`.
  - `timeline_two_days_older` (`cases.rs:406`) already sets `days: true`; add only a `days:false` case and a `date_only` case.
  - Ends the red window, including the two `Primitive` case-count checks listed in the 13c.1b errata.
  - Depends on 13c.1g.

### 13c.2a Refs seam (all [opus] core-implementer, in this order)
- **13c.2a1 — chunked column fan-out and the cap.**
  - Scope: `MAX_PANEL_REFS`, chunking in `fan_out`, one deadline shared by a column's chunks, all-or-nothing `Failed`.
  - Cap check in `LocalFace::panel` (`core/controlplane/src/lib.rs:261`, beside the `write_refusal` check at :281) returning `invalid`.
  - Files: controlplane `lib.rs`, `engine/columns.rs:36`, `core/admin/src/tests.rs` (beside the fan-out test at :1393), `tools/conformance/src/policy.rs:192`, baseline.
  - Tests: distinct refs per kind; more refs than the cap → chunks all run and fill every cell; shared deadline on a paused clock (0.6× passes, 1.1× fails); one failed chunk fails the whole column; over-cap refused before the provider runs (monolith face).
  - Decision to add: `req.refs` gets its own rationale constant naming the bound. Today it shares `CONTROL_PLANE_READ_RATIONALE`, which cannot name it.
- **13c.2a2 — the label pass.**
  - Scope: `Point::EntityLabel{kind}`; one pass per slot response after its body and columns; refs read from the body only; only the owning manifest's contribution is called; chunked like columns; failure draws the plain id with no banner.
  - Files: controlplane `lib.rs` (Point plus a named placement test), new `engine/labels.rs`, `engine/mod.rs`, `engine/panels.rs:106` (not a slot), `routes.rs`, `render/cell.rs:88` (`ref_cell` takes the label), `render/panel.rs` (`Ctx`), admincheck `checks.rs:69` (`point_kind` arm), `tests.rs`.
  - Tests: owner-only (a second module's label gets zero calls); a row carrying two refs of one kind; fail, timeout, denied and missing each draw the id; labels never read from another slot; Point placement test.
  - This step must decide what a label answers:
    - **Recommended:** `Primitive::ColumnCells` with one text cell per ref. That adds no new Primitive variant, so no Step 28 golden is needed.
    - It must also list which `EntityRef` carriers get labels: `Row.entity` (`lib.rs:987`) and the timeline `target` (`lib.rs:1113`) at least, plus the K10 actor `Cell`.
  - Depends on 2a1.
- **13c.2a3 — kind ownership and the first producer.**
  - Admincheck flags a kind declared by two manifests, next to `unique_ids`. The plan's anchor `checks.rs:57` is stale: it is :44, with the entity-kind line at :59.
  - Accounts `EntityLabel{player}` gives handles by id and leaves missing players out.
  - Files: `tools/admincheck/src/checks.rs`, `modules/accounts/src/{admin.rs,store.rs,lib.rs}`, new `modules/accounts/src/admin_tests.rs` (25f2 later extends it).
  - Tests: accounts answers the handle per id and omits a missing player. The admincheck fixture stays 13c.3's.
  - Depends on 2a2.

### 13c.2b (both [opus] core-implementer)
- **13c.2b1 — K17 one destination.**
  - `SearchHit` carries an entity or a `Link`.
  - Files: controlplane `lib.rs:417`, `engine/search.rs`, `render/shell.rs:240`, `routes.rs`, mail `admin.rs:567`, accounts `admin.rs:706-720`, `events_provider.rs:752`, `tests.rs` (the fakes at :973, :1368, :1631), baseline.
  - Test: a hit reaches its one destination, entity and page variant each.
  - Independent of 13c.2a by files, except controlplane `lib.rs` and the baseline, so run it after 2a3.
- **13c.2b2 — K20 page params and FilterKind::Select.**
  - `PageDecl.params` (`lib.rs:541`) whitelists what `routes.rs:688` forwards; today `params: query` forwards everything.
  - Select draws in the toolbar (`render/table.rs:76`, `primitives.html.tmpl:198`).
  - Tests: only declared keys reach `PanelQuery.params`; an undeclared key is dropped.
  - New red window: `FilterKind::Select` is a drawn variant, so the Step 28 gate is red from 2b2 until 13c.3a. The plan only declares 13c.1a–13c.1g; add this window.

### 13c.3 (test-author [opus]; split so each part follows the code it gates)
- **13c.3a:** Select golden (after 2b2) and the admincheck duplicate-kind fixture (after 2a3). Files: `cases.rs`, `checks_tests.rs`.
- **13c.3b:** splitproof over-cap `cp.panel` refused by the remote `LocalFace` (after 2a1).
  - Open decision: splitproof talks HTTP today and has no direct `cp.*` edge client, so this needs a new harness (mTLS edge dial) or an HTTP route that can send more refs than the cap. The plan must pick one.

### 13d admindiff (core-implementer [opus]; sequential, all touch `crawl.rs`/`mock.rs`)
- **13d1 — tab targets and reachability.**
  - T1 (`?tab=` on a `FirstRow` page, own accept file), a crawl that fails on an unreachable target, and T5 `option` collapse.
  - Files: `pages.rs`, `crawl.rs`, `mock.rs`, `lib.rs`, `skeleton.rs`, plus the G6 shared accept line.
- **13d2 — T3 menus diffed.** The diffed menu body replaces the status-only check at `crawl.rs:210-223`. Files: `crawl.rs`, `mock.rs`, `lib.rs`.
- **13d3 — T2 modals.**
  - Files: `pages.rs`, `crawl.rs`, `mock.rs`, `accept.rs` (new accept sections).
  - Open decision: the harness throws a sentinel at `innerHTML`, so `after` and openers never run (`mock.rs:22-23`). How a JS opener runs is undecided.
- **13d4 — T4 force ok state.** Files: `pages.rs`, `mock.rs`.
  - Recommendation: move T6 (prelude plus a portal route parameter) into Step 23a1. Config is its only user, and it touches `core/admin`, which 13d's (a) does not list.
- **13d5 — test-author** for every target kind in `crawl_tests.rs`, `mock_tests.rs`, `skeleton_tests.rs`, `pages_tests.rs`. After 13d4; proof-auditor audits.
- **13d6 — [docs]** skill §1/§6/§7.

### Port template (applies to every port; replaces "part a")
- **Before a** (unchanged checklist). Its §7 is split into contract gaps and data decisions.
- **Na0 [opus], only if the checklist finds a contract gap.** Core contract plus render; tests in the step; `Nt0` test-author for its golden. Never folded into Na1.
- **Na1 [opus] — read surface.**
  - Provider skeleton with `writes: false`: pages, tabs, panels, search and kinds at the X2 orders; store readers (X4 reuse); slot in `lib.rs`; `Cargo.toml`; `pages.rs` route or tab entry; FAKE todo items.
  - Tests: `admin_tests.rs` panel data mapping. Target ≤7 files, about 60 calls.
- **Na2 [opus] — writes (writers only).**
  - Actions and `invoke`, `writes: true`, `request_id` → the idempotency key (X8) or a Leftover, `implemented:false` items with todo entries.
  - Probe in `modules/<m>/src/conformance.rs` plus CapCase rows in `tools/conformance/src/policy.rs`.
  - Tests: success and failure of each REAL action.
- **Na3 [opus] — contributions outside the module's own page or tab.**
  - X1 Purges-pending, dashboard tile, players list column, tab count, EntityLabel, nav counter. Each is one reader and one contribution.
  - Tests: each reader's data mapping, and Purges-pending shown only while the sweep is behind.
  - Folded into Na1 for ports estimated under 60 calls.
- **Nt** (test-author) and **Nb** (view) are unchanged.
- Review runs once per a-part, on that part's cumulative diff.
- Ports run in sequence, never in parallel:
  - they share `pages.rs`, `policy.rs`, `admin-second-pass.md` and `cases.rs`;
  - only one test rollout runs at a time;
  - X7: the owner's ordering against `2026-11-17-1900-purge-race-locks-plan.md` is still undecided in the plan, and Step 14 needs it.

### Port-specific cuts (exceptions to the template)
- **14:**
  - 14a1 (tab, fact, count; checklist G2/G3/G7/G8/G10/G11);
  - 14a3 (list column at 10, `EntityLabel{character}`, Purges-pending at 61).
  - No a2: characters is not a writer.
- **15 (largest):**
  - 15a1 wallet tab and its count;
  - 15a2 grant/revoke with `request_id` → `idempotency_key`, probe, ADMIN2-40 accept entry;
  - 15a3 Economy page reads: currencies, FAKE global ledger with the player EntityLabel consumer, spent-24h scan, `pages.rs` economy mapped;
  - 15a4 create-currency (Q1, D6 placement);
  - 15a5 Embers column, tile, Purges-pending at 62;
  - 15t splitproof label assertion after 15a3.
  - Open decision: which currency is "Embers" (research P list).
- **16, 17, 19, 22:** one a each (Na1+Na3). 22 also edits `core/admin/src/fakes.rs` and `portal_provider.rs` (the storage.gc move); say so in its file list.
- **18:**
  - 18a1 (tab via `page_mine`, group link without an entity, Purges-pending at 65);
  - 18a2 (promote-to-admin plus the restored probe).
  - Missing anchor: name the commit the probe is restored from, as mail's `b8435c9a^`.
- **20:**
  - 20a1 (inbox tab, Purges-pending at 67);
  - 20a2 (Notify and Send actions, probe).
  - Open decision: the Notify form shape ("undesigned").
- **21:**
  - 21a1 (page, Roles panel, search provider `apikey`, kind);
  - 21a2 (create with reveal using the header read from `modules/gateway`, revoke, change role, probe, create-retry Leftover).
- **23:**
  - 23a1 (page reads, FAKE revisions timeline, K20 key selection, plus T6 moved from 13d);
  - 23a2 (PanelForm edit, CAS `expected_revision`, value cap, change note, probe).
  - Open decision: research P says "per-key revisions NEW". Whether that is new storage is unspecified; if it is, it needs its own step.
- **24:**
  - 24.a1 audit page: per-topic actor/action/target, Select actor filter, X5 operator cells;
  - 24.a2 dashboard "Recent operator activity" panel and tone rule;
  - 24.a3 admincheck finding for a peer with no provider (fixture by test-author 24.t);
  - 24.a4 crate-wide dead-code sweep to clippy-clean plus the X9 accept-line sweep. Core-implementer, `sonnet`, mechanical, but it needs the "no port reused" list. Make that a scout inventory first.

### 24b demoseed
- **24b0** code-scout [opus]: a table, per module, of the public-API creatable data and the accept entries each would resolve. Owner approves it before 24b1. Today's text leaves this choice to the implementer.
- **24b1:** accounts, characters, inventory, wallet.
- **24b2:** friends, groups, party, notifications.
- **24b3:** storage, apikeys, config, mail. Each step has tests in `tools/demoseed/src/tests.rs`.
- **24b4** [inline]: fleet crawl/diff, then delete the resolved `data difference` entries.
- 24b1–24b3 share `lib.rs`, so they run in sequence.

### 25 (all core-implementer [opus] unless tagged)
- **25c:**
  - 25c1 `core/controlplane` (routing, sections, Forbidden, encode/decode table);
  - 25c2 `controlplane-rpc` loopback plus `core/app` `cp.manifests`;
  - 25c3 `asyncevents` `admin_tests.rs` (run alone, `--test-threads=1`).
  - The three are file-disjoint and could be authored in parallel, but their test runs must be serialized.
- **25d** (all in `core/admin/src/tests.rs`, sequential):
  - 25d1 `classify_form`/`classify_click` unit tables;
  - 25d2 claim and click-claim concurrency and windows (Notify, back-dated rows), late release, generation and `request_id`;
  - 25d3 settle, 422/409 re-renders, toasts, `HX-Retarget`, pre-invoke releases;
  - 25d4 POST decoding (Money, Multi, blanks, blank-rule order, unknown key), `validate_item`, `__` key and mint failure, search cache key, `NoOp` flash.
- **25e:** 25e1 the `core/admin` engine list (including the 9a/9b/10a items); 25e2 auth suite role, `tools/adminctl`, weles `fleet_toml_tests` ([sonnet] is enough).
- **25f:** 25f1 mail (extends the existing `admin_tests.rs`) and 25f2 accounts (extends 13c.2a3's file). Parallel-safe by files.
- **25g** (test-author): 25g1 `tools/conformance` (input inventory plus writer check) and 25g2 admincheck fixtures (9a, 8a, every finding kind). Parallel-safe by files.
- **25h** (test-author, one fleet run each, sequential in `main.rs`):
  - 25h1 portal core: AD0–AD6g, ADX1–ADX3b, AU3, `admin_csrf`, plus the killed-provider and viewer-denied additions;
  - 25h2 monolith parity: M3/M3b, WL6m, NT4m, ML3–ML4b, `mail_form`;
  - 25h3 cross-process money/inbox: WL3–WL6, NT2*, NT4, PH3/PH6/PH11, PY7 (now at `party_checks.rs:282-336`, not the plan's :291-293), `wallet_form`, `inbox_form`;
  - 25h4 FR8, GR5, config CAS edit, one real invoke per writer.
  - The plan never says what each "rewrite" asserts after the redesign. For example, ML4b drives `requeue-all-parked`, which G11 dropped. Each sub-step needs a group → new-assertion line before dispatch.

### 26 (docs-writer [sonnet])
- **26a:** `.agents`/`.claude/rules`/skills.
- **26b:** `docs/reference`, `roadmap`, todo moves, research errata.
- **26c:** dead-crate prose in `api/`, `core/`, `cmd/`, `modules/`, `tools/` comments.
- **26d:** memory plus the `memory-backup` push.
- 26a–26c are file-disjoint, parallel-safe, and run no cargo. 26d goes last.

## 3. Decisions left to implementers (cost calls today)
- **13c.2a:** what a label answers, which carriers get labelled, and the `req.refs` rationale constant.
- **13c.2b2:** the Select red window is undeclared.
- **13c.3:** how splitproof sends a raw `cp.panel`.
- **13d:** how T2 runs a JS opener past the harness sentinel; the T6 portal parameter; the accept-section format.
- **Ports:** X7 ordering (14–20); the "Embers" currency (15); the groups probe sha (18); the Notify form (20); per-key revision storage (23).
- **24b:** what data each module seeds.
- **25h:** what each rewritten group asserts.
- **Shared file:** `modules/accounts/src/admin_tests.rs` is created by whichever of 13c.2a3 or 25f2 runs first. Assign it to 13c.2a3.
