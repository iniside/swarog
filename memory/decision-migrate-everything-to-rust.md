---
name: decision-migrate-everything-to-rust
description: "DECIDED 2026-07-08 — the whole backend migrates to Rust; supersedes the Quarkus/Go deliberation. Don't re-litigate or assert Go/Quarkus as the target."
metadata: 
  node_type: memory
  type: project
  originSessionId: df367cfa-2fb8-48f2-aac5-11559e2ce7f6
---

**DECIDED 2026-07-08: everything moves to Rust.** This supersedes the long Quarkus -> Go -> "in flux" deliberation (the old `decision-quarkus-as-final` memory is deleted). Do NOT re-open Go-vs-Rust or assert Quarkus/Go as the target anymore.

**Why:** the Rust sketch reached split-verified M1 ([[rust-sketch-split-verified-m1]]) — foundations + durable messaging + gateway + characters/inventory, proven live in the two-process topology. The post-port assessment: for this project's profile (split-from-start, agent-written, correctness over iteration speed) Rust is equal-or-better. The rule-4 cost (nominal traits -> consumers import the provider's contract crate) turned out bounded and clean, not pervasive; iteration speed (the old pro-Go argument) was a non-issue (~7s full build); and crate-per-module gives compiler-enforced physical separation that serves the "extractable to microservices" north star ([[gamebackend-north-star-and-jvm-exploration]]) better than Go's lint-enforced boundaries.

**Scope:** "everything" — the Go backend and the JVM sketches converge on Rust. Keep the split-first, no-hacks discipline ([[dont-descope-transport-for-simplicity]], [[never-monolith-only-features]]) and the honest api/<domain>/{api,events,rpc} layout.

**STATUS: DONE 2026-07-08** — the full port completed the same day ([[rust-sketch-split-verified-m1]]): fortress refactor + accounts/admin/audit/scheduler/match/rating/leaderboard/webui + metrics/httpmw + tiered verify net. The Rust workspace at repo root is the only developed project; `experiments/go-sketch/` is an archived reference (owner chose to keep it, not delete).
