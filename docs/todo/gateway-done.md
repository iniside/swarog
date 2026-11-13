# Gateway

Last updated: 2026-11-13

- [x] **Production WebPKI certificate for the player QUIC port** (2026-11-13, player-quic-production-tls step 7)
  GATEWAY-07. `core/app` `resolve_player_tls` (core/app/src/lib.rs:1580) picks the player
  QUIC certificate once at boot in both topologies: `PLAYER_TLS_CERT_PATH` +
  `PLAYER_TLS_KEY_PATH`, else the HTTPS front's `files` certificate or its one shared ACME
  resolver, else a dev-CA leaf with a boot warning (412a2e8a, 5121cefa); clients dial by
  DNS name with system roots (175ac725 Rust, 3a0bff7a C#). Source:
  docs/plans/2026-07-08-1330-rust-sketch-quic-player-front-plan.md:365 (2026-07-08);
  plan-slug player-quic-production-tls step 7.
