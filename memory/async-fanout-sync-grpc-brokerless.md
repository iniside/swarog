---
name: async-fanout-sync-grpc-brokerless
description: "GameBackend cross-process comms — async events are fanout-only (broker-less HTTP), anything needing log/order/buffering is sync gRPC"
metadata: 
  node_type: memory
  type: project
  originSessionId: 2dde7081-732d-49f5-b0aa-ce19637ba5f1
---

Design decision for the GameBackend split (verified in `experiments/jvm-quarkus-sketch`, Option E):

- **Async events = fire-and-forget FANOUT ONLY.** If something needs a log, buffering, ordering, or
  delivery guarantees beyond eventual, it is modeled as a **synchronous gRPC call**, not an async event.
- **No message broker.** Cross-process async delivery is broker-less: the transactional **outbox** (per-module
  table) gives durability + retry, and a `@Scheduled` relay POSTs each unsent row's JSON straight to the
  subscriber's HTTP endpoint (`POST /events/<x>`), marking sent on 2xx. Consumer endpoints are idempotent via an
  **inbox** table. Fanout target is env-driven (`INVENTORY_ADDR`): self in monolith, peer process in split.
- **Sync capability was gRPC, now edge/QUIC** (e.g. `PlayerCharacters.ownerOf`); **admin composition = REST
  fan-out** (Stork). gRPC was a PoC and has been **fully deleted** — the sync seam runs over the `edge` RPC core
  (MessagePack) on real QUIC via msquic/FFM (see [[edge-quic-msquic]] / repo `docs/reference/edge-gateway-quic.md`
  + `msquic-ffm-probe.md`). Matches Pragma's "JVM + DB only, no message queues" footprint (see
  [[gamebackend-north-star-and-jvm-exploration]]).

Why not SmallRye/Kafka: a single artifact (all modules on the classpath, ROLES selects active ones) can't
share one SmallRye channel between the relay Emitter and the `@Incoming` consumer — split boot fails with
SRMSG00073. Broker-less fanout sidesteps it entirely and stays verifiable without Docker.
