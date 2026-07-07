# Memory Index

- [Memory is git-backed via repo mirror](memory-git-backup-workflow.md) — run scripts/memory-sync.sh push after any memory change, pull after git sync; repo memory/ mirrors the per-machine live dir
- [Verify the at-risk path, not the safe one](verify-the-at-risk-path-not-the-safe-one.md) — exercise the topology a change actually affects (split, not just monolith) with a committed repeatable proof; don't pass off easy-path testing as coverage
- [Work on master, no branches](work-on-master-no-branches.md) — commit directly on master for this repo; overrides CLAUDE.md's "branch-first" (solo for-fun project, history is enough)

- [DECISION: Go vs Kotlin (in flux)](decision-quarkus-as-final.md) — back UNSETTLED (2026-07-07), now leaning GO again (admin list-free via runtime slot + linker split-build); 2026-07-05 "Quarkus final" downgraded — don't assert either as settled
- [Local Postgres is the test DB](local-postgres-is-the-test-db.md) — integration tests target the local Postgres directly; NOT a Docker/Testcontainers fallback, don't frame it as one
- [Local Postgres](reference_local_postgres.md) — role/db/DSN for GameBackend + superuser password (postgres/qwerty)
- [Store-launch auth deferred to SDK](store-launch-auth-deferred-to-sdk.md) — backend stays a verifier; web=EAS(account id), native/store=Connect(PUID) needs a 2nd verifier later
- [North star + JVM exploration](gamebackend-north-star-and-jvm-exploration.md) — two goals (pluggable + extractable to microservices); framework-free Kotlin/JDK26 port at experiments/jvm-kotlin-sketch/
- [Follow UILayout mockup faithfully](follow-uilayout-mockup-faithfully.md) — UI has an exact spec in UILayout/*.dc.html; translate 1:1, don't improvise visuals
- [Async fanout / broker-less](async-fanout-sync-grpc-brokerless.md) — events are fanout-only via outbox+HTTP POST (no broker); log/order/buffering ⇒ sync. Verified in jvm-quarkus-sketch (Option E)
- [Edge QUIC via msquic/FFM](edge-quic-msquic.md) — player-edge transport = QUIC (msquic bound with JDK 26 Panama/FFM, MessagePack, no protobuf); proven by replacing the internal gRPC ownerOf seam
- [Never monolith-only features](never-monolith-only-features.md) — split to wspierana ścieżka kompilacji; ficzer MUSI działać w obu topologiach (wspólny Postgres, per-job advisory-lock, rejestracja w każdym procesie) — nie odkładaj split jako „przyszłość"
- [Prometheus adopted (Tier-1)](prometheus-adopted-tier1.md) — observability używa prometheus/client_golang (2026-07-07); odwraca wcześniejsze „odroczone", nie asertuj już że pominięte
- [Go parity additive dual-deploy](go-parity-additive-dual-deploy.md) — Go backend reached full dual-deploy parity with the Quarkus sketch additively (core/ untouched), QUIC via native quic-go (no FFM); branch go-parity, verified live
