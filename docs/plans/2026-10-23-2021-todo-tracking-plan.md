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
