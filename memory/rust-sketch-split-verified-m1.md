---
name: rust-sketch-split-verified-m1
description: "A Rust port of the backend exists at experiments/rust-sketch/, reached split-verified M1 parity (foundations + gateway + durable messaging + characters/inventory); accounts/admin-portal/audit are M2"
metadata: 
  node_type: memory
  type: project
  originSessionId: df367cfa-2fb8-48f2-aac5-11559e2ce7f6
---

This is now the **migration beachhead**: as of 2026-07-08 the whole backend moves to Rust ([[decision-migrate-everything-to-rust]]), so rust-sketch is no longer just an experiment.

Rust port of the modular monolith lives at `experiments/rust-sketch/` (sibling to the JVM sketches — see [[gamebackend-north-star-and-jvm-exploration]], [[go-parity-additive-dual-deploy]]). Milestone 1 finished 2026-07-08 across 12 committed steps (`da75f50`..`1f2c76c`), and the **split microservices topology is verified live** (`split-proof.ps1`/`.sh` + `verify.ps1`): create-character in characters-svc (A) → starter item materializes in inventory-svc (B) over durable messaging; `list_character` in B authorizes via `owner_of` over QUIC/mTLS to A; delete wipes holdings via event (no FK). Plan: `docs/plans/2026-07-08-0937-rust-sketch-foundations-two-modules-plan.md`.

**Rust-specific design decisions (differ from Go, intentional):** registry keys are capability-scoped `"<module>.<cap>"` (nominal traits can't multiplex one `dyn Any`) — the rule-4 relaxation from [[separate-public-surface-from-impl]]; consumers import the provider's `*api` crate for the trait; durable bus handlers are a named `TxHandler` trait over `&mut PgConnection` (async-borrow closures don't infer); identity is an explicit leading `opsapi::Identity` param (no ambient ctx); `#[rpc]` proc-macro replaces `tools/rpcgen`; edge codec is JSON over quinn+rustls (ring), NOT MessagePack.

**M1 kept gateway + RPC codegen at user insistence** ([[dont-descope-transport-for-simplicity]]). **Deferred to M2:** accounts (dev SessionVerifier stands in), admin portal (modules still contribute adminapi.Item), audit. Don't assert full parity with the Go backend yet — only these two modules + the transport substrate are ported.

**QUIC player front + gateway-svc landed 2026-07-08** (plan `docs/plans/2026-07-08-1330-rust-sketch-quic-player-front-plan.md`, status `docs/2026-07-08-1429-rust-sketch-quic-player-front-status.md`, commits `7fe9035`..`78d44f6`): dedicated `cmd/gateway-svc` single front door — player QUIC `:9100` (server-cert-only TLS, ALPN `edge-player`, token-in-envelope verified at the front, route-table allow-list) + HTTP `:8082`, stub-only, no DB; inventory-svc now serves edge `:9001`; monolith serves the player front too (parity). **Edge-server rule: a process runs an edge server ⇔ it hosts a provider some peer calls synchronously.** This closes the `:9100`-unauthenticated gap Go left open ([[unified-operation-transport]]). Proven live by extended `split-proof.sh` (P1–P5 + monolith stage).
