---
paths:
  - "docs/todo/**"
  - "docs/plans/**"
  - "docs/roadmap/**"
---

# TODO, plan and status doc format

File locations and the leftover rule: `.agents/shared/core-rules.md` **Plans And
Status Docs**.

## TODO lists

- A TODO list is a set of `- [ ]` task items; a plan's migration inventory
  table is a record, not a TODO list. Feature-level status lives in
  `docs/roadmap/feature-tracker.md`; a feature first found during work becomes
  a tracker row with `(added YYYY-MM-DD)`.
- Every topic is a pair: `<kebab-topic>.md` (open work) and
  `<kebab-topic>-done.md` (closed work, empty until the first item closes), no
  date in the name. Topics are module/crate kebab names (`accounts`, `wallet`,
  `core-edge`, `splitproof`); cross-cutting topics use a name
  (`account-deletion`, `event-plane`, `agent-tooling`). Both files have a
  `Last updated: YYYY-MM-DD` line under the title, changed on every edit.
- Before creating a topic, list `docs/todo/` and add to a matching file.

### Open file

These sections, in this order, and nothing else:

- `## In progress` — someone started it and it is not finished; parked or
  merely partial/unverified items are not in progress.
- `## Owner checks` (optional) — things only the owner can verify by hand (live
  fleet, browser, client), each naming what to check and where; no `###`.
- `## Open` — not started; may hold `###` area subsections, with postponed work
  in a last `### Deferred`.
- `## Notes` (optional) — background, design context, links, decisions,
  measured facts, rejected alternatives and non-goals under `### Not doing`;
  never a task.

Every task is a `- [ ] **Short title**` line (a short noun phrase) with detail
indented under it. Empty `In progress`/`Open` keep a `_None._` line.

Leftovers the owner chose to defer go under `### Known limits (<plan-slug> step
N)` in `## Open`; `<plan-slug>` is the plan file name without the date and
`-plan.md`. Leftovers with no plan step use `### Known limits (<area>)`.

### Done file

`- [x] **Short title** (YYYY-MM-DD, <ref>)` items, newest first, detail
indented. `<ref>` is exactly one of `<plan-slug> step N`, a commit sha (7-12
hex), or `no ref`; an unknown date is `date unknown`. Nothing closed: a single
`_None._` line.

### What is an item

A real gap in a feature or a real bug, in the product or tool, that someone
must fix later. Session and process notes are never items (a fleet not
restarted, a build/test/review/measurement not run in this session, a commit
that took someone else's file): say those in the report.

### Lifecycle

- The implementer lists leftovers under **Leftovers for owner**; the owner
  decides per item; the lead fixes the "fix now" items in the same task and
  writes only the deferred ones to the TODO file.
- Starting an item moves it to `In progress` (the lead, before dispatching the
  step). Finishing it moves it to the done file with the date and the plan step
  or commit (the step's implementer, in the step commit).
- Before committing a plan step or named fix, re-check the open items of the
  TODO topic for the area you changed (Known limits included) and move every
  item the change fixed or made obsolete to the done file in the same commit;
  leave items you cannot prove closed untouched.

## Plans

When a plan is finished but some steps have no step commit, add a
`Status: done` line under the plan title. When a plan or its finished work
waits for the owner's approval, the line is `Status: waiting for approval`
instead, replaced once the owner decides.
