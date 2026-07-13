---
name: never-monolith-only-features
description: "nigdy nie projektuj ficzerów jako monolit-only — split to wspierana ścieżka kompilacji, feature MUSI działać w obu (repeat-offense correction)"
metadata: 
  node_type: memory
  type: feedback
  originSessionId: 88cdd953-b406-40a0-8ab2-6c7eb07acece
---

NIGDY nie rób ficzera „monolit-only" ani nie odkładaj działania w split jako „przyszłość".
Split (`cmd/*-svc` + gateway) to wspierana, kompilowalna ścieżka — feature działający tylko w
monolicie łamie north-star dual-deploy. Mechanika „jak zrobić coś w split" (wspólny Postgres,
per-job `pg_try_advisory_lock`, rejestracja modułu w KAŻDYM procesie) jest już w CLAUDE.md
("Adding a module", scheduler, hard-constraint #5) — ta pamięć trzyma tylko korektę:

**Why:** user złapał mnie na tym (2026-07-07: „znowu odpierdalasz jaki monolit only") przy
planie Tier-1 — zaprojektowałem scheduler i audit jako monolit-only bo bus jest per-proces.
To była linia najmniejszego oporu, nie poprawny projekt. **„znowu" = wzorzec, nie
jednorazówka** — dlatego zostaje jako feedback. Related:
[[dont-descope-transport-for-simplicity]].
