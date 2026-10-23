# TODO tracking (port from ArcGame) + migration of open gaps

## Context

Owner (2026-10-23), after the trackable commit format landed: "no to przenieś bo bez
nich to nie wiadomo po co hook" — port ArcGame's TODO tracking so the checked commit
suffix and `Kind: review-fix` have something to track against. Owner decisions:
- **Both levels**: `docs/roadmap/feature-tracker.md` keeps feature-level status (BaaS
  gap matrix); `docs/todo/<topic>.md` holds item-level work (bugs, known limits, owner
  checks). The tracker links a TODO file instead of listing item-level gaps.
- **Migrate now**: existing open item-level gaps move into `docs/todo/`.

This plan run is the owner's authorization for the named root-guidance and agent-file
edits (step 1) and for `docs-writer` writing TODO items during the migration (step 3).

Rule source: `G:/ArcGame/.agents/shared/core-rules.md` **Plans And Status Docs** (all
TODO clauses :37-93), `G:/ArcGame/.claude/agents/core-implementer.md` Leftovers
paragraph, `G:/ArcGame/docs/reference/agents/docs-writing.md:76-78` (a doc's limits
section links `docs/todo` instead of copying items).

### Overlap / what happens to each existing place

- `feature-tracker.md`: feature rows stay; Notes cells that list item-level gaps become
  a link. Its rule :24 ("Partial rows must say what is missing in Notes") is rewritten
  (step 1). Its **Change log** entries are dated history → untouched.
- `.agents/shared/gamebackend.md` known-gap lines (≈:245, :374, :389, :436, :453-455,
  :481) → links (step 4).
- Reference docs: each **item** is classified, not each doc. Accepted behaviour limits
  stay where they are. Open tasks listed there (e.g. `docs/reference/event-plane-ops.md:69-80`,
  `weles-design.md:840-847`, `module-reference.md:163`) move to TODO and the doc's list
  is replaced by a link (step 4).
- Dated docs (`docs/status/`, `docs/summaries/`, `docs/reviews/`, plans,
  `game-backend-feature-gaps.md` frozen analysis): archives, never edited; their open
  items are copied into TODO with the source cited.
- `KNOWN GAP` code comments (≈23, e.g. `modules/gateway/src/lib.rs:237,259,1622,1782`,
  `modules/inventory/src/store.rs:8`, `weles/src/control.rs:423`,
  `tools/splitproof/src/main.rs:2500`): the pending work becomes a TODO item citing the
  comment; the comment stays as a present-tense description of current behaviour at
  the site (allowed by **Comments**). No `.rs` edits in this plan.
- Agent/dispatch "recorded as known gaps" wording (`planning-dispatch.md:216,249`,
  `subagent-dispatch.md:20`, `.claude/agents/{core-implementer:40,test-author:69}`,
  `.grok/agents/{core-implementer:41,test-author:68}`) → Leftovers for owner (step 1).

A pre-plan read-only sweep found ≈180 doc items over ≈20 areas; it did not cover code
comments, reference-doc open lists or the gap-analysis errata. Step 2 produces the
complete, committed inventory.

## Swarog adaptation of the ArcGame rule (step 1 ports every clause)

All ArcGame clauses :37-93 are carried over, with these substitutions only:
- "TODO lists only in `docs/todo/`" gets one exception: feature-level status lives in
  `docs/roadmap/feature-tracker.md`; a feature first found during work becomes a tracker
  row with `(added YYYY-MM-DD)`.
- Topics: module/crate kebab names (`accounts`, `wallet`, `core-edge`, `splitproof`, …);
  cross-cutting topics by name (`account-deletion`, `event-plane`, `agent-tooling`).
- No `docs/<type>/<folder>/` layout change (swarog plans stay flat).
- `Status: done` / `Status: waiting for approval` plan lines as in ArcGame :90-93.
Everything else verbatim in substance: pair files, `Last updated`, section order and
meanings (In progress, Owner checks without `###`, Open with Known limits
`(<plan-slug> step N)` / `(<area>)` and last `### Deferred`, Notes with `### Not doing`),
item format, done format with `<plan-slug> step N` | sha 7–12 hex | `no ref`, `date
unknown`, items-not-session-notes, Leftovers for owner (implementer reports, owner
decides, lead writes deferred ones), list `docs/todo/` before creating a topic,
starting → In progress (the lead, before dispatch), finishing → done (the step's
implementer, in the step commit), close-on-commit for the touched topic.

## Steps

### Step 0 — plan copy `[inline]`
`docs/plans/2026-10-23-2021-todo-tracking-plan.md` (HHMM from `date -u`), commit
`docs(plans): add the todo tracking plan`.

### Step 1 — rules `[docs]` (sonnet)
(a) Files and exact claims:
- `.agents/shared/core-rules.md` **Plans And Status Docs**: add the TODO subsection and
  `Status:` lines per the adaptation above.
- `.agents/shared/planning-dispatch.md:216-217` (round-cap leftovers → Leftovers for
  owner, deferred ones to `docs/todo`), `:249` (sibling sweep → fix or list under
  Leftovers for owner), `:103-106` (`[docs]` lane scope adds `docs/todo`).
