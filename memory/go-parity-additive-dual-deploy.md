---
name: go-parity-additive-dual-deploy
description: "Historical: the retired Go backend reached full dual-deploy parity additively (core/ untouched) before the Rust migration — archived, superseded, no forward action"
metadata: 
  node_type: memory
  type: project
  originSessionId: 88cdd953-b406-40a0-8ab2-6c7eb07acece
---

Historical pointer only. The retired Go backend reached full monolith+split dual-deploy
parity **additively** — `core/` was never edited across all steps; the machinery (ROLES env,
`remote.Stub` per unhosted dep, per-schema outbox+relay+sink, admin fan-out, a `gateway/`
front door over native `quic-go`) lived entirely in new packages + composition-root wiring,
because the seams already existed. Verified live.

Superseded by the Rust migration ([[decision-migrate-everything-to-rust]]); Go is archived at
`experiments/go-sketch/` ("do not evolve"). Kept only as evidence the additive dual-deploy
pattern works — the current Rust repo realizes the same shape. No forward action.
