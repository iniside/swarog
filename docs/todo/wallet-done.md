# Wallet

Last updated: 2026-11-17

- [x] **Share a lock between the purge sweep and the movement guard** (2026-11-17, security-quick-wins step 2)
  WALLET-01. `Service::apply_on` and the sweep's per-player purge both take
  `Store::lock_player_tx` (a `wallet.player/`-namespaced `pg_advisory_xact_lock`) before
  the tombstone guard or the purge (39e2f802); a fire claims and locks at most
  `WALLET_PLAYERS_PER_FIRE` = 64 players. Race and fire-bound tests: b6108348, 61855ddd,
  51fb57aa.
