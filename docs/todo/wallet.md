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
  a time window on `at` (`Store::spent_last_day`, the dashboard's 24 h spent tile, which
  scans every ledger row on each dashboard render) has an index. `ledger (seq)` and
  `ledger (at)` indexes end the scans; the count stays exact unless the foot takes an
  estimate. A panel deadline drops the answer, not the statement.
- [ ] **A deleted player's balances count in the Economy Supply and Holders until the sweep**
  `Store::currency_stats` (`modules/wallet/src/store.rs`) sums every `wallet.balances`
  row and does not exclude players tombstoned in `wallet.purged_players`, so a deleted
  player's money stays in Supply and Holders until the five-minute `player-purge` sweep
  removes the rows.
- [ ] **No test proves the Ledger count or paging across pages**
  `the_ledger_narrows_by_search_reason_and_currency_and_counts_the_selection`
  (`modules/wallet/src/admin_tests.rs`) checks totals below `LEDGER_PAGE_SIZE` only, so a count
  that equals the page size, or one that also applies the cursor, would pass; keyset paging is
  proven only for one player's filter, never on the global ledger.
- [ ] **The Ledger Reason filter trusts keys and reasons the contract does not reserve**
  The Admin option selects every movement whose idempotency key starts with `admin:`, but the
  walletapi contract reserves only `starter:` (`api/wallet/api/src/lib.rs:62-67`), so a wire
  caller's `admin:` key lists as an operator movement. The Starter option matches reason text a
  wire caller can also write. Reserving `admin:` in the contract and filtering starter grants by
  the `starter:` key end it.
- [ ] **A retried currency create reads "code exists"**
  `currency.create` (`modules/wallet/src/admin.rs`) has no idempotency key, so a retry after a
  timeout or failed settle whose insert already landed gets the Code "already exists" error
  instead of a success. Nothing is written twice; only the message misleads.
- [ ] **Currency codes and kinds take any text within their byte caps**
  `parse_new_currency` (`modules/wallet/src/admin.rs`) trims and length-checks Code and Kind
  only, so inner spaces, mixed case or Unicode pass, and Kind accepts more than the "soft or
  hard" its help names. Movements apply the same rule (`modules/wallet/src/service.rs:48`).
- [ ] **No test pins the wallet Money shape error text**
  The unknown-key case in `modules/wallet/src/admin_tests.rs` asserts only `is_err()`, so a
  change to "Amount must name an amount and a currency" in `parse_amount` passes unnoticed.

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
