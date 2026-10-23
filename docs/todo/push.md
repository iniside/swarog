# Push

Last updated: 2026-10-23

## In progress

_None._

## Open

- [ ] **Target::Players for multi-recipient pushes**
  PUSH-01. Party pushes one `Target::Player` per member
  (`modules/party/src/service.rs:208`); a multi-player target changes the
  backplane `Envelope`, which needs a fronts-before-producers rollout
  (docs/plans/2026-09-28-2212-push-multiplex-invoke-and-party-plan.md:613,
  2026-10-05; Grep lower bound found no `Target::Players`).
- [ ] **Gateway-global presence view**
  PUSH-02. Presence is front-local and default off (`PUSH_PRESENCE`); a shared
  view needs a friends graph for the audience and persistence
  (docs/reference/push-hub.md:182, 2026-09-05). Item-level half of the partial
  tracker row at docs/roadmap/feature-tracker.md:202 (no fan-in across gateway
  instances).
- [ ] **Sequence numbers for presence announcements**
  PUSH-03. `online`/`offline` announcements are enqueued independently and can
  reach an observer in either order; the frames carry no sequence number or
  timestamp. Needs presence as sequenced state
  (docs/reference/push-hub.md:188-192, 2026-09-05).
- [ ] **Fuzz target for the /push frame parser**
  PUSH-04. `tools/verifyctl/src/stages/fuzz.rs:22` runs `frame_decode` and
  `wire_decode` only (docs/reference/push-hub.md:270, 2026-10-03; read at HEAD).
- [ ] **routecheck coverage of the fixed /push route**
  PUSH-05. `/push` is a real axum route added ahead of the fallback
  (`modules/gateway/src/lib.rs:796-797`), outside routecheck's `#[http]` model,
  so a future op at the same path would shadow it with no gate noticing
  (docs/reference/push-hub.md:271, 2026-10-03).
- [ ] **Lost 1009 close after frame_too_large**
  PUSH-08. The protocol-level 1009 close can be lost (the peer may see 1006);
  the typed JSON `close` frame still arrives. Listed under Carried gaps with no
  owner decision accepting it (docs/reference/push-hub.md:268-269, 2026-10-03).
