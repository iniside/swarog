---
name: purge-race-is-a-bug-not-accepted
description: "Owner 2026-11-17 reversed the 2026-09-10 \"Known gap, not taken\" — the tombstone-guard vs purge-sweep race (rows outliving account erasure) is a serious bug to FIX in every module, never an accepted gap"
metadata:
  node_type: memory
  type: project
  originSessionId: fb0529fe-4b60-43b5-a11a-9d60ec9ddba8
  modified: 2026-11-17T09:53:06.490Z
---

Owner 2026-11-17: "wyscigow nie zakceptujemy. to akurat powazny blad". The orphan-row race between a writer's tombstone guard and the purge sweep (`docs/plans/2026-09-10-1917-account-deletion-purge-plan.md:437-460`, filed as ACCOUNT-DELETION-05 "accepted") is reversed: it must be fixed in characters, friends, notifications, inventory, groups — the same way wallet (39e2f802) and storage do it: one per-player `pg_advisory_xact_lock` taken by every guarded writer before the tombstone read and by the sweep before purging.

**Why:** a row that survives erasure is a GDPR/erasure correctness bug, not a cosmetic leftover — "recoverable by the counterparty" does not make it acceptable.

**How to apply:** never file an erasure/purge leak as accepted or as a Note; never propose "accept the race window" as an option. When a plan finds such a race, it is open work. Before filing any todo, check the todo-tracking inventory for an existing ID and decision (I reopened it once without citing the old decision, then refiled it as accepted — both wrong). Related: [[always-fix-at-the-source]].
