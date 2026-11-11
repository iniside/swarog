# Gateway

Last updated: 2026-11-11

## In progress

_None._

## Open

- [ ] **`DevSessionVerifier` mints non-uuid player ids**
  GATEWAY-03. modules/gateway/src/verifier.rs:46-48 returns the `dev-` token suffix
  verbatim. Nine store sites fold a non-uuid id into a green empty result:
  modules/wallet/src/store.rs:403, modules/notifications/src/store.rs:98,287,
  modules/friends/src/store.rs:412,443,475, modules/groups/src/store.rs:498,544.
  Candidate closure: a deterministic uuid. Source: docs/roadmap/feature-tracker.md:157 @ fd797b3a
  (2026-09-12).
- [ ] **Describe validation checks the method-id dimension only**
  GATEWAY-04. Verb, path and auth of an advertised `OpManifest` are trusted
  verbatim, so a buggy peer can claim another provider's URL path unauthenticated.
  Needs a path-ownership rule. Source: modules/gateway/src/lib.rs:1622 (2026-07-28).
- [ ] **Repeated describe fetch-task failures are invisible to `/readyz`**
  GATEWAY-05. Keep-last hides a broken pass behind a green readiness; only a log
  records it. Source: modules/gateway/src/lib.rs:1782 (2026-07-28).
- [ ] **Test that `POOL_STOP_BUDGET` fan-out is concurrent**
  GATEWAY-06. Needs `remote::Pool::with_factory` (core/remote/src/lib.rs:804, private)
  reachable from gateway tests. Source: modules/gateway/src/tests.rs:2477 (2026-07-28).
- [ ] **Production WebPKI certificate for the player QUIC port**
  GATEWAY-07. core/app/src/lib.rs:1020-1030 binds the player front with
  `server_tls_public` derived from the shared dev CA; `TLS_MODE=acme` covers HTTP only
  (docs/reference/hetzner-deploy-checklist.md:21). Source:
  docs/plans/2026-07-08-1330-rust-sketch-quic-player-front-plan.md:365 (2026-07-08).
  Owner priority (2026-11-11): next after the docker-compose monolith work — needed to
  expose players publicly in both topologies (monolith and gateway-svc).

### Deferred

- [ ] **Gateway session-verification cache**
  GATEWAY-08. No cache in modules/gateway/src/verifier.rs. A cache interacts with
  refresh-family revocation and needs its own reviewed change. Source:
  docs/plans/2026-07-30-2230-accounts-federated-providers-plan.md:107 (2026-07-30).
