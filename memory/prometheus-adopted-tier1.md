---
name: prometheus-adopted-tier1
description: "prometheus/client_golang przyjęty do observability (2026-07-07), odwraca wcześniejsze \"odroczone\""
metadata: 
  node_type: memory
  type: project
  originSessionId: 04330268-1c09-44b3-a968-56b350eb9ba4
---

Decyzja użytkownika (2026-07-07): observability w GameBackend używa
`github.com/prometheus/client_golang` — `/metrics` + HTTP-middleware (counter+histogram,
label `method`/`r.Pattern`/`status`), NIE OpenTelemetry (nadmiar dla jednego procesu).

**Why:** to odwraca wcześniejszą decyzję "prometheus odroczony" z
`docs/2026-07-05-2000-go-parity-status.md`. Przy okazji Tier-1 infra (rate-limit /
observability / scheduler / audit) user świadomie ją cofnął — prometheus jest de-facto
standardem dla monolitu Go.

**How to apply:** nie asertuj już że "prometheus jest odroczony/poza zakresem". Metryki
są in-scope. Plan: [[go-parity-additive-dual-deploy]]. Szczegóły w
`docs/plans/2026-07-07-1309-tier1-infra-plan.md` (Step 3). Rate-limit/observability to
warstwa transportu w boot-layerze (nie moduły); scheduler/audit to moduły monolit-only.