- `docs/reference/subagent-dispatch.md:20` (handoff "known gaps" → Leftovers for owner).
- Who writes TODO files (one owner per action):
  - implementers (`core-implementer`, `test-author`, `mockup-implementer`) never **add**
    TODO items; they report Leftovers for owner, and they DO move items their change
    closed to the done file in the same commit (close-on-commit);
  - the lead's own `docs/todo/` edits outside a dispatched step are inline:
    `planning-dispatch.md` closed `[inline]` list (:116-121) gains "the lead's
    `docs/todo/` edits outside a dispatched step: adding owner-deferred leftovers, and
    moving an item to `## In progress` before dispatching the step that works on it".
    The entry says explicitly that it gets no review round (the owner already decided
    each item; owner approval of this plan authorizes extending the closed list);
  - inside a step, the step's implementer moves the items its change closed to the done
    file in the step commit (subagents are not banned from that: the inline entry only
    covers the lead's edits outside a step);
  - `docs-writer` adds TODO items only when its dispatch names them (a planned
    migration/docs step), never on its own initiative.
- `.claude/agents/core-implementer.md:40`, `test-author.md:69`, `mockup-implementer.md`
  return section; `.grok/agents/core-implementer.md:41`, `test-author.md:68`,
  `mockup-implementer.md:71` return section: the ArcGame Leftovers paragraph with the
  verb "never add" and the close-on-commit duty.
- `.claude/agents/docs-writer.md`: :43-45 docs layout adds `docs/todo`; :87-91
  out-of-scope stale prose → Leftovers for owner; the dispatch-named TODO exception
  above; and the ArcGame `docs-writing.md:76-78` clause: a doc's limits/known-gaps
  section describes accepted behaviour only and links `docs/todo/<topic>.md` for open
  work instead of listing it.
- `.claude/agents/proof-auditor.md:26`, `.grok/agents/proof-auditor.md:11`: "claimed
  known gap" → "a Leftovers-for-owner entry or a `Known limits` TODO item".
- Define "TODO list" in core-rules as `- [ ]` task items; a plan's migration inventory
  table is a record, not a TODO list.
- `docs/roadmap/feature-tracker.md` **How to use this**: rewrite :24 (partial rows link
  the topic's TODO file for what is missing), narrow :25-26 to feature-level gaps (an
  item-level gap found during work follows Leftovers for owner → `docs/todo`), add the
  item-level vs feature-level split; bump the `Last update` header.
- `AGENTS.md`: none (core-rules is loaded for every edit/commit; Non-Negotiables stay
  MANDATORY-only).
(b) First: steps 2–4 write in this format.
Commit `docs(agents): port the todo tracking rules (todo-tracking step 1)`.

### Step 2 — committed inventory `[docs]` (sonnet)
(a) **Appendix: inventory** at the end of the plan copy
`docs/plans/2026-10-23-2021-todo-tracking-plan.md` (TODO lists live only in
`docs/todo/`; the inventory is a migration record, not a TODO list). This dispatch
names the appendix as an authorized addition to the dated plan copy.
(b) After step 1 (classes use its definitions); before step 3 (its input).
(c) Sweep: tracker Notes/subsections, `gamebackend.md`, root `README.md` (e.g.
:139-141), `docs/status|summaries|reviews|research|design|rust-sketch`,
top-level `docs/*.md`, `docs/plans/*.md`, `docs/reference/*.md` known-gap sections,
`game-backend-feature-gaps.md` errata (:287-318), and code/test comments and names
matching `(?i)known[ _-]?(gap|limit)` (Grep tool over `core modules api cmd tools weles
demos scripts .claude/hooks`; e.g. `modules/gateway/src/push_ws_tests.rs:2729`,
`.claude/hooks/tests/test_hooks.py` known-gap tests). Comments whose wording is process
prose ("recorded, not fixed here") are listed in the report, not edited. One table row per
item: stable ID (`<TOPIC>-NN`), title, class (`open` | `accepted` | `feature` |
`process` | `closed`), target topic (exactly one; owner module when cross-cutting is not
needed), source `file:line` (date). `closed` and `accepted` rows carry one line of
evidence. Totals per class must add up. Topics cover every area found (incl. `audit`,
`match`, `rating`, `scheduler`, `core-*`, `archcheck`, `topiccheck`, `conformance`,
`eventctl`, …).
Commit `docs(plans): add the todo migration inventory (todo-tracking step 2)`.
Review focus: spot-check every `accepted`/`closed`/`process` row against its source.

### Step 3a/3b/3c — write TODO pairs `[docs]` (sonnet), parallel
(a) `docs/todo/<topic>.md` + `<topic>-done.md` for every topic with ≥1 `open` row,
split by topic so the three agents own disjoint files: 3a accounts, account-deletion,
gateway, wallet, admin, apikeys, audit, notifications, mail; 3b friends, groups, party,
push, storage, leaderboard, rating, match, inventory, characters, config, scheduler,
event-plane, core-*; 3c weles, devctl/verifyctl/processctl/splitproof/archcheck/
topiccheck/conformance/eventctl/csharp-client, agent-tooling. Topics the inventory adds
are assigned in the inventory table to one of 3a/3b/3c.
(b) After step 2.
(c) Every `open` row → one `- [ ]` item; detail cites the inventory ID and source. Each
item is re-checked at HEAD (rust-analyzer → targeted read → Grep as lower bound; method
named in the report); if found closed, it is not written and the report lists the ID
with evidence. `feature` rows with no tracker row are listed in the report for step 4.
Done files `_None._`. Each agent `git add`s its own new files, then commits with an
explicit pathspec of those files only (retry on `index.lock`).
Report: per ID → written | verified-closed (evidence).
Commits `docs(todo): migrate <group> open items (todo-tracking step 3a|3b|3c)`.

### Step 4 — single source in tracker, guidance and reference docs `[docs]` (sonnet)
(a) `feature-tracker.md` Notes cells / subsections whose item-level gaps were migrated →
link the TODO file (feature status kept; Change log untouched; `feature` rows found only
in old docs added as rows with `(added 2026-10-23)`); `gamebackend.md` known-gap lines →
links; `README.md:139-141` gap clause → link to `docs/todo/account-deletion.md` (prose
kept); reference docs whose open-task lists moved (named per inventory row) → the list
replaced by a link, accepted limits kept. Heading text that other docs link to stays
unchanged — in particular `feature-tracker.md:275` `### Storage contract and known gaps`
(linked from the frozen `game-backend-feature-gaps.md` errata and `gamebackend.md:454`);
only its body changes. Bump the tracker's `Last update`.
(b) After 3a–3c (links target existing files).
Commit `docs(roadmap): link item-level gaps to docs/todo (todo-tracking step 4)`.

## Review
After each step commit a fresh `core-reviewer` (opus, effort medium), max 2 rounds,
review-fix commits with `Kind: review-fix`. Step 3 review: every `open` inventory ID is
either an item or a verified-closed entry with evidence (count match); no item written
twice; section order exact. Step 4 review: no migrated item still listed at its source
(except archives and code comments), links resolve.

## Verification
- Inventory class totals add up; step 3 reports account for every `open` ID.
- All commits pass the installed commit-msg hook (`todo-tracking` slug).
- No code changes → no cargo run.

## Appendix: inventory

Migration record for step 2, written 2026-10-23 against HEAD `dfc1adbe`. It is a
record, not a TODO list (core-rules **Plans And Status Docs**): step 3 turns every
`open` row into one item under `docs/todo/`, and step 4 links the sources.

Conventions:

- Classes: `open` is a real product or tool gap or bug still to fix, including an
  owner check and work deliberately deferred; `accepted` is a documented, deliberate
  limit or non-goal; `feature` is feature-level work that belongs to
  `docs/roadmap/feature-tracker.md`; `process` is a session, procedure or naming
  note that is not an item; `closed` is fixed, with the commit or code line as
  evidence.
- The date is that of the cited document or, for a code line, of the commit that
  last wrote it. A comment whose wording is process prose ("recorded, not fixed here")
  is cited as the source of its pending work and is not edited.
- Restatements are cited in the evidence of the row they repeat and get no row of
  their own: `README.md:140-141`, the known-gap lines of
  `.agents/shared/gamebackend.md` (:245, :389-392, :405, :436-437, :454, :481), the
  `docs/reference/game-backend-feature-gaps.md` errata (:287-318) and the
  `docs/roadmap/feature-tracker.md` Notes cells.
- Plan non-goal lists that share one rationale are one `accepted` row. `open` and
  `feature` rows are one item each.
- Evidence was read in the code or Grep'd at HEAD where the row cites a code line;
  Grep results are lower bounds. No cargo command was run. A row that cites only a
  document was not re-checked against the code, and step 3 re-checks every `open`
  row before writing it.
- Group is the step 3 agent that owns the topic. A topic with no `open` row gets no
  TODO file.

| ID | Title | Class | Topic | Group | Source | Evidence / notes |
|---|---|---|---|---|---|---|
| ACCOUNTS-01 | Epic OAuth callback renders Postgres outage two ways (503 vs /?epic=error redirect) | open | accounts | 3a | modules/accounts/src/epic_oauth.rs:323 (2026-07-28) | KNOWN GAP comment: no single failure-rendering policy for the handler |
| ACCOUNTS-02 | Apple OIDC provider (seq 2b) | feature | accounts | 3a | docs/roadmap/feature-tracker.md:46 (2026-07-30) | tracker sequence row 2b and "Multi-provider auth" row :140 |
| ACCOUNTS-03 | Facebook and console login providers | open | accounts | 3a | docs/roadmap/feature-tracker.md:140 (2026-08-30) | named in the tracker row title only; each is a new verifier, not a new op; item-level half of a partial tracker row that has no sequence row of its own |
| ACCOUNTS-04 | Steam ticket verifier (seq 6) | feature | accounts | 3a | docs/roadmap/feature-tracker.md:52 (2026-07-28) | tracker sequence row 6 |
| ACCOUNTS-05 | Identity unlink and last-identity policy | feature | accounts | 3a | docs/roadmap/feature-tracker.md:142 (2026-08-30) | tracker row "Account linking / unlinking" partial; decided deferred to #2b (:114-118) |
| ACCOUNTS-06 | Production self-registration (email verify, password reset, throttling, password policy, email uniqueness) | feature | accounts | 3a | docs/roadmap/feature-tracker.md:144 (2026-08-30) | tracker row partial, seq #4 depends on #3b; register/login exist behind ACCOUNTS_DEV_AUTH (default off) |
| ACCOUNTS-07 | Ban, suspend and moderation state gating session verify | feature | accounts | 3a | docs/roadmap/feature-tracker.md:194 (2026-07-22) | tracker row not started (P1#12); also docs/research/2026-10-22-1707-admin-redesign-gap-analysis.md:94 |
| ACCOUNTS-08 | GDPR data export | open | accounts | 3a | docs/roadmap/feature-tracker.md:145 (2026-09-10) | tracker row "Account self-delete + GDPR export" partial; export does not exist; item-level half of a partial tracker row that has no sequence row of its own |
| ACCOUNTS-09 | Profile fields beyond the handle (avatar, language, editable display name) | open | accounts | 3a | docs/roadmap/feature-tracker.md:146 (2026-09-06) | tracker row partial; display_name immutable (docs/plans/2026-09-05-1921-friends-module-plan.md:589); item-level half of a partial tracker row that has no sequence row of its own |
| ACCOUNTS-10 | Account merge across identities | feature | accounts | 3a | docs/roadmap/feature-tracker.md:154 (2026-08-30) | "merging accounts post-wallet means merging balances and is its own feature"; no tracker row |
| ACCOUNTS-11 | Google and Apple browser-redirect OAuth (PKCE, nonce, provider column on oauth_states) | feature | accounts | 3a | docs/plans/2026-07-30-2230-accounts-federated-providers-plan.md:117 (2026-07-30) | #2a shipped the ID-token path only; modules/accounts/src/epic_oauth.rs hardcodes its callback; no tracker row |
| ACCOUNTS-12 | MFA for player accounts | feature | accounts | 3a | docs/reference/baas-feature-gap-matrix.md:108 (2026-07-11) | matrix lists MFA as absent; no tracker row |
| ACCOUNTS-13 | Provider environment parsed inside the module instead of cmd/* | accepted | accounts | 3a | docs/roadmap/feature-tracker.md:148 (2026-08-30) | deliberate (docs/plans/2026-07-30-2230-accounts-federated-providers-plan.md:112-116): the one module reading env outside a composition root |
| ACCOUNTS-14 | player.promoted from_provider is the constant "guest" | open | accounts | 3a | modules/accounts/src/lib.rs:562 (2026-08-29) | docs/roadmap/feature-tracker.md:150; only guest is promotable today |
| ACCOUNTS-15 | Proof fleet leaves Google unconfigured to keep known-but-unconfigured (503) apart from unknown (400) | accepted | accounts | 3a | docs/roadmap/feature-tracker.md:151 (2026-08-30) | deliberate proof design, named in the tracker |
| ACCOUNTS-16 | Expired sessions and refresh tokens never deleted | closed | accounts | 3a | docs/summaries/2026-07-10-1847-review-findings-verification-summary.md:27 (2026-07-10) | modules/accounts/src/store.rs:857,870,885 delete expired sessions, delete tickets and refresh tokens |
| ACCOUNT-DELETION-01 | No redaction of asyncevents.events and audit.log after player.deleted | open | account-deletion | 3a | docs/roadmap/feature-tracker.md:145 (2026-09-10) | the id and, for player.deleted, the handle survive 90/30 days; mirrored in docs/reference/game-backend-feature-gaps.md:306 and README.md:140 |
| ACCOUNT-DELETION-02 | leaderboard, rating and match cannot purge a deleted player | open | account-deletion | 3a | docs/roadmap/feature-tracker.md:145 (2026-09-10) | match.finished carries opaque contestant strings; same root as the notifications row; README.md:140-141; .agents/shared/gamebackend.md:245 |
| ACCOUNT-DELETION-03 | mail cannot purge a deleted player | open | account-deletion | 3a | docs/roadmap/feature-tracker.md:145 (2026-09-10) | mail.outbox has no player_id; README.md:140-141 |
| ACCOUNT-DELETION-04 | Purge is scheduled daily, not immediate, and capped sweeps may need several fires | accepted | account-deletion | 3a | docs/roadmap/feature-tracker.md:145 (2026-09-10) | stated design: shared daily player-purge schedule, 64 owners per fire (storage section :290) |
| ACCOUNT-DELETION-05 | Orphan-row race window left by the tombstone plain-read guard | accepted | account-deletion | 3a | docs/plans/2026-09-10-1917-account-deletion-purge-plan.md:440 (2026-09-10) | plan: closing it needs one shared per-player advisory lock on every guarded write path, "Not taken" |
| GATEWAY-01 | Session verifier flattened an accounts outage into 401 | closed | gateway | 3a | docs/summaries/2026-07-10-1847-review-findings-verification-summary.md:22 (2026-07-10) | modules/gateway/src/verifier.rs:25-39,115-117 return Err(VerifyUnavailable) so an outage is 503 |
| GATEWAY-02 | Split-mode Epic OAuth lost the token to a followed redirect | closed | gateway | 3a | docs/summaries/2026-07-10-1847-review-findings-verification-summary.md:14 (2026-07-10) | modules/gateway/src/proxy.rs:95 builds the client with redirect::Policy::none() |
| GATEWAY-03 | DevSessionVerifier mints non-uuid player ids that nine store sites fold into green empty results | open | gateway | 3a | docs/roadmap/feature-tracker.md:157 (2026-09-12) | modules/gateway/src/verifier.rs:46-48 still returns the dev-token suffix verbatim; sites modules/wallet/src/store.rs:403, modules/notifications/src/store.rs:98,287, modules/friends/src/store.rs:412,443,475, modules/groups/src/store.rs:498,544; candidate closure is a deterministic uuid |
| GATEWAY-04 | Describe validation checks the method-id dimension only (verb, path, auth trusted verbatim) | open | gateway | 3a | modules/gateway/src/lib.rs:1622 (2026-07-28) | KNOWN GAP comment: a buggy peer can claim another provider's URL path unauthenticated; needs a path-ownership rule |
| GATEWAY-05 | Repeated describe fetch-task failures are invisible to /readyz | open | gateway | 3a | modules/gateway/src/lib.rs:1782 (2026-07-28) | KNOWN GAP comment: keep-last hides a broken pass behind a green readiness; only a log records it |
| GATEWAY-06 | Concurrent fan-out of POOL_STOP_BUDGET is not distinguishable by tests | open | gateway | 3a | modules/gateway/src/tests.rs:2477 (2026-07-28) | needs remote::Pool::with_factory (core/remote/src/lib.rs:804, private) reachable from gateway tests |
| GATEWAY-07 | Player QUIC port serves a dev-CA server certificate only, no production WebPKI path | open | gateway | 3a | docs/plans/2026-07-08-1330-rust-sketch-quic-player-front-plan.md:365 (2026-07-08) | core/app/src/lib.rs:1020-1030 binds the player front with server_tls_public off the shared dev CA; TLS_MODE=acme covers HTTP only (docs/reference/hetzner-deploy-checklist.md:21) |
| GATEWAY-08 | Gateway session-verification cache (deferred) | open | gateway | 3a | docs/plans/2026-07-30-2230-accounts-federated-providers-plan.md:107 (2026-07-30) | no cache in modules/gateway/src/verifier.rs; a cache interacts with refresh-family revocation and needs its own reviewed change |
| GATEWAY-09 | Idle-connection select guard in the push invoke loop has no failing test | accepted | gateway | 3a | modules/gateway/src/push_ws_tests.rs:2729 (2026-09-28) | KNOWN LIMIT comment: select! disables the arm on None, so removing the guard changes no observable behaviour; the neighbouring regression is pinned |
| WALLET-01 | Purge sweep and movement guard share no lock (money can reappear after erasure) | open | wallet | 3a | docs/roadmap/feature-tracker.md:177 (2026-09-12) | modules/wallet has no advisory lock (store.rs:157,233 only comment on it); storage closes the same race (modules/storage/src/service.rs:237, modules/storage/src/projection.rs:61) |
| WALLET-02 | No ledger retention (append-only, unbounded) | accepted | wallet | 3a | docs/plans/2026-07-28-2125-wallet-module-plan.md:1041 (2026-07-30) | plan gap 1: reads stay bounded by the (player_id, seq DESC) index; "keep money history forever" may be the permanent answer |
| WALLET-03 | recent_ledger has a LIMIT but no cursor | accepted | wallet | 3a | docs/plans/2026-07-28-2125-wallet-module-plan.md:1045 (2026-07-30) | plan gap 2: only reader is the admin drill-down; a player-facing history op must add a cursor |
| WALLET-04 | Starter grant is single-currency and new-players-only | accepted | wallet | 3a | docs/plans/2026-07-28-2125-wallet-module-plan.md:1048 (2026-07-30) | plan gap 3; back-filling is the admin grant page |
| WALLET-05 | wallet.myBalances is HTTP-only, not on the player-QUIC allow-list | closed | wallet | 3a | docs/plans/2026-07-28-2125-wallet-module-plan.md:1050 (2026-07-30) | tools/csharp-client-gen/testdata/manifest.golden.json:992 lists wallet.myBalances as a player wire method and modules/gateway/src/lib.rs:1269 finds routes by rpc method name, so no allow-list exists |
| WALLET-06 | A successful admin grant redirects to the bare catalog view, dropping ?player= | open | admin | 3a | docs/plans/2026-07-28-2125-wallet-module-plan.md:1055 (2026-07-30) | authority is admin render_after_submit, modules/admin/src/lib.rs:1401,1411 (see_other without a query); topic changed from wallet to admin after review (ID kept stable) |
| WALLET-07 | Portal money movements carry no operator attribution | open | admin | 3a | docs/plans/2026-07-28-2125-wallet-module-plan.md:1060 (2026-07-30) | admin.action records field names only and adminSubmit carries no operator identity across the edge; topic changed from wallet to admin after review (ID kept stable) |
| WALLET-08 | Postgres session headroom 85/87 breaks at the 13th DB-backed process | closed | wallet | 3a | docs/plans/2026-07-28-2125-wallet-module-plan.md:1066 (2026-07-30) | d97503f2 provisions 147 usable sessions from REQUIRED_MAX_CONNECTIONS = 150, pinned by assert_eq!(REQUIRED_MAX_CONNECTIONS, 150) at weles/src/pgfloor_tests.rs:276 |
| ADMIN-01 | resolve_items fetches every remote admin_data on every /admin request, sequentially | open | admin | 3a | docs/roadmap/feature-tracker.md:257 (2026-09-17) | also docs/research/2026-10-22-1707-admin-redesign-gap-analysis.md:38-43 |
| ADMIN-02 | Local-render pages run block_in_place that HTTP_REQUEST_TIMEOUT_MS does not cancel | open | admin | 3a | docs/roadmap/feature-tracker.md:257 (2026-09-17) | apikeys/friends/groups/mail/notifications/wallet |
| ADMIN-03 | Avatar palette seeded from the raw URL param, not the canonical lowercase id | open | admin | 3a | docs/roadmap/feature-tracker.md:257 (2026-09-17) | friends drill-down; same pattern in wallet and notifications |
| ADMIN-04 | PARAM_PLAYER and PLAYER_REF_PREFIX redeclared as local literals in friends, notifications, storage and wallet | open | admin | 3a | docs/roadmap/feature-tracker.md:257 (2026-09-17) | should derive from accountsapi::admin::PLAYERS_ROW_MENU; Grep at HEAD finds the pair in modules/{friends,notifications,storage,wallet}/src/admin.rs (the tracker names three; storage is a fourth) |
| ADMIN-05 | A remote-fetch Err collapses an item's section and label to its id | open | admin | 3a | docs/plans/2026-09-05-1921-friends-module-plan.md:596 (2026-09-05) | modules/admin/src/lib.rs:1495-1510 per the plan; breaks the page's slug on a DB blip; companion to the audit and notifications rows |
| ADMIN-06 | PLAYERS_ROW_MENU ordering is unpinned across topologies | open | admin | 3a | docs/plans/2026-09-05-1921-friends-module-plan.md:601 (2026-09-05) | characters, friends, notifications and wallet all use priority 0 in their src/admin.rs, inventory uses 10 |
| ADMIN-07 | Operator RBAC (fixed roles in v1, permissions declared by modules, portal-side check) | feature | admin | 3a | docs/research/2026-10-22-1707-admin-redesign-gap-analysis.md:166 (2026-10-22) | owner decision 2026-10-22; admin.users has no roles today; no tracker row |
| ADMIN-08 | Admin redesign: panel manifest/data split, parallel fan-out with deadlines, entity detail, search, typed primitives (G1-G12) | feature | admin | 3a | docs/research/2026-10-22-1707-admin-redesign-gap-analysis.md:57 (2026-10-22) | owner decision to redesign the admin and its API; no plan yet; no tracker row |
| ADMIN-09 | Event-plane admin surface (subscriptions, pause/resume/retry/skip) with an undecided owner | feature | admin | 3a | docs/research/2026-10-22-1707-admin-redesign-gap-analysis.md:81 (2026-10-22) | core cannot contribute an adminapi::Item; resume/skip exist only in tools/eventctl; decide the owner before planning; no tracker row |
| ADMIN-10 | Admin pages for match, rating and leaderboard | feature | admin | 3a | docs/research/2026-10-22-1707-admin-redesign-gap-analysis.md:54 (2026-10-22) | no admin provider wired; also docs/plans/2026-07-14-1851-admin-extension-points-plan.md:423; party is accepted without a page; no tracker row |
| ADMIN-11 | Mockup-driven backend features: suspend account, sessions list, active-players aggregate, wallet spent-24h, ledger across players | feature | admin | 3a | docs/research/2026-10-22-1830-admin-redesign-data-sources.md:55 (2026-10-22) | each mockup action with no backend renders disabled and becomes a per-module follow-up (research :173-175); no tracker row |
| ADMIN-12 | No TOTP/2FA for operators | accepted | admin | 3a | docs/reference/hetzner-deploy-checklist.md:71 (2026-07-10) | admin hardening plan non-goals (docs/plans/2026-07-10-2314-admin-hardening-plan.md:90) |
| ADMIN-13 | Admin non-goals: HTTP-to-HTTPS redirect, CSRF on the login form, multi-admin invitation UI | accepted | admin | 3a | docs/plans/2026-07-10-2314-admin-hardening-plan.md:91 (2026-07-10) | stated non-goals; adminctl create-user covers N users |
| ADMIN-14 | Remote admin form submits (remote pages read-only) and admin AUTH_DDL lock order | closed | admin | 3a | docs/plans/2026-07-10-2314-admin-hardening-plan.md:94 (2026-07-10) | adminSubmit exists (api/admin/api); AUTH_DDL deadlock fixed 153bf4b (docs/status/2026-07-17-2348-macos-port-complete-status.md:55) |
| ADMIN-15 | Login page fonts not linked and .checkbox-group unstyled | closed | admin | 3a | docs/research/2026-07-14-1804-admin-extension-points-research-summary.md:137 (2026-07-14) | modules/admin/src/login.html.tmpl:7-9 links the fonts; modules/admin/src/theme.css:236-240 styles .checkbox-group |
| ADMIN-16 | Hetzner deploy-day checklist (23 manual boxes) | process | admin | 3a | docs/reference/hetzner-deploy-checklist.md:10 (2026-07-10) | a reusable procedure rerun per deploy, not a pending check |
| APIKEYS-01 | Role-policy editor is a blind full replace (checkboxes render unchecked) | open | apikeys | 3a | modules/apikeys/src/admin.rs:121 (2026-07-14) | docs/plans/2026-07-14-1421-apikeys-configurator-plan.md:316; policy_method_options sets checked: false (admin.rs:152) |
| APIKEYS-02 | rpc_modules_from_fs scans each api/<name>/api/src/lib.rs as text | accepted | apikeys | 3a | docs/plans/2026-07-14-1421-apikeys-configurator-plan.md:326 (2026-07-14) | plan: textual proxy accepted, stronger authority named |
| APIKEYS-03 | API-key expiry, per-key rate limits, secret rotation | feature | apikeys | 3a | docs/summaries/2026-07-09-1845-api-key-policy-summary.md:55 (2026-07-09) | listed non-goals/follow-ups; rotate/expires are NEW in docs/research/2026-10-22-1830-admin-redesign-data-sources.md:106,111; no tracker row |
| APIKEYS-04 | No Identity::Service widening or impersonation of Player ops | accepted | apikeys | 3a | docs/summaries/2026-07-09-1845-api-key-policy-summary.md:58 (2026-07-09) | "only if a real need appears"; plan non-goal :314 |
| APIKEYS-05 | Passthrough surfaces (/admin, /accounts/epic) stay key-free | accepted | apikeys | 3a | docs/plans/2026-07-09-1711-api-key-policy-plan.md:317 (2026-07-09) | plan non-goal |
| APIKEYS-06 | conformance_key_rejected is pure arithmetic over a length | open | apikeys | 3a | modules/apikeys/src/conformance.rs:16 (2026-07-12) | docs/status/2026-09-05-1416-leaderboard-shared-table-pollution-status.md:89; executes no production code (gateway-side cases were added by the push rollout) |
| APIKEYS-07 | Key hashing at rest | closed | apikeys | 3a | docs/summaries/2026-07-09-1845-api-key-policy-summary.md:55 (2026-07-09) | modules/apikeys/src/store.rs:9-11 stores only a SHA-256 secret_hash |
| APIKEYS-08 | Uncapped operator-supplied admin form strings | closed | apikeys | 3a | docs/plans/2026-07-28-2125-wallet-module-plan.md:1073 (2026-07-30) | modules/apikeys/src/conformance.rs:20-36 call store::validate_policy/validate_name over COLUMN_CAPS |
| AUDIT-01 | Audit filters by topic and actor, paging, export | feature | audit | 3a | docs/research/2026-10-22-1830-admin-redesign-data-sources.md:162 (2026-10-22) | page shows the latest 100 rows; no tracker row |
| AUDIT-02 | audit admin_data returns Err (never-Err contract) | open | audit | 3a | modules/audit/src/lib.rs:263 (2026-07-08) | map_err(internal) on admin_content; docs/plans/2026-09-05-1921-friends-module-plan.md:593 |
| AUDIT-03 | Domain-op awaits are unbounded below the whole-request HTTP timeout | accepted | audit | 3a | docs/plans/2026-07-13-1415-audit-remediation-plan.md:200 (2026-07-13) | inventory owner_of, match mmr, admin fan-out; covered by HTTP_REQUEST_TIMEOUT_MS, consistent with unbounded player dispatch (docs/reviews/2026-07-13-2138-audit-remediation-status.md:63) |
| AUDIT-04 | Audit table ALTER TABLE for event_id | closed | audit | 3a | docs/status/2026-07-12-0941-remediation-round4-summary.md:23 (2026-07-12) | commit 1970677 folded event_id into CREATE TABLE |
| AUDIT-05 | Analytics and telemetry event pipeline | open | audit | 3a | docs/roadmap/feature-tracker.md:253 (2026-07-22) | tracker row partial (P1#13); item-level half of a partial tracker row that has no sequence row of its own |
| NOTIFICATIONS-01 | match.finished produces no inbox row | open | notifications | 3a | docs/roadmap/feature-tracker.md:244 (2026-08-31) | winner/loser are opaque contestant strings; docs/plans/2026-08-31-0032-notifications-inbox-3a-plan.md:558 |
| NOTIFICATIONS-02 | No shared pagination grammar (cursor lives in one module's request body) | open | notifications | 3a | docs/plans/2026-08-31-0032-notifications-inbox-3a-plan.md:555 (2026-08-31) | authority fix is query args plus Page<T> in opsapi |
| NOTIFICATIONS-03 | Per-player notification preferences and mute | feature | notifications | 3a | docs/plans/2026-08-31-0032-notifications-inbox-3a-plan.md:561 (2026-08-31) | no tracker row |
| NOTIFICATIONS-04 | No command topic: every module wanting an inbox row edits notifications | open | notifications | 3a | docs/plans/2026-09-05-1921-friends-module-plan.md:591 (2026-09-05) | mail closed this for itself (mail.send_requested) |
| NOTIFICATIONS-05 | notifications admin_data returns Err (never-Err contract) | open | notifications | 3a | modules/notifications/src/admin.rs:454 (2026-08-31) | map_err to opsapi::Error::internal; docs/plans/2026-09-05-1921-friends-module-plan.md:593 |
| NOTIFICATIONS-06 | A notifications.new nudge can lose a race with its own commit | accepted | notifications | 3a | docs/reference/push-hub.md:155 (2026-09-05) | documented client contract: a nudge is a hint to re-read; fixing needs a post-commit hook in core/asyncevents (docs/plans/2026-09-03-2022-push-hub-websocket-plan.md:349) |
| NOTIFICATIONS-07 | fleet_tests snapshot stays hand-written | accepted | notifications | 3a | docs/plans/2026-08-31-0032-notifications-inbox-3a-plan.md:563 (2026-08-31) | deriving it from FleetSpec would make the test tautological |
| NOTIFICATIONS-08 | Push notifications FCM/APNs (seq 3c) | feature | notifications | 3a | docs/roadmap/feature-tracker.md:247 (2026-08-31) | tracker row not started |
| NOTIFICATIONS-09 | Title news and announcements | feature | notifications | 3a | docs/roadmap/feature-tracker.md:254 (2026-07-22) | tracker row not started, "fits the notifications module later" |
| MAIL-01 | Mail queue pause, retry-now and park from the admin | feature | mail | 3a | docs/research/2026-10-22-1830-admin-redesign-data-sources.md:124 (2026-10-22) | parked/requeue exist; pause and retry-now are new; no tracker row |
| MAIL-02 | TLS negotiation (STARTTLS upgrade, certificate check, AUTH) unproven by any automated test | open | mail | 3a | docs/roadmap/feature-tracker.md:246 (2026-09-05) | a cfg(test) plaintext constructor pins the SMTP dialogue only |
| MAIL-03 | mail.send_requested has no in-tree producer | open | mail | 3a | docs/roadmap/feature-tracker.md:246 (2026-09-05) | Grep: only mail and audit reference SEND_REQUESTED; intended producer is accounts (seq 4) |
| MAIL-04 | Address storage and verification in accounts (nothing resolves a player_id to a recipient) | feature | mail | 3a | docs/plans/2026-08-31-1650-mail-outbound-channel-3b-plan.md:874 (2026-08-31) | seq #4 in the tracker |
| MAIL-05 | A remote admin submit produces no admin.action row (needs mail.operator_sent topic) | open | mail | 3a | docs/roadmap/feature-tracker.md:246 (2026-09-05) | Grep: no operator_sent in modules/mail/src |
| MAIL-06 | cancel cannot recall a message the relay already accepted | accepted | mail | 3a | docs/roadmap/feature-tracker.md:246 (2026-09-05) | inherent to SMTP hand-off |
| MAIL-07 | Mail non-goals: templates, mailctl CLI, mail.sent/failed events, bounce handling, DKIM/SPF | accepted | mail | 3a | docs/plans/2026-08-31-1650-mail-outbound-channel-3b-plan.md:864 (2026-08-31) | listed as deliberate non-goals |
| FRIENDS-01 | Socket presence and follow | open | friends | 3b | docs/roadmap/feature-tracker.md:202 (2026-09-06) | tracker row "Presence / online status / follow" partial; gateway has no DB, presence is per-process RAM, offline runs in a Drop that cannot await; item-level half of a partial tracker row that has no sequence row of its own |
| FRIENDS-02 | Friend blocking | open | friends | 3b | docs/plans/2026-09-05-1921-friends-module-plan.md:586 (2026-09-05) | a future state on the existing edge plus Removed{reason: "blocked"}; named in the tracker row title only; tracker row :200 shows the status as shipped and names blocking in its title, so this is also an overclaim (see the overclaim list) |
| FRIENDS-03 | Handle-existence oracle is not closed (201 vs 404), only rate-limited | open | friends | 3b | docs/roadmap/feature-tracker.md:200 (2026-09-06) | .agents/shared/gamebackend.md:405 repeats it |
| FRIENDS-04 | Friends admin page never compared against a live render (owner check) | open | friends | 3b | docs/plans/2026-09-05-1921-friends-module-plan.md:609 (2026-09-05) | templates are include_str! embedded and no fleet was booted during Step 8; needs devctl up monolith then /admin/friends |
| FRIENDS-05 | decline has no split-proof assertion | accepted | friends | 3b | docs/roadmap/feature-tracker.md:200 (2026-09-06) | "[FR1]-[FR9] ... decline deliberately unasserted" is a stated decision |
| FRIENDS-06 | admin_recent orders by the text-cast id column | open | friends | 3b | modules/friends/src/store.rs:461 (2026-09-05) | docs/roadmap/feature-tracker.md:258; Read at HEAD: select list projects id::text, ORDER BY created_at DESC, id DESC unqualified |
| FRIENDS-07 | Raced retry branch of request is not exercised by any test | open | friends | 3b | modules/friends/src/tests.rs:1072 (2026-09-05) | needs two concurrent sessions interleaved between insert_pending_tx and find_pair_tx; no production hook exists |
| GROUPS-01 | Group invites and membership changes produce no push nudge or notification | open | groups | 3b | docs/roadmap/feature-tracker.md:201 (2026-09-10) | docs/reference/game-backend-feature-gaps.md:296-300 and .agents/shared/gamebackend.md:436 |
| GROUPS-02 | No group rename or join_policy change after create | open | groups | 3b | docs/roadmap/feature-tracker.md:201 (2026-09-10) | plan non-goal docs/plans/2026-09-06-2230-groups-membership-plan.md:663 |
| GROUPS-03 | chat's dependency on role_of is unexercised until chat lands | accepted | groups | 3b | docs/roadmap/feature-tracker.md:201 (2026-09-10) | chat is a not-started tracker row (:203) |
| GROUPS-04 | No group.deleted topic: a teardown, including a sole admin's account deletion, reads as N leaves | open | groups | 3b | docs/roadmap/feature-tracker.md:201 (2026-09-10) | also docs/roadmap/feature-tracker.md:145 and .agents/shared/gamebackend.md:245 |
| GROUPS-05 | memberships_player_idx does not serve list_mine | open | groups | 3b | docs/roadmap/feature-tracker.md:201 (2026-09-10) | no equality predicate on state |
| GROUPS-06 | accept and reject are not exported consts in groupsapi | open | groups | 3b | docs/roadmap/feature-tracker.md:201 (2026-09-10) | every other vocabulary word in that crate is |
| GROUPS-07 | join resolves the caller's handle before its transaction (503 instead of 404 when accounts is down) | open | groups | 3b | docs/roadmap/feature-tracker.md:201 (2026-09-10) | the handle lookup is a directory call made before the transaction opens |
| GROUPS-08 | Invite outage path has no conformance or unit proof | open | groups | 3b | docs/plans/2026-09-28-2212-push-multiplex-invoke-and-party-plan.md:689 (2026-10-05) | party's equivalent is unit-proven by invite_target_rules (modules/party/src/service_tests.rs) |
| GROUPS-09 | admin_recent_groups evaluates a LATERAL count for every row before the LIMIT | open | groups | 3b | modules/groups/src/store.rs:564 (2026-09-10) | docs/roadmap/feature-tracker.md:266; Read at HEAD: JOIN LATERAL then ORDER BY ... LIMIT $2 |
| GROUPS-10 | Groups non-goals: group search, group-delete op, member_count on summaries, channels and messages, player promote/demote | accepted | groups | 3b | docs/plans/2026-09-06-2230-groups-membership-plan.md:659 (2026-09-06) | stated non-goals; channels/messages belong to the chat tracker row; modules/groups/src/admin.rs:17 calls promote/demote a non-goal of the player contract and only ACTION_PROMOTE (operator promote) exists |
| GROUPS-11 | Silent swept invitations (plan non-goal) | closed | groups | 3b | docs/plans/2026-09-06-2230-groups-membership-plan.md:660 (2026-09-06) | reversed by landing: group.member_left reason expired (7807980) |
| PARTY-01 | Disconnect is detected by lease expiry, not socket close | open | party | 3b | docs/roadmap/feature-tracker.md:204 (2026-10-05) | typically lease plus one 5s sweep tick; docs/plans/2026-09-28-2212-push-multiplex-invoke-and-party-plan.md:679; socket-driven leases are the follow-up |
| PARTY-02 | PARTY_LEASE_SECS has no devctl operator override | open | party | 3b | docs/plans/2026-09-28-2212-push-multiplex-invoke-and-party-plan.md:618 (2026-10-05) | only PROOF_PARTY_LEASE_SECS in tools/processctl/src/fleet.rs:126,962,1025 |
| PARTY-03 | Purge residue window leaves a player_revisions row nothing deletes | open | party | 3b | docs/plans/2026-09-28-2212-push-multiplex-invoke-and-party-plan.md:683 (2026-10-05) | an op verified before the session revoke can re-insert a member row after the purge ran; the lease sweep removes the row later |
| PARTY-04 | Sweep COMMIT is not covered by its statement_timeout | open | party | 3b | docs/plans/2026-09-28-2212-push-multiplex-invoke-and-party-plan.md:686 (2026-10-05) | modules/party/src/sweep.rs worst-case arithmetic counts the commit as a bounded statement |
| PARTY-05 | Invite outage path is proven by a unit test only, not by conformance | open | party | 3b | docs/plans/2026-09-28-2212-push-multiplex-invoke-and-party-plan.md:669 (2026-10-05) | conformance drives resolve_target |
| PARTY-06 | No admin page for parties | accepted | party | 3b | docs/plans/2026-09-28-2212-push-multiplex-invoke-and-party-plan.md:619 (2026-10-05) | not a conformance convention; ephemeral lease-bound state |
| PUSH-01 | No Target::Players: a party pushes one Target::Player per member | open | push | 3b | docs/plans/2026-09-28-2212-push-multiplex-invoke-and-party-plan.md:613 (2026-10-05) | changes the backplane Envelope (fronts-before-producers rollout) |
| PUSH-02 | Gateway-global presence view (shared store, friends-graph audience) | feature | push | 3b | docs/reference/push-hub.md:182 (2026-09-05) | presence is front-local and default off; a global view needs a friends graph and persistence; folded into the presence tracker row |
| PUSH-03 | presence online/offline announcements can reach an observer in either order, no sequence number | open | push | 3b | docs/reference/push-hub.md:188 (2026-09-05) | needs presence as sequenced state |
| PUSH-04 | No fuzz target covers the /push frame parser | open | push | 3b | docs/reference/push-hub.md:270 (2026-10-03) | tools/verifyctl/src/stages/fuzz.rs:22 runs frame_decode and wire_decode only |
| PUSH-05 | The fixed /push route is invisible to routecheck | open | push | 3b | docs/reference/push-hub.md:271 (2026-10-03) | a future op at the same path would shadow it with no gate noticing |
| PUSH-06 | Handshake checks API-key presence and validity but not policy | accepted | push | 3b | docs/reference/push-hub.md:259 (2026-10-03) | KeyCheck::PresenceOnly; policy is enforced per invoke |
| PUSH-07 | HTTP limiter charges only the upgrade; outbound bounded by queue depth and aggregate caps | accepted | push | 3b | docs/reference/push-hub.md:263 (2026-10-03) | documented; per-connection and per-IP invoke buckets bound inbound |
| PUSH-08 | After frame_too_large the protocol-level 1009 close can be lost | open | push | 3b | docs/reference/push-hub.md:268 (2026-10-03) | the typed JSON close frame still arrives; listed under Carried gaps (:256-273) with no owner decision accepting it, and its siblings PUSH-04 and PUSH-05 are open |
| PUSH-09 | No invoke resume or replay; graceful stop aborts invokes; 128 KiB escaped frame ceiling | accepted | push | 3b | docs/reference/push-hub.md:142 (2026-10-03) | documented wire contract |
| PUSH-10 | Group join is not permission-checked; membership and in-flight messages are lost on reconnect | accepted | push | 3b | docs/reference/push-hub.md:176 (2026-09-05) | groups address an audience, never a privilege (docs/reference/push-hub.md:177-178); docs/plans/2026-09-03-2022-push-hub-websocket-plan.md:326 |
| STORAGE-01 | Cloud-save download archive from the admin | feature | storage | 3b | docs/research/2026-10-22-1707-admin-redesign-gap-analysis.md:103 (2026-10-22) | admin page is read-only inspection; no tracker row |
| STORAGE-02 | Public objects are addressable by owner id (addressing oracle D1) | accepted | storage | 3b | docs/roadmap/feature-tracker.md:285 (2026-09-17) | storage section "Storage contract and known gaps" |
| STORAGE-03 | No age-based collection or per-collection TTL (lifetime D2) | accepted | storage | 3b | docs/roadmap/feature-tracker.md:297 (2026-09-17) | abandoned live accounts retain saves indefinitely; stated in the storage section |
| STORAGE-04 | Versions restart at 1 after delete and recreate (D6) | accepted | storage | 3b | docs/roadmap/feature-tracker.md:300 (2026-09-17) | compare-and-swap protects one incarnation, not an address's lifetime |
| STORAGE-05 | Address alphabet [A-Za-z0-9._-] with no slash (D9) | accepted | storage | 3b | docs/roadmap/feature-tracker.md:303 (2026-09-17) | validated in the service for every transport |
| STORAGE-06 | Values are normalized jsonb, not byte-identical | accepted | storage | 3b | docs/roadmap/feature-tracker.md:307 (2026-09-17) | byte-preserving payloads need a separate product decision |
| STORAGE-07 | put is not retry_safe and has no idempotency key or replay receipt | accepted | storage | 3b | docs/roadmap/feature-tracker.md:315 (2026-09-17) | client must re-read and reconcile after an ambiguous result |
| STORAGE-08 | No value-indexed query, sharing beyond public_read, or cross-owner atomic writes | accepted | storage | 3b | docs/roadmap/feature-tracker.md:318 (2026-09-17) | docs/plans/2026-09-11-0130-storage-cloud-save-plan.md:426-434 non-goals |
| STORAGE-09 | At the 10,000-object cap put(expected_version = 0) to an owned address returns a 409 with a misleading remedy | open | storage | 3b | docs/roadmap/feature-tracker.md:320 (2026-09-17) | the status is right but the message suggests the wrong fix; positive-version updates stay available |
| LEADERBOARD-01 | leaderboard.scores pollution by splitproof champ-% and replicas-% players kept top_scores_orders_by_wins_desc red | closed | leaderboard | 3b | docs/status/2026-09-05-1416-leaderboard-shared-table-pollution-status.md:7 (2026-09-05) | tools/splitproof/src/main.rs:1021-1064 reset_scoreboard_baseline sweeps every harness prefix at the start of a run (84392a39, 09ff586d); 5151f221 made the leaderboard test clean up on a red run and stop depending on the top-100 window (modules/leaderboard/src/tests.rs:74-89) |
| LEADERBOARD-02 | Leaderboard seasons, reset and rotation (seq 5) | feature | leaderboard | 3b | docs/roadmap/feature-tracker.md:211 (2026-07-28) | tracker row not started |
| LEADERBOARD-03 | Tournaments | feature | leaderboard | 3b | docs/roadmap/feature-tracker.md:212 (2026-07-22) | tracker row not started (P1#6) |
| MATCH-01 | Matchmaking | feature | match | 3b | docs/roadmap/feature-tracker.md:233 (2026-07-22) | tracker row not started (P1#9) |
| INVENTORY-01 | Item stacks and instances (inventory model) | open | inventory | 3b | docs/roadmap/feature-tracker.md:223 (2026-07-22) | tracker row partial: static catalog, per-character holdings; item-level half of a partial tracker row that has no sequence row of its own |
| INVENTORY-02 | The dev-grant route is not a reference mutation pattern | accepted | inventory | 3b | docs/reference/module-reference.md:139 (2026-07-14) | gated by INVENTORY_DEV_GRANT (default off), documented "Do NOT copy" #1; real pattern is grant_starter |
| INVENTORY-03 | Character tombstone table grows without GC | open | inventory | 3b | docs/reference/module-reference.md:151 (2026-07-14) | documented "Do NOT copy" #2: acceptable only in the wipe-is-migration phase |
| INVENTORY-04 | Holdings list truncates silently at HOLDINGS_HARD_LIMIT with no cursor | open | inventory | 3b | modules/inventory/src/store.rs:8 (2026-07-14) | KNOWN GAP comment; docs/reference/module-reference.md:159 |
| INVENTORY-05 | Create/delete reorder left orphaned holdings (review finding 1) | closed | inventory | 3b | docs/summaries/2026-07-10-1847-review-findings-verification-summary.md:12 (2026-07-10) | modules/inventory/src/lib.rs:222-224 plants a character tombstone in the wipe delivery tx and the grant handler skips tombstoned ids; pinned by deleted_before_created_tombstones_and_skips_late_grant (modules/inventory/src/tests.rs:550) |
| CHARACTERS-01 | Character list silently truncates at LIST_HARD_LIMIT | closed | characters | 3b | modules/characters/src/store.rs:8 (2026-07-14) | the cap the comment awaits landed: modules/characters/src/service.rs:150-159 clamps the per-player cap to LIST_HARD_LIMIT so create cannot admit more than list returns; the KNOWN GAP wording is stale |
| CHARACTERS-02 | characters::create has no idempotency key | open | characters | 3b | docs/reference/module-reference.md:168 (2026-07-14) | documented "Do NOT copy" #4, modules/characters/src/service.rs:101-176; match::report ReportId is the exemplar |
| CONFIG-01 | Config revision history view and per-key revision | feature | config | 3b | docs/research/2026-10-22-1830-admin-redesign-data-sources.md:150 (2026-10-22) | only the global revision exists; history derivable from audit.log config.changed; no tracker row |
| CONFIG-02 | Remote config targeting | open | config | 3b | docs/roadmap/feature-tracker.md:250 (2026-07-22) | tracker row :250 is partial: live-reload delivery exists, no targeting; item-level half of a partial tracker row; A/B and live events are CONFIG-06 |
| CONFIG-03 | config.settings accumulates test_* junk rows | open | config | 3b | docs/research/2026-07-14-2058-timing-sensitive-tests-survey.md:105 (2026-07-14) | modules/config/src/tests.rs:220,278,339 clean some namespaces only; low severity |
| CONFIG-04 | Config admin page has no AdminSubmit, so the remote page is read-only in the split | open | config | 3b | docs/research/2026-10-22-1830-admin-redesign-data-sources.md:152 (2026-10-22) | Grep: no AdminSubmit in modules/config |
| CONFIG-05 | Live-deleting a config key | closed | config | 3b | docs/2026-07-07-1213-config-split-verified.md:73 (2026-07-07) | modules/config/src/lib.rs:122-123 trigger is AFTER INSERT OR UPDATE OR DELETE |
| CONFIG-06 | A/B experiments and live events | feature | config | 3b | docs/roadmap/feature-tracker.md:251 (2026-07-22) | tracker rows :251-252 not started (P2#16) |
| SCHEDULER-01 | Scheduler admin editing (create, edit, delete, fire now, recent fires) | feature | scheduler | 3b | docs/research/2026-10-22-1830-admin-redesign-data-sources.md:141 (2026-10-22) | scheduler admin page is read-only today; no tracker row |
| SCHEDULER-02 | Timing-sensitive tests flagged FRAGILE in the 2026-07-14 survey | closed | scheduler | 3b | docs/research/2026-07-14-2058-timing-sensitive-tests-survey.md:69 (2026-07-14) | all 10 fixed per the survey's own addendum (eb288ae, 08ca7c3, 8a77406, c1e86a6, 44e5623, 47489a8) |
| EVENT-PLANE-01 | notifications (16) and mail (13) tests call deliver_all() once | open | event-plane | 3b | docs/reference/event-plane-ops.md:69 (2026-07-14) | counts re-verified at HEAD: modules/notifications/src/tests.rs 16, modules/mail/src/projection_tests.rs 13; fix needs deliver_until_consumed hoisted into asyncevents::testing plus --bless-public-api |
| EVENT-PLANE-02 | Idle-in-transaction belt covers only the plane's own sessions (global timeout is an operator decision) | accepted | event-plane | 3b | docs/reference/event-plane-ops.md:17 (2026-07-14) | "a cluster-wide operator decision, not something the plane can enforce" |
| EVENT-PLANE-03 | Retention sweep can block a roster op's row lock for up to PRUNE_BUDGET (5s) | accepted | event-plane | 3b | docs/reference/event-plane-ops.md:93 (2026-09-10) | documented in the section; SKIP LOCKED rules out deadlock |
| EVENT-PLANE-04 | A paused purge subscription is invisible in /admin and /readyz stays green | accepted | event-plane | 3b | docs/reference/event-plane-ops.md:106 (2026-09-10) | structural: core cannot contribute an adminapi::Item and archcheck bans foreign-schema reads (docs/plans/2026-09-10-1917-account-deletion-purge-plan.md:502-506); metrics + eventctl are the surface |
| EVENT-PLANE-05 | ALLOW_UNSUBSCRIBED and ALLOW_INPROCESS_DEFINED key on the topic string only | accepted | event-plane | 3b | tools/topiccheck/src/main.rs:74 (2026-07-13) | KNOWN GAP comments at :74 and :88; both lists are empty |
| EVENT-PLANE-06 | policy_columns days clamp to i32::MAX | accepted | event-plane | 3b | docs/plans/2026-07-13-1415-audit-remediation-plan.md:203 (2026-07-13) | reachable only from hardcoded literals (7/30 days) |
| EVENT-PLANE-07 | Plane scale choices: one event per delivery tx, no partition parallelism, offline generation bumps | accepted | event-plane | 3b | core/asyncevents/README.md:138 (2026-07-10) | docs/summaries/2026-07-10-1138-durable-event-log-v2-complete-summary.md:73 |
| EVENT-PLANE-08 | No non-Postgres transport, no event origin exposed to consumers, in-process emit/on kept on the same Bus | accepted | event-plane | 3b | docs/plans/2026-07-09-1422-anytx-seam-plan.md:25 (2026-07-09) | plan: a foreign-store producer needs a second Transport impl, out of scope; origin not exposed :281; Bus facade split deferred (docs/plans/2026-07-09-1118-asyncevents-plane-plan.md:66) |
| EVENT-PLANE-09 | Readiness does not flip on a poisoned or paused subscription | accepted | event-plane | 3b | docs/status/2026-07-12-0941-remediation-round4-summary.md:30 (2026-07-12) | plan decision (metrics + eventctl only: docs/plans/2026-07-10-2153-review-round2-hardening-plan.md:519) |
| EVENT-PLANE-10 | POST /events unauthenticated ingress | closed | event-plane | 3b | docs/design/2026-07-09-1652-api-access-control-options-summary.md:37 (2026-07-09) | deleted by durable log v2 (docs/reviews/2026-07-09-architecture-security-review.md:6-11) |
| EVENT-PLANE-11 | Worker timeout-arm failure write could overwrite a replica's progress (review finding 9) | closed | event-plane | 3b | docs/summaries/2026-07-10-1847-review-findings-verification-summary.md:25 (2026-07-10) | core/asyncevents/src/worker.rs:376-395 record_failure is guarded on the claim-time cursor; pinned by the F9 test at core/asyncevents/src/worker_tests.rs:615 |
| EVENT-PLANE-12 | history_contracts seeding cache not transaction-aware (review finding 10) | closed | event-plane | 3b | docs/summaries/2026-07-10-1847-review-findings-verification-summary.md:26 (2026-07-10) | core/asyncevents/src/transport.rs:62-66 documents that no per-process seeding cache exists and enqueue_tx seeds idempotently |
| EVENT-PLANE-13 | Stale outbox/inbox vocabulary in shipping docs (review gap G5) | closed | event-plane | 3b | docs/summaries/2026-07-10-1847-review-findings-verification-summary.md:40 (2026-07-10) | Grep (lower bound) over modules/{scheduler,characters,match,accounts,inventory,audit,leaderboard} finds no outbox/inbox; the remaining hits are the legacy-table DROP DDL and "no outbox" statements in core/asyncevents, and mail/notifications use the words for their own product tables |
| CORE-APP-01 | MODULE_STOP_GRACE_MS is parsed with no floor or clamp | open | core-app | 3b | modules/gateway/src/lib.rs:237 (2026-07-28) | KNOWN GAP comment: a value below the inner 2s timeout drops the stop future; same convention at modules/scheduler/src/lib.rs (4s grace) and core/remote (PROBE_STOP_GRACE); core/app/src/lib.rs:357 |
| CORE-APP-02 | Security review findings 1, 2, 4-6, 8-12 | closed | core-app | 3b | docs/reviews/2026-07-09-architecture-security-review.md:3 (2026-07-10) | addendum :3-31 lists the closing commits (durable log v2 deleted POST /events, 2f6a063, 499bba3, a07a246, 29cbb8a, 58d02f6, c6d0f72); CSRF (docs/reference/hetzner-deploy-checklist.md:63) and hashed keys (modules/apikeys/src/store.rs:9) landed later |
| CORE-APP-03 | SIGTERM bypassed graceful shutdown and the HTTP drain was unbounded | closed | core-app | 3b | docs/summaries/2026-07-10-1847-review-findings-verification-summary.md:15 (2026-07-10) | core/app/src/lib.rs:1489-1508 listens for SIGTERM and Ctrl-C; :29-30,351 EDGE_DRAIN_GRACE_MS and HTTP_DRAIN_GRACE_MS bound the drains |
| CORE-EDGE-01 | Edge mTLS identity is a shared dev CA with fixed localhost SANs (no per-host or per-peer identity) | accepted | core-edge | 3b | docs/reference/weles-design.md:109 (2026-07-21) | core/edge/src/client.rs:57-59 and core/edge/src/tls.rs:13-14,154 document the localhost SAN; breaks silently if hardened, fixed only with the multi-machine work |
| CORE-EDGE-02 | Every mTLS peer is equally trusted; admin ops are not restricted per method | accepted | core-edge | 3b | docs/research/2026-10-22-1707-admin-redesign-gap-analysis.md:178 (2026-10-22) | owner decision "edge trust stays as is" (core/edge/src/tls.rs:174); also security review finding 3 deferral (docs/reviews/2026-07-09-architecture-security-review.md:24) |
| CORE-EDGE-03 | Edge QUIC drain was close-and-hope for non-transport hangs | closed | core-edge | 3b | docs/reviews/2026-07-13-2138-audit-remediation-status.md:65 (2026-07-13) | core/edge/src/server.rs:474-488 RunningServer::shutdown aborts straggler handler futures at grace |
| CORE-LIFECYCLE-01 | Module::start has no generic bound (only stop is bounded) | open | core-lifecycle | 3b | docs/reviews/2026-07-13-2138-audit-remediation-status.md:68 (2026-07-13) | core/lifecycle/src/app.rs:241 awaits m.start unbounded |
| CORE-LIFECYCLE-02 | Module migrations unserialized across replicas | closed | core-lifecycle | 3b | docs/summaries/2026-07-10-1847-review-findings-verification-summary.md:24 (2026-07-10) | core/lifecycle/src/tests.rs:255-260 pins the advisory lock around App::migrate |
| CORE-REMOTE-01 | EdgeDialer parses numeric host:port only (no hostname or DNS answer path) | open | core-remote | 3b | docs/reference/weles-design.md:113 (2026-07-21) | needed once resolve returns real hosts; core/remote EdgeDialer |
| CORE-REMOTE-02 | Pool::stop and Stub::stop set no stopped fence | open | core-remote | 3b | modules/gateway/src/lib.rs:259 (2026-07-28) | KNOWN GAP comment: a dispatch after stop would mint fresh instances; core/remote/src/lib.rs has no stopped flag (Grep); safe today only through app::run teardown order |
| CORE-CONTRIB-01 | Contributions silently dropped wrong-typed values (review finding 12) | closed | core-contrib | 3b | docs/summaries/2026-07-10-1847-review-findings-verification-summary.md:28 (2026-07-10) | core/contrib/src/lib.rs:66-78 takes a typed Slot<T> (compile_fail doc test) and panics when a slot name is reused with another type |
| STATS-01 | Statistics, achievements and battle pass | feature | stats | 3b | docs/roadmap/feature-tracker.md:213 (2026-07-22) | three tracker rows not started (:213-215) |
| STORE-01 | Store, IAP receipt validation, player trading and crafting | feature | store | 3b | docs/roadmap/feature-tracker.md:224 (2026-07-22) | tracker rows not started (:224-227); seq 7 |
| REALTIME-01 | Realtime multiplayer, relayed matches, state sync and lobbies | feature | realtime | 3b | docs/roadmap/feature-tracker.md:234 (2026-07-22) | four tracker rows not started (:234-237), P2 needing a design conversation |
| CHAT-01 | Realtime chat | feature | chat | 3b | docs/roadmap/feature-tracker.md:203 (2026-07-22) | tracker row not started, gated on the realtime decision |
| LORE-01 | Hosted Lore as a Swarog module (adapter, event bridge, per-repo permissions, seat and storage reporting) | feature | lore | 3b | docs/research/2026-10-22-1539-lore-hosting-exploration-summary.md:13 (2026-10-22) | exploration only; owner decisions P1-P5 recorded at :88-123; no plan and no tracker row |
| LORE-02 | player_id to user_id rename sweep | feature | lore | 3b | docs/research/2026-10-22-1707-admin-redesign-gap-analysis.md:85 (2026-10-22) | precondition for user: entity refs; not done; decided to follow the wipe rule (docs/research/2026-10-22-1539-lore-hosting-exploration-summary.md:90-93); no tracker row |
| WELES-01 | Readiness probe runs synchronously on the monitor thread during WaitingHealthy | open | weles | 3c | weles/src/supervisor.rs:1326 (2026-07-21) | docs/status/2026-07-16-1342-weles-m1-readiness-review-2-status.md B1 and docs/status/2026-07-16-0810-weles-m1-readiness-review-status.md P7; `observe` still calls `health::probe` inline per service |
| WELES-02 | Fixed DOWN_TIMEOUT not derived from fleet size | open | weles | 3c | weles/src/main.rs:17 (2026-07-15) | constant 130s vs worst-case teardown N x (STOP_GRACE+STOP_FORCE); weles/fleet.split.toml now declares 19 [[service]] entries (19 x 10s worst case exceeds 130s); docs/status/2026-07-16-1342-weles-m1-readiness-review-2-status.md B2 |
| WELES-03 | Control endpoint bound only after the prepare hooks (M2) | open | weles | 3c | weles/src/supervisor.rs:866 (2026-07-16) | hooks run at :807, bind at :873; docs/reference/weles-design.md:905, docs/plans/2026-07-16-0830-weles-m1-hardening-plan.md:259, docs/status/2026-07-16-1342-weles-m1-readiness-review-2-status.md; `weles down` in the prep window gets "very early startup" |
| WELES-04 | flip_current writes `current` without fsync | open | weles | 3c | weles/src/prep.rs:942 (2026-07-16) | docs/status/2026-07-16-1342-weles-m1-readiness-review-2-status.md B3' (optional); no sync_all in prep.rs; sha-verify-on-read (`verify_generation`) catches the symptom |
| WELES-05 | Sustained state-checkpoint failure mid-run degrades silently (M3) | open | weles | 3c | docs/plans/2026-07-16-0830-weles-m1-hardening-plan.md:264 (2026-07-16) | only the initial pin checkpoint is critical (weles/src/supervisor.rs:788); later `reporter.checkpoint` calls stay best-effort |
| WELES-06 | Control-endpoint serve thread panic is not reported by dead() | open | weles | 3c | weles/src/agentapi.rs:237 (2026-07-17) | KNOWN GAP comment; weles/src/control.rs:112 stores thread_dead only inside the Err arm; the agent endpoint was fixed with DeathFlag, control was not |
| WELES-07 | Linux supervisor_alive is pid-only (no start-time reuse check) | open | weles | 3c | weles/src/control.rs:423 (2026-07-16) | KNOWN GAP comment; failure mode is benign over-protection; Windows has the asymmetric check |
| WELES-08 | Backward wall-clock jump in the create-to-record window could mark a live supervisor dead | accepted | weles | 3c | docs/plans/2026-07-16-0830-weles-m1-hardening-plan.md:302 (2026-07-16) | plan names it "acceptable, named, extremely narrow" (core-reviewer L2); docs/status/2026-07-16-1342-weles-m1-readiness-review-2-status.md C1 |
| WELES-09 | Retention protects only one live pin (single-pin assumption) | accepted | weles | 3c | docs/status/2026-07-16-1342-weles-m1-readiness-review-2-status.md:63 (2026-07-16) | weles/src/prep.rs:958-961 documents the one-up-at-a-time invariant; rollout.lock enforces it and deploy takes lock::acquire_deploy (weles/src/prep.rs:430) |
| WELES-10 | Concurrent weles deploy has no lock | closed | weles | 3c | docs/status/2026-07-16-0930-weles-pre-m1-backlog-complete-status.md:62 (2026-07-16) | weles/src/prep.rs:419,430,642 `lock::acquire_deploy` shared by deploy and rollback |
| WELES-11 | Deployed binary sha256 recorded but never verified on read | closed | weles | 3c | docs/status/2026-07-16-1342-weles-m1-readiness-review-2-status.md:62 (2026-07-16) | weles/src/prep.rs:181,680,852 `verify_generation` compares sha256 on discover and rollback |
| WELES-12 | Hung-service stop ignored a failed shutdown (Directive::Kill sibling of P2) | closed | weles | 3c | docs/status/2026-07-16-1342-weles-m1-readiness-review-2-status.md:52 (2026-07-16) | weles/src/supervisor.rs:1411,1477 routes the result through `stop_outcome` |
| WELES-13 | Stale control_endpoint during teardown (P6) | closed | weles | 3c | docs/status/2026-07-16-0930-weles-pre-m1-backlog-complete-status.md:55 (2026-07-16) | drop-order fixed per docs/status/2026-07-16-1342-weles-m1-readiness-review-2-status.md "P6 drop-order"; weles/src/supervisor.rs:971 comment |
| WELES-14 | Stub re-dial after peer restart (B1) | closed | weles | 3c | docs/status/2026-07-15-1120-weles-m0-pre-m1-backlog-status.md:52 (2026-07-15) | not reproducible; recovery pinned by cde5282 (core/remote/src/redial_tests.rs) |
| WELES-15 | devctl down_waits_for_stopped_checkpoint flake (B2) | closed | weles | 3c | docs/status/2026-07-15-1120-weles-m0-pre-m1-backlog-status.md:80 (2026-07-15) | commit 15e3b917 turned its waits into hang-guards |
| WELES-16 | Dead-peer detection window of about 30 s | accepted | weles | 3c | docs/status/2026-07-16-0930-weles-pre-m1-backlog-complete-status.md:64 (2026-07-16) | docs/status/2026-07-15-1745-b1-stub-redial-diagnosis-status.md: idle timeout x HTTP_REQUEST_TIMEOUT_MS; "a separate decision, not a bug" |
| WELES-17 | Weles narrative discipline: manifest is the development flavour | process | weles | 3c | docs/status/2026-07-15-1120-weles-m0-pre-m1-backlog-status.md:44 (2026-07-15) | guidance on how to describe weles, not a work item |
| WELES-18 | verifyctl --fast not run end to end after adding weles-fleet-parity | process | weles | 3c | docs/status/2026-07-16-0930-weles-pre-m1-backlog-complete-status.md:59 (2026-07-16) | session note; the stage was later deleted (docs/reference/weles-design.md:50-58) |
| WELES-19 | Optional splitproof [B1-REDIAL] assertion | closed | weles | 3c | docs/status/2026-07-15-1745-b1-stub-redial-diagnosis-status.md:58 (2026-07-15) | diagnosis found no bug; assertion explicitly "optional, does not discriminate fixed/unfixed"; no `B1-REDIAL` string in tools/ |
| WELES-20 | Replica path has no live single-run test through weles minting and resolve | open | weles | 3c | docs/reference/weles-design.md:840 (2026-07-28) | tools/verifyctl/src/stages/weles_managed_gateway.rs:760-830 proves set consumption with a stage-owned fake agent, not weles minting |
| WELES-21 | Spread across replica instances is not observable in the live fleet | open | weles | 3c | docs/plans/2026-07-30-2149-weles-store-doc-replicas-remediation-plan.md:584 (2026-07-30) | needs a per-instance edge counter in core/metrics; spread proven only at unit level |
| WELES-22 | Cross-process store rejection (deploy racing weles up mint pass) modelled in-process only | open | weles | 3c | docs/plans/2026-07-30-2149-weles-store-doc-replicas-remediation-plan.md:589 (2026-07-30) | weles/master/src/store_tests.rs proves the reject class with two handles in one process |
| WELES-23 | M2 liveness: resolve alive flag is a placeholder and 200 {addrs:[]} is never produced | open | weles | 3c | docs/plans/2026-07-30-2149-weles-store-doc-replicas-remediation-plan.md:581 (2026-07-30) | weles/master/src/store.rs:88-89 `alive`; weles/src/agentapi.rs:34-35; docs/reference/weles-design.md:877-888 |
| WELES-24 | prep_tests retention half became vacuous | accepted | weles | 3c | docs/plans/2026-07-30-2149-weles-store-doc-replicas-remediation-plan.md:593 (2026-07-30) | the plan records the vacuity rather than fabricating a replacement for a property with no failure mode left |
| WELES-25 | Multi-machine weles (agent-master mTLS hop, own CA, master repoint, remote-peer cache, per-host port namespace, artifact distribution) | feature | weles | 3c | docs/reference/weles-design.md:97 (2026-07-21) | deliberately deferred to the real-hardware proof; also covers the remote hello/resolve peer-cred gap (docs/status/2026-07-16-1342-weles-m1-readiness-review-2-status.md C3); the tracker row "Dedicated game-server orchestration / fleet" does not list it; no tracker row |
| WELES-26 | Weles management API (read, imperative ops, fleet.toml CRUD, artifact upload; no UI of its own) | feature | weles | 3c | docs/reference/weles-design.md:142 (2026-07-31) | decided 2026-07-31; includes the open question of a panel module restarting its own host service (:197-199); no tracker row |
| WELES-27 | Agent not installable as a system service; root/privilege requirement for it unstated | open | weles | 3c | docs/reference/weles-design.md:120 (2026-07-21) | also :468-472 "requires the agent binary to be installable as a system service, which it is not today" |
| WELES-28 | Weles assumes the Swarog process contract (PORT/EDGE_ADDR env, GET /readyz, Edge\|Http peer kinds, loopback) | accepted | weles | 3c | docs/reference/weles-design.md:128 (2026-07-21) | stated platform contract in the same errata ("not a bug") |
| WELES-29 | Master unavailable means orchestration is down; no agent re-attach | accepted | weles | 3c | docs/reference/weles-design.md:450 (2026-07-16) | sections titled "Accepted" / "We take the other side of that trade" |
| WELES-30 | Fleet boot is sequential, recovery time is the sum of per-service healthy times | accepted | weles | 3c | docs/reference/weles-design.md:509 (2026-07-16) | design states the lever is parallel boot if it ever hurts |
| WELES-31 | Replicas:2 blockers: in-memory Epic OAuth state store and admin show-once reveal store | closed | weles | 3c | docs/reference/weles-design.md:821 (2026-07-21) | modules/accounts/src/epic_oauth.rs:313 redeems via oauth.take_state (DB); modules/admin/src/lib.rs:801-828 stashes the reveal in the store; no Mutex<HashMap> remains in either |
| WELES-32 | admin and gateway per-process rate limiters multiply by N under replicas | accepted | weles | 3c | docs/reference/weles-design.md:826 (2026-07-21) | "SOFT (non-correctness) rate-limit dilutions" in the same errata |
| WELES-33 | fleet.toml resolve="asks" schema question and DB-less fleet handling | closed | weles | 3c | docs/plans/2026-07-18-1048-weles-fleet-from-toml-plan.md:456 (2026-07-18) | weles/master/src/fleet_toml.rs:147-151,349-362 accept only "asks" and reject peers beside it; weles/src/pgfloor.rs:15-23 handles DB-less services |
| WELES-34 | Round-robin over N live instances (connection pool, selection, health) | closed | weles | 3c | docs/reference/weles-design.md:848 (2026-07-21) | remote::Pool landed (tools/verifyctl/src/stages/weles_managed_gateway.rs:773 cites C1/C2); not a gap at HEAD |
| WELES-35 | weles-fleet-parity residual gap (dropped gateway peer key undetectable) | closed | weles | 3c | docs/reference/weles-fleet-parity.md:112 (2026-07-18) | the stage was deleted (doc banner :3-17, step 5a of docs/plans/2026-07-18-1048-weles-fleet-from-toml-plan.md); fleet_toml::validate replaces it |
| WELES-36 | Dev/prod seam: no prod fleet flavor exists, a future one must omit the dev overlay | accepted | weles | 3c | docs/reference/weles-fleet-parity.md:122 (2026-07-15) | constraint note by design ("deliberately does NOT add one") |
| WELES-37 | weles plan/apply (Terraform-style diff of fleet.toml against the running fleet) | feature | weles | 3c | docs/reference/weles-design.md:316 (2026-07-10) | "not yet built"; the 2026-07-31 errata keeps fleet.toml as the truth; no tracker row |
| WELES-38 | supervisor.rs doc comment claimed observe/step never see a probe | closed | weles | 3c | docs/reference/weles-design.md:700 (2026-07-16) | weles/src/supervisor.rs:322 now says a probe reaches step through observe in WaitingHealthy |
| PROCESSCTL-01 | BUILD_ENV_ALLOWLIST lacks SYSTEMDRIVE/ProgramData | open | processctl | 3c | docs/status/2026-07-15-1120-weles-m0-pre-m1-backlog-status.md:92 (2026-07-15) | tools/processctl/src/fleet.rs:9-15 still lacks both; append_msvc_linker_path (fleet.rs:636-640) scans only ProgramFiles(x86); weles/src/prep.rs:16-19 still calls it a latent gap; b50c5537 (2026-07-12) predates the bisection in the status doc and is not the fix |
| PROCESSCTL-02 | macOS containment lacks PR_SET_PDEATHSIG and PR_SET_CHILD_SUBREAPER equivalents | accepted | processctl | 3c | docs/status/2026-07-17-2348-macos-port-complete-status.md:63 (2026-07-17) | "accepted not hidden"; [W2] forced_remainder reports forced_group only |
| PROCESSCTL-03 | No unit test for the Linux /proc pgrp parse in guardian.rs list_process_group | open | processctl | 3c | docs/status/2026-07-17-2348-macos-port-complete-status.md:74 (2026-07-17) | tools/processctl/src/guardian.rs:495 untested at HEAD (no test referencing a comm with spaces/parens); needs a Linux box |
| PROCESSCTL-04 | processctl tests.rs is gated cfg(all(test, windows)) so its Linux-only tests never compile | open | processctl | 3c | docs/plans/2026-07-14-1721-guardian-argv0-shim-fix-plan.md:198 (2026-07-14) | tools/processctl/src/lib.rs:68 still gates the module; no Linux guardian test module is compiled (guardian.rs:957-959 includes only guardian_darwin_tests.rs); an uncommitted plan exists in the working tree, nothing is committed |
| PROCESSCTL-05 | supported-targets check covers only the ring-free processctl and weles crates | accepted | processctl | 3c | docs/status/2026-07-17-2348-macos-port-complete-status.md:45 (2026-07-17) | cargo check --target for three triples by design |
| PROCESSCTL-06 | WorkspaceLayout ignores .cargo/config.toml build.target-dir | accepted | processctl | 3c | tools/processctl/src/layout.rs:24 (2026-07-13) | "# Known gap" doc section: only CARGO_TARGET_DIR and root/target are honoured |
| SPLITPROOF-01 | Splitproof [W2] forced_remainder not run as a Linux runtime assertion since the enumerate-before-reap change | open | splitproof | 3c | docs/status/2026-07-17-2348-macos-port-complete-status.md:71 (2026-07-17) | docs/plans/2026-07-17-1601-macos-rollout-port-plan.md:879; owner check on a Linux box (verifyctl --fast or splitproof) |
| SPLITPROOF-02 | status_or_err and player_burst text-sniff an error string | open | splitproof | 3c | tools/splitproof/src/main.rs:7109 (2026-07-11) | Err arm does e.to_string().contains(want); the audit plan says typed edge codes (step 16) allow re-pointing it (docs/plans/2026-07-13-1415-audit-remediation-plan.md:204) |
| SPLITPROOF-03 | [GR4e] and [DL2e] exist only in the split because the monolith hosts no internal edge | accepted | splitproof | 3c | tools/splitproof/src/main.rs:2500 (2026-09-10) | KNOWN GAP comments at :2500 and :2717: a structural limit of the parity pass, not faked in-process |
| SPLITPROOF-04 | Status::Invalid and Status::Internal are indistinguishable at the portal surface in [NT2c] | accepted | splitproof | 3c | tools/splitproof/src/main.rs:5611 (2026-08-31) | KNOWN GAP comment: the assertion pins the message and the absent row, not the 400-vs-500 split |
| SPLITPROOF-05 | Scripted split-proof (psql exit code ignored, /healthz instead of /readyz) and hand-derived port maps | closed | splitproof | 3c | docs/summaries/2026-07-10-1847-review-findings-verification-summary.md:29 (2026-07-10) | orchestration scripts replaced by processctl/devctl/verifyctl (docs/status/2026-07-13-1240-architecture-remediation-summary.md:12-16); splitproof runs the processctl fleet |
| ARCHCHECK-01 | No runtime DB-role isolation between module schemas (static foreign-schema SQL tripwire only) | accepted | archcheck | 3c | docs/reviews/2026-07-09-architecture-security-review.md:24 (2026-07-10) | finding 7 "deliberately deferred"; the archcheck foreign-schema SQL tripwire (58d02f6) covers accidental drift |
| ARCHCHECK-02 | archcheck did not constrain core/*, svc-hosts-module checked a Cargo dep only, topiccheck keyed by topic | closed | archcheck | 3c | docs/summaries/2026-07-10-1847-review-findings-verification-summary.md:36 (2026-07-10) | closed by 23ec41a/5e8ae52 (archcheck rule 9) and 89353e6/75d4042 (topiccheck (topic,version)) per docs/reviews/2026-07-13-2138-audit-remediation-status.md:24-25 |
| TOPICCHECK-01 | Contract-golden fingerprint blind spots: scalar collapse, first array element only, unsampled enum variants, default-sampled Option/Vec bodies | accepted | topiccheck | 3c | tools/topiccheck/src/golden.rs:64 (2026-07-13) | KNOWN GAPS by design in GOLDEN_HEADER; also tools/rpc-macro/src/lib.rs:296 and docs/plans/2026-07-13-1415-audit-remediation-plan.md:206-207 |
| TOPICCHECK-02 | admin-svc not modelled as planeless (review gap G4) | closed | topiccheck | 3c | docs/summaries/2026-07-10-1847-review-findings-verification-summary.md:39 (2026-07-10) | tools/topiccheck/src/main.rs:93-99: admin-svc is DB-backed since the admin hardening, so only gateway-svc is planeless; Grep finds no without_db in cmd/admin-svc/src/main.rs |
| CONFORMANCE-01 | A Validated { cap } policy number is not linked to the CapCase that enforces it | open | conformance | 3c | docs/plans/2026-09-10-1917-account-deletion-purge-plan.md:626 (2026-09-10) | docs/roadmap/feature-tracker.md:324; tools/conformance/src/policy.rs:72-79 still carry literal caps with prose bases; only checks::CREDENTIAL_CAPS binds |
| CONFORMANCE-02 | OPAQUE_REQUEST_TYPES matches the written bare name Identity, so a foreign type imported under it bypasses the inventory | open | conformance | 3c | docs/plans/2026-09-10-1917-account-deletion-purge-plan.md:635 (2026-09-10) | tools/conformance/src/input_inventory.rs:354-356 compares opaque.name == written |
| CONFORMANCE-03 | Stance::KnownGap and InputPolicy::KnownGap are a mechanism with no live entries | process | conformance | 3c | tools/conformance/src/model.rs:51 (2026-07-12) | names matching the sweep; Grep shows constructors only in main.rs/tests and none in tools/conformance/src/policy.rs, and --deny-gaps rejects any |
| CHECKMODULES-01 | Fleet lists hand-duplicated with no cross-check (review gap G3) | closed | checkmodules | 3c | docs/summaries/2026-07-10-1847-review-findings-verification-summary.md:38 (2026-07-10) | tools/checkmodules/src/tests.rs:12 split_fleet_matches_cmd_dirs compares the list to the cmd/*-svc directories; tools/processctl/src/fleet.rs:370 reports fleet drift; the shell scripts the finding cites are gone |
| EVENTCTL-01 | eventctl resume leaves consecutive_failures and last_error set (retry resets them) | accepted | eventctl | 3c | docs/research/2026-10-22-1830-admin-redesign-data-sources.md:194 (2026-10-22) | tools/eventctl/src/lib.rs:171-173 documents the resume/retry verb split as deliberate |
| EVENTCTL-02 | eventctl skip could rewind the checkpoint | closed | eventctl | 3c | docs/summaries/2026-07-10-1847-review-findings-verification-summary.md:13 (2026-07-10) | tools/eventctl/src/lib.rs:211-233 refuses skip on a row it did not lock-and-read |
| CSHARP-CLIENT-01 | C# fixture is RPC-only and does not exercise /push | open | csharp-client | 3c | docs/reference/push-hub.md:258 (2026-09-05) | docs/roadmap/feature-tracker.md:434 |
| CSHARP-CLIENT-02 | csharp-client stage SKIPs without dotnet or without libmsquic on Linux | accepted | csharp-client | 3c | docs/reference/csharp-client.md:186 (2026-07-08) | written to SKIP cleanly (exit 3); codegen-freshness has no such dependency |
| AGENT-TOOLING-01 | commit-msg hook is unchecked in a clone without the installed hook and under core.hooksPath overrides | accepted | agent-tooling | 3c | docs/reference/commit-format.md:77 (2026-10-23) | accepted in docs/plans/2026-10-23-1745-trackable-commit-format-plan.md:32 and :123; the remaining known gaps of commit-format.md:79-92 are AGENT-TOOLING-15..19 |
| AGENT-TOOLING-02 | git_safety_hook false denies and path-resolution gaps | open | agent-tooling | 3c | docs/plans/2026-10-21-2252-arcgame-workflow-port-plan.md:369 (2026-10-22) | git -C path resolves against the payload cwd, echo 'git' 'stash' denied, git reset <deleted-file> denied, branch-named file ambiguous, git checkout :/msg not a path, git reset --soft HEAD~ without a digit denied (.claude/hooks/tests/test_hooks.py:339), existing-path checkout denied when git rev-parse cannot run |
| AGENT-TOOLING-03 | git_safety_hook does not guard git rm/mv, aliases, git via sh -c or Invoke-Expression, or a lone & separator | open | agent-tooling | 3c | docs/plans/2026-10-21-2252-arcgame-workflow-port-plan.md:374 (2026-10-22) | also git checkout --track origin/x creates a branch silently (.claude/hooks/tests/test_hooks.py:343); owner chose one final fix and no third review round |
| AGENT-TOOLING-04 | A broken shell_data.py silently disables both shell guards | accepted | agent-tooling | 3c | docs/plans/2026-10-21-2252-arcgame-workflow-port-plan.md:376 (2026-10-22) | "fail-open by design" in the plan's Deferred list |
| AGENT-TOOLING-05 | test_known_gap_* pins in the hooks test file | process | agent-tooling | 3c | .claude/hooks/tests/test_hooks.py:339 (2026-10-22) | two tests pin the git reset --soft HEAD~ denial and the checkout --track silence; they restate the two git_safety_hook rows above, not separate items |
| AGENT-TOOLING-06 | No concurrent-rollout guard hook (deny a second cargo test, verifyctl or devctl up while cargo/rustc runs) | open | agent-tooling | 3c | docs/plans/2026-10-21-2252-arcgame-workflow-port-plan.md:384 (2026-10-22) | enforces a MANDATORY rule but was not requested |
| AGENT-TOOLING-07 | rust-nav LSP daemon, arcsem-style index and Codex .codex agent definitions | open | agent-tooling | 3c | docs/plans/2026-10-21-2252-arcgame-workflow-port-plan.md:386 (2026-10-22) | listed under the plan's Deferred section |
| AGENT-TOOLING-08 | About 19 .rs sites still say "CLAUDE.md hard constraint N" and need repointing to gamebackend.md | open | agent-tooling | 3c | docs/plans/2026-10-21-2252-arcgame-workflow-port-plan.md:380 (2026-10-22) | Grep lower bound outside docs/: tools/archcheck/src/main.rs 14, plus modules/gateway, modules/config, modules/match tests, core/remote, tools/topiccheck, tools/csharp-client-gen; the plan says about 40 including comments and scripts; repointing needs a cargo build |
| AGENT-TOOLING-09 | "Historical docs are archives" rule lives only in .claude/agents/docs-writer.md and memory | open | agent-tooling | 3c | docs/plans/2026-10-21-2252-arcgame-workflow-port-plan.md:378 (2026-10-22) | Grep of .agents/ for "archives" finds nothing; owner to decide whether it gets a core-rules.md home |
| AGENT-TOOLING-10 | Existing memory files that copy rules are left as they are | process | agent-tooling | 3c | docs/plans/2026-10-21-2252-arcgame-workflow-port-plan.md:387 (2026-10-22) | plan decision about how to treat memory files, not a work item |
| AGENT-TOOLING-11 | macOS governed-doc follow-up: CLAUDE.md and AGENTS.md said rollout tooling runs on Windows and Linux only | closed | agent-tooling | 3c | docs/status/2026-07-17-2348-macos-port-complete-status.md:76 (2026-07-17) | Grep for the phrase outside docs/ finds no match at HEAD |
| AGENT-TOOLING-12 | Dispatch and agent files told implementers to record "known gaps" | closed | agent-tooling | 3c | docs/plans/2026-10-23-2021-todo-tracking-plan.md:41 (2026-10-23) | replaced by Leftovers for owner in commit e6c6fc5e (todo-tracking step 1) |
| AGENT-TOOLING-13 | Stale public-api baseline items from the mail rollout and stale mail-svc / invalidation counts | closed | agent-tooling | 3c | docs/status/2026-09-05-1416-leaderboard-shared-table-pollution-status.md:82 (2026-09-05) | docs/reference/public-api-baseline/adminapi.txt:599,648 and schedulerevents.txt:6 exist, and mailevents.txt is present, under docs/reference/public-api-baseline/; .agents/shared/gamebackend.md:645 lists mail-svc; README.md:87 names core/invalidation; modules/ has 19 dirs = 18 domain modules + gateway, matching README.md:124 |
| AGENT-TOOLING-14 | sh/ps1 twin scripts stay hand-maintained | accepted | agent-tooling | 3c | docs/status/2026-07-12-0941-remediation-round4-summary.md:36 (2026-07-12) | shrunk to install, db-provision and memory-sync pairs (git ls-files) after the orchestration scripts were replaced by Rust tooling |
| AGENT-TOOLING-15 | commit-msg hook is bypassed by git aliases, nested shells (bash -c), git.exe and git global options the parser does not strip | open | agent-tooling | 3c | docs/reference/commit-format.md:79 (2026-10-23) | listed as "Known gaps (not checked)" (:79-80); not accepted by the trackable-commit-format plan |
| AGENT-TOOLING-16 | Exempt subject prefixes are prefix-only: anything after them is unchecked | open | agent-tooling | 3c | docs/reference/commit-format.md:81 (2026-10-23) | listed as a known gap |
| AGENT-TOOLING-17 | Plan slug lookup reads the working tree, so an untracked plan file also validates | open | agent-tooling | 3c | docs/reference/commit-format.md:82 (2026-10-23) | listed as a known gap |
| AGENT-TOOLING-18 | Commit cleanup is modelled only from GIT_EDITOR: explicit --cleanup= and commit.cleanup are invisible, and an exported GIT_EDITOR=: gets no-editor checking | open | agent-tooling | 3c | docs/reference/commit-format.md:83 (2026-10-23) | :83-88; fails closed in one direction and accepts a #-led subject git keeps in the other |
| AGENT-TOOLING-19 | Content after a scissors line is never checked (fail-open) | open | agent-tooling | 3c | docs/reference/commit-format.md:89 (2026-10-23) | :89-92; the hook always stops at the scissors line, git cuts there only under --cleanup=scissors or commit -v |
| RPC-MACRO-01 | Wire-only Default-sampled request bodies leave value kinds beyond the default unpinned | accepted | rpc-macro | 3c | tools/rpc-macro/src/lib.rs:296 (2026-07-13) | KNOWN GAP (by design) comment; event payloads close it with hand-populated samples |
| ROUTECHECK-01 | routecheck GATES is a hand-curated allowlist | accepted | routecheck | 3c | docs/status/2026-07-12-0941-remediation-round4-summary.md:36 (2026-07-12) | derivation rejected as disproportionate |
| REQUIRECHECK-01 | Requires issued outside init (start or a request handler) are invisible to requirecheck | accepted | requirecheck | 3c | docs/plans/2026-07-09-0839-architecture-enforcement-checkers-plan.md:49 (2026-07-09) | documented structural limitation; round-4 summary calls the drift accepted (docs/status/2026-07-12-0941-remediation-round4-summary.md:33) |
| EXPERIMENTS-01 | Quarkus/Kotlin JVM sketch plans and reference docs (gateway v1 limits, msquic probe remaining build, compile-time verification, test-coverage plan) | process | experiments | 3c | docs/reference/gateway.md:70 (2026-07-06) | describe experiments/jvm-quarkus-sketch and jvm-kotlin-sketch, not the Rust product; their deferred items (worker-pool dispatch, portal-shell admin, MsQuic transport, concurrency bugs) belong to those experiments |
| EXPERIMENTS-02 | Go-era statuses: gateway-svc double layer, accounts not in the proxy map, native-client QUIC auth, config live-delete | closed | experiments | 3c | docs/2026-07-07-2015-unified-operation-transport-status.md:107 (2026-07-07) | superseded by docs/2026-07-07-2145-gateway-svc-single-front-door-status.md:4 and by the Rust gateway; the QUIC front authenticates bearers (docs/2026-07-08-1429-rust-sketch-quic-player-front-status.md:61-63) |
| EXPERIMENTS-03 | Rust-sketch honest limits: dev session verifier, no player-port rate limiting, no admin fan-out | closed | experiments | 3c | docs/2026-07-08-1429-rust-sketch-quic-player-front-status.md:65 (2026-07-08) | real accounts sessions, player QUIC rate limits (docs/reference/hetzner-deploy-checklist.md:72) and the admin module landed; the dev-CA player certificate remains as the gateway row |

### Totals

| Class | Rows |
|---|--:|
| open | 102 |
| accepted | 68 |
| feature | 37 |
| process | 7 |
| closed | 47 |
| **total** | **261** |

Per topic:

| Topic | Group | open | accepted | feature | process | closed | rows |
|---|---|--:|--:|--:|--:|--:|--:|
| accounts | 3a | 5 | 2 | 8 | 0 | 1 | 16 |
| account-deletion | 3a | 3 | 2 | 0 | 0 | 0 | 5 |
| gateway | 3a | 6 | 1 | 0 | 0 | 2 | 9 |
| wallet | 3a | 1 | 3 | 0 | 0 | 2 | 6 |
| admin | 3a | 8 | 2 | 5 | 1 | 2 | 18 |
| apikeys | 3a | 2 | 3 | 1 | 0 | 2 | 8 |
| audit | 3a | 2 | 1 | 1 | 0 | 1 | 5 |
| notifications | 3a | 4 | 2 | 3 | 0 | 0 | 9 |
| mail | 3a | 3 | 2 | 2 | 0 | 0 | 7 |
| friends | 3b | 6 | 1 | 0 | 0 | 0 | 7 |
| groups | 3b | 8 | 2 | 0 | 0 | 1 | 11 |
| party | 3b | 5 | 1 | 0 | 0 | 0 | 6 |
| push | 3b | 5 | 4 | 1 | 0 | 0 | 10 |
| storage | 3b | 1 | 7 | 1 | 0 | 0 | 9 |
| leaderboard | 3b | 0 | 0 | 2 | 0 | 1 | 3 |
| match | 3b | 0 | 0 | 1 | 0 | 0 | 1 |
| inventory | 3b | 3 | 1 | 0 | 0 | 1 | 5 |
| characters | 3b | 1 | 0 | 0 | 0 | 1 | 2 |
| config | 3b | 3 | 0 | 2 | 0 | 1 | 6 |
| scheduler | 3b | 0 | 0 | 1 | 0 | 1 | 2 |
| event-plane | 3b | 1 | 8 | 0 | 0 | 4 | 13 |
| core-app | 3b | 1 | 0 | 0 | 0 | 2 | 3 |
| core-edge | 3b | 0 | 2 | 0 | 0 | 1 | 3 |
| core-lifecycle | 3b | 1 | 0 | 0 | 0 | 1 | 2 |
| core-remote | 3b | 2 | 0 | 0 | 0 | 0 | 2 |
| core-contrib | 3b | 0 | 0 | 0 | 0 | 1 | 1 |
| stats | 3b | 0 | 0 | 1 | 0 | 0 | 1 |
| store | 3b | 0 | 0 | 1 | 0 | 0 | 1 |
| realtime | 3b | 0 | 0 | 1 | 0 | 0 | 1 |
| chat | 3b | 0 | 0 | 1 | 0 | 0 | 1 |
| lore | 3b | 0 | 0 | 2 | 0 | 0 | 2 |
| weles | 3c | 12 | 9 | 3 | 2 | 12 | 38 |
| processctl | 3c | 3 | 3 | 0 | 0 | 0 | 6 |
| splitproof | 3c | 2 | 2 | 0 | 0 | 1 | 5 |
| archcheck | 3c | 0 | 1 | 0 | 0 | 1 | 2 |
| topiccheck | 3c | 0 | 1 | 0 | 0 | 1 | 2 |
| conformance | 3c | 2 | 0 | 0 | 1 | 0 | 3 |
| checkmodules | 3c | 0 | 0 | 0 | 0 | 1 | 1 |
| eventctl | 3c | 0 | 1 | 0 | 0 | 1 | 2 |
| csharp-client | 3c | 1 | 1 | 0 | 0 | 0 | 2 |
| agent-tooling | 3c | 11 | 3 | 0 | 2 | 3 | 19 |
| rpc-macro | 3c | 0 | 1 | 0 | 0 | 0 | 1 |
| routecheck | 3c | 0 | 1 | 0 | 0 | 0 | 1 |
| requirecheck | 3c | 0 | 1 | 0 | 0 | 0 | 1 |
| experiments | 3c | 0 | 0 | 0 | 1 | 2 | 3 |

### Topics per group

Topics with at least one `open` row, with the count of those rows. Topics the
inventory added are assigned here.

- **3a:** accounts (5), account-deletion (3), gateway (6), wallet (1), admin (8), apikeys (2), audit (2), notifications (4), mail (3)
- **3b:** friends (6), groups (8), party (5), push (5), storage (1), inventory (3), characters (1), config (3), event-plane (1), core-app (1), core-lifecycle (1), core-remote (2)
- **3c:** weles (12), processctl (3), splitproof (2), conformance (2), csharp-client (1), agent-tooling (11)

Topics with rows but no `open` row (no TODO file; `lore`, `stats`, `store`,
`realtime`, `chat` and `core-contrib` are new topic names assigned to 3b, the
first five for feature rows only; `rpc-macro`, `routecheck`, `requirecheck`,
`experiments` and `checkmodules` are new and assigned to 3c): leaderboard, match, scheduler, core-edge, core-contrib, stats, store, realtime, chat, lore, archcheck, topiccheck, checkmodules, eventctl, rpc-macro, routecheck, requirecheck, experiments.

### Reference-doc sections with `open` items (step 4 input)

- `docs/reference/event-plane-ops.md:69-80` — EVENT-PLANE-01.
- `docs/reference/weles-design.md` — :97-126 single-host known gaps (WELES-27),
  :840-847 replica path (WELES-20), :877-888 and :905 liveness and bind-before-prep
  (WELES-23, WELES-03).
- `docs/reference/module-reference.md:151-176` — "Do NOT copy" #2, #3 and #4
  (INVENTORY-03, INVENTORY-04, CHARACTERS-02). #3 also names the characters
  `LIST_HARD_LIMIT`, which CHARACTERS-01 records as closed.
- `docs/reference/push-hub.md` — :188 presence ordering (PUSH-03) and the Carried
  gaps list :258, :270, :271 (CSHARP-CLIENT-01, PUSH-04, PUSH-05).
- `docs/roadmap/feature-tracker.md` Notes cells and subsections: :140
  (ACCOUNTS-03), :145 (ACCOUNT-DELETION-01..03, ACCOUNTS-08), :146 (ACCOUNTS-09),
  :157-175 (GATEWAY-03), :177-193 (WALLET-01), :200 (FRIENDS-02, FRIENDS-03,
  FRIENDS-06), :201 (GROUPS-01, -02, -04..-07), :202 (FRIENDS-01), :204
  (PARTY-01..05), :223 (INVENTORY-01), :244 (NOTIFICATIONS-01), :246 (MAIL-02, -03,
  -05), :250 (CONFIG-02), :253 (AUDIT-05), :257 (ADMIN-01..04), :258-274
  (FRIENDS-06, GROUPS-09), :279-330 (STORAGE-09, CONFORMANCE-01, CONFORMANCE-02).
- `.agents/shared/gamebackend.md` :245, :389-392, :405, :436-437 (the others, :454
  and :481, only point at the tracker).
- `README.md:140-141` (ACCOUNT-DELETION-02, -03) and `README.md:187`, which names
  the tracker as the home of "current gaps".
- `docs/reference/commit-format.md:79-92` — the "Known gaps (not checked)" list
  after its first two entries (AGENT-TOOLING-15..19); the first two are
  AGENT-TOOLING-01, accepted.
- No `open` row remains in `docs/reference/hetzner-deploy-checklist.md` or
  `docs/reference/weles-fleet-parity.md`.

### Feature rows with no tracker row (step 4 input)

- WELES-25 Multi-machine weles (agent-master mTLS hop, own CA, master repoint, remote-peer cache, per-host port namespace, artifact distribution)
- WELES-26 Weles management API (read, imperative ops, fleet.toml CRUD, artifact upload; no UI of its own)
- WELES-37 weles plan/apply (Terraform-style diff of fleet.toml against the running fleet)
- ACCOUNTS-10 Account merge across identities
- ACCOUNTS-11 Google and Apple browser-redirect OAuth (PKCE, nonce, provider column on oauth_states)
- ACCOUNTS-12 MFA for player accounts
- ADMIN-07 Operator RBAC (fixed roles in v1, permissions declared by modules, portal-side check)
- ADMIN-08 Admin redesign: panel manifest/data split, parallel fan-out with deadlines, entity detail, search, typed primitives (G1-G12)
- ADMIN-09 Event-plane admin surface (subscriptions, pause/resume/retry/skip) with an undecided owner
- ADMIN-10 Admin pages for match, rating and leaderboard
- ADMIN-11 Mockup-driven backend features: suspend account, sessions list, active-players aggregate, wallet spent-24h, ledger across players
- SCHEDULER-01 Scheduler admin editing (create, edit, delete, fire now, recent fires)
- CONFIG-01 Config revision history view and per-key revision
- AUDIT-01 Audit filters by topic and actor, paging, export
- STORAGE-01 Cloud-save download archive from the admin
- MAIL-01 Mail queue pause, retry-now and park from the admin
- APIKEYS-03 API-key expiry, per-key rate limits, secret rotation
- NOTIFICATIONS-03 Per-player notification preferences and mute
- LORE-01 Hosted Lore as a Swarog module (adapter, event bridge, per-repo permissions, seat and storage reporting)
- LORE-02 player_id to user_id rename sweep

### Tracker rows that overclaim (step 4 input)

- `docs/roadmap/feature-tracker.md:200` "Friends (add/remove/block, states)" is
  marked shipped, but its own Notes say "no blocking in v1" (FRIENDS-02, `open`).
  Step 4 corrects the row title or status instead of only linking the TODO file.
