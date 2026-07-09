---
name: rust-sketch-split-verified-m1
description: "The Rust backend at repo root is COMPLETE — full Go port done 2026-07-08 (Steps 1–15, fortress refactor + all 8 remaining modules + infra + verify net); go-sketch archived as reference"
metadata: 
  node_type: memory
  type: project
  originSessionId: df367cfa-2fb8-48f2-aac5-11559e2ce7f6
---

**The Rust backend at repo root is the only developed project — the full Go port completed 2026-07-08** ([[decision-migrate-everything-to-rust]]), via `docs/plans/2026-07-08-1517-go-to-rust-full-port-plan.md` (15 steps, commits `bf7f049`..`7cf4e3e` + CLAUDE.md rewrite). `experiments/go-sketch/` stays as an archived reference at the owner's choice (NOT deleted — Step 15 was reduced to the CLAUDE.md rewrite + memory update).

**What landed on top of M1** — Phase 0 "fortress refactor": tests out of lib.rs into `src/tests.rs`; `<name>api` (pure) split from `<name>rpc` (glue) via a meta-callback `macro_rules!` handoff (`<prefix>_<snake>_meta!(rpc_macro::generate_glue)`); `edge::EDGE_SLOT` killed the `Option<edge::Server>` topology leak; asyncevents (durable events plane)+remote moved to `core/` (generic `Stub::new(provider, addr, factories)`); config-svc + durable `config.changed` + `tools/archcheck` + `fortress` verify stage. Phase 1: accounts (argon2id, Epic OIDC/OAuth, REAL gateway session verification — dev verifier only with explicit `ACCOUNTS_DEV_AUTH=1`, else fail-loud), admin portal (minijinja, QUIC admin fan-out `admin.adminData`), audit, scheduler (advisory-lock fire), match/rating/leaderboard, webui (sanctioned monolith-only exception). Phase 2: `core/metrics` (private Prometheus registry, MatchedPath labels), `core/httpmw` (per-IP token bucket, right-to-left XFF trusted-proxy walk, readiness slot). Phase 3: verify tiering (fast/all/slow/strict; cargo-audit blocking; public-api additive guard; proptest outbox/edge; cargo-fuzz targets; cargo-mutants), `tools/topiccheck` (runtime harness — recording transport diffs defined-vs-subscribed topics; chosen over linkme as more honest).

**Key architecture rules now codified in CLAUDE.md** (read that first — this memory is the pointer, not the spec): fortress rule (every domain module its own `cmd/<name>-svc`, archcheck-enforced), ALL cross-module events durable (`emit_tx`/`on_tx`; plain emit is in-process only), modules topology-blind (EDGE_SLOT + registry swap).

**End state verified live:** 232 tests, clippy `-D warnings` clean, `verify.sh --all` PASS, `split-proof` on **11 processes + monolith parity** (ports: HTTP 8080–8090, edge 9000–9008, player-QUIC 9100).
