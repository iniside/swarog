# Wallet

Last updated: 2026-10-23

## In progress

_None._

## Open

- [ ] **Share a lock between the purge sweep and the movement guard**
  WALLET-01. modules/wallet has no advisory lock (modules/wallet/src/store.rs:157,233
  only comment on it), so money can reappear after erasure. Storage closes the same
  race: modules/storage/src/service.rs:237, modules/storage/src/projection.rs:61.
  Source: docs/roadmap/feature-tracker.md:177 @ fd797b3a (2026-09-12).
