# Account deletion

Last updated: 2026-11-18

- [x] **Orphan-row race between the tombstone guard and the purge sweep** (2026-11-18, purge-race-locks step 6)
  ACCOUNT-DELETION-05. The owner reversed the accepted "Known gap ... Not taken" decision
  (docs/plans/2026-09-10-1917-account-deletion-purge-plan.md:437-460) on 2026-11-17. Every
  guarded writer now takes a per-player advisory lock before its tombstone read, and every
  purge sweep claims at most 64 players per fire and takes the same lock per player before it
  purges: wallet (39e2f802), storage, characters (dd789cc2), friends (8622b65d),
  notifications (a228ccbd), inventory (424f00b1) and groups. In groups, `create`, `join` and
  `invite` take the player lock before the group lock, and the sweep takes every claimed
  player's lock before its first group lock; `invite` answers a purged target with the
  directory-miss `NotFound`.
