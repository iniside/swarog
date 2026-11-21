# Wallet

Last updated: 2026-11-21

## In progress

_None._

## Open

- [ ] **Wallet search provider in the admin palette**
  The mockup registers a wallet search provider (`UILayout/redesign/assets/registry.js:41`);
  Step 15 of the admin redesign does not port it (owner, 2026-11-20, O-5 in
  `docs/research/2026-11-20-2230-admin-ports-contract-gaps.md`).

### Known limits (admin-control-plane-redesign step 15)

- [ ] **The Economy Currencies panel scans every balance on each render**
  Supply (`sum(amount)`) and Holders (`count(*)` above zero) group `wallet.balances` by
  `currency`, and no index leads with `currency` (the PK is `(player_id, currency)`), so
  `Store::currency_stats` (`modules/wallet/src/store.rs`) reads the whole table each time
  the page draws. A `balances (currency)` index, or a maintained per-currency total, ends it.
  A panel deadline drops the answer, not the statement.
- [ ] **The Economy Ledger scans the whole ledger on each render**
  Without a search, `Store::ledger` (`modules/wallet/src/store.rs`) orders every movement
  by `seq` and `Store::ledger_count` runs an exact `count(*)` over `wallet.ledger`; the
  only index is `ledger_player_seq_idx (player_id, seq DESC)`, so neither `ORDER BY seq` nor
  a time window on `at` (the 15a5 Embers-spent tile) has an index. `ledger (seq)` and
  `ledger (at)` indexes end the scans; the count stays exact unless the foot takes an
  estimate. A panel deadline drops the answer, not the statement.

- [ ] **No test sends an uppercase or unhyphenated player uuid to the existence check**
  `Service::player_exists` canonicalises the id (`modules/wallet/src/service.rs:165-171`)
  because accounts' batch lookup drops non-canonical spellings. The padded-uuid test is
  trimmed in `invoke` before that, and `FakeDirectory` (`modules/wallet/src/tests.rs:63-71`)
  matches exactly, so a broken canonicalisation would pass every test.
- [ ] **A player deleted in accounts but not yet tombstoned in wallet reads "No such player."**
  The directory omits a deleted player, so the portal says "No such player." instead of
  "Player has been deleted." until wallet's `player.deleted` tombstone lands. Nothing is
  written either way; wording only.
- [ ] **The admin key conformance probe covers one call site and one direction**
  `conformance_admin_key_rejected` (`modules/wallet/src/conformance.rs`) reaches the key
  check through the grant path only, so it proves the `service.rs:364` check and not the
  one at `service.rs:276`, and the revoke direction is never probed.
