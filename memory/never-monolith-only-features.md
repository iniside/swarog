---
name: never-monolith-only-features
description: "nigdy nie projektuj ficzerów jako monolit-only — split to wspierana ścieżka kompilacji, feature musi działać w OBU"
metadata: 
  node_type: memory
  type: feedback
  originSessionId: 04330268-1c09-44b3-a968-56b350eb9ba4
---

Projektując nowy ficzer w GameBackend, NIGDY nie rób go „monolit-only" ani nie odkładaj
działania w split jako „przyszłość". Split-mikroserwisy (`cmd/*-svc` + gateway) to
**wspierana, kompilowalna ścieżka** — feature działający tylko w monolicie łamie north-star
dual-deploy ([[go-parity-additive-dual-deploy]], „extractable to microservices").

**Why:** user złapał mnie na tym (2026-07-07: „znowu odpierdalasz jaki monolit only") przy
planie Tier-1 — zaprojektowałem scheduler i audit jako monolit-only, bo bus jest per-proces.
To była linia najmniejszego oporu, nie poprawny projekt. „znowu" = wzorzec, nie jednorazówka.

**How to apply:** zanim uznasz że coś „nie da się w split", wykorzystaj że split **dzieli
jeden wspólny Postgres** i ma outbox/edge:
- stan globalny → współdzielony schemat w tej samej bazie (wiele procesów, jedna tabela,
  migracja idempotentna) — tak audit staje się globalny bez plumbingu;
- „zrób raz globalnie" → `pg_try_advisory_lock` per-jednostka-pracy (per-job), nie długożyjący
  lider — działa multi-proces i multi-replika;
- rejestruj moduł w KAŻDYM procesie modułowym (`cmd/server` + `cmd/*-svc`), nie tylko w monolicie.
Jeśli coś naprawdę wymaga cross-process eventów → outbox sink (jak `inventory` `/events/*`),
nie odkładaj tego jako wymówki na monolit-only.
