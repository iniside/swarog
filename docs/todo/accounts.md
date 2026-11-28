# Accounts

Last updated: 2026-11-28

## In progress

_None._

## Open

- [ ] **Link more identities to one player**
  A signed-in player can attach further identities (Google, Epic, others) from the hosted page
  whenever they want. Today Google is sign-in only and Link is Epic's JSON flow.
  Source: docs/plans/2026-11-26-1123-google-browser-login-plan.md, owner answer 2 (2026-11-26).

- [ ] **Single failure-rendering policy for the Epic OAuth callback**
  ACCOUNTS-01. The Epic callback renders a Postgres outage as a 503 body on
  `handle_callback`'s state lookup but as a `/?epic=error` redirect on
  `epic_oauth::complete`'s link-flow `verify_access`. Source: modules/accounts/src/oauth_flow.rs:345 (state lookup)
  and modules/accounts/src/epic_oauth.rs:206 (link-flow lookup) (2026-07-28).
- [ ] **Facebook and console login providers**
  ACCOUNTS-03. Not present in modules/accounts/src (Grep, lower bound). Each is a
  new credential verifier, not a new op. Source: docs/roadmap/feature-tracker.md:140 @ fd797b3a
  (2026-08-30).
- [ ] **GDPR data export**
  ACCOUNTS-08. No export exists in modules/accounts/src (Grep, lower bound).
  Source: docs/roadmap/feature-tracker.md:145 @ fd797b3a (2026-09-10).
- [ ] **Profile fields beyond the handle**
  ACCOUNTS-09. Avatar, language and an editable display name do not exist in
  modules/accounts/src (Grep, lower bound); the display name is immutable.
  Source: docs/roadmap/feature-tracker.md:146 @ fd797b3a (2026-09-06); see
  docs/plans/2026-09-05-1921-friends-module-plan.md:589.
- [ ] **`player.promoted` `from_provider` is the constant `guest`**
  ACCOUNTS-14. modules/accounts/src/lib.rs:562 sets `from_provider: GUEST`; only
  guest accounts are promotable today. Source: modules/accounts/src/lib.rs:562
  (2026-08-29); docs/roadmap/feature-tracker.md:150 @ fd797b3a.
- [ ] **Open registration (1b)**
  `ACCOUNTS_OPEN_REGISTRATION` stays (default OFF) until this lands.
  Source: docs/plans/2026-11-24-2128-accounts-browser-login-plan.md, owner decision 1 (2026-11-25).
- [ ] **UILayout mockup of the hosted login page**
  Add a `UILayout/` mockup that roughly matches the shipped page, then port the page to it.
  Scheduled after roadmap Phase A (Steps 2-4) of docs/plans/2026-11-24-0004-lore-integration-roadmap-plan.md
  (owner, 2026-11-26).
  Source: docs/plans/2026-11-24-2128-accounts-browser-login-plan.md, owner decision 8 (2026-11-25).
- [ ] **Signing-key rotation**
  No operator action rotates the ES256 OIDC signing key. The `retiring`/`retired` statuses of
  `accounts.signing_keys` exist so rotation needs no schema change; a retiring key stays in the JWKS
  until its last token expires. When rotation lands, splitproof `[SK3]`/`[SK3m]`
  (tools/splitproof/src/oidc_discovery_checks.rs) must change: they expect exactly one live row.
  Scheduled with roadmap Step 9 (docs/plans/2026-11-24-0004-lore-integration-roadmap-plan.md).
  The gateway learns a new key only on its next `token_state` pull (every 5 s), so a token signed
  with a brand-new kid gets 401 until then. Publish a new key before signing with it
  (modules/gateway/src/verifier.rs, one-token-mechanism step 5 review, 2026-11-27).
  Source: docs/plans/2026-11-26-1415-oidc-signing-keys-plan.md D8, owner answer 3 (2026-11-26).
- [ ] **KEK rotation**
  Changing `ACCOUNTS_SIGNING_KEK` refuses startup until `accounts.signing_keys` is dropped and
  accounts boots fresh, which invalidates every issued token. No re-seal tool exists.
  Scheduled with roadmap Step 9.
  Source: docs/plans/2026-11-26-1415-oidc-signing-keys-plan.md D8, owner answer 3 (2026-11-26).
- [ ] **No single browser-session revoke for operators**
  An operator cannot end one browser session; only Revoke all ends them.
  Source: docs/plans/2026-11-24-2128-accounts-browser-login-plan.md, owner decision 11 (2026-11-26).

### Known limits (google-browser-login step 4)

- [ ] **`[BG3]` has never been watched go red**
  No split run with a deliberately broken `provider = $3` predicate has shown
  tools/splitproof/src/accounts_browser_checks.rs `[BG3]` (:368) failing.
  Source: docs/plans/2026-11-26-1352-oauth-drift-fixes-plan.md D5, owner answer ("ok 1,2 zrobic reszta do todo", 2026-11-26).

### Known limits (accounts-browser-login step 8)

- [ ] **The old `/accounts/epic` binding cookie can coexist for up to 10 minutes**
  Source: docs/plans/2026-11-24-2128-accounts-browser-login-plan.md, deferred review leftover (2026-11-26).

### Known limits (one-token-mechanism step 3)

- [ ] **refresh_player_idx may be dead**
  Its last-seen reader moved to `token_families` in db9bda92; check for other readers of
  `refresh_player_idx` (modules/accounts/src/lib.rs) and drop it if none.
- [ ] **EPIC_START hard-codes dev-game**
  `tools/splitproof/src/accounts_browser_checks.rs:68` writes `dev-game` literally while every
  other site uses `accountsapi::DEV_GAME_CLIENT_ID`.

### Known limits (one-token-mechanism step 1)

- [ ] **The `delete_client` lock has no test**
  No test shows that `delete_client` (modules/accounts/src/clients.rs) serializes with a
  concurrent login or refresh on the same client.

### Known limits (one-token-mechanism step 6)

- [ ] **The Epic flow reads the whole token state per request**
  `epic_oauth.rs:130` and `:206` call `Tokens::verify_access` (modules/accounts/src/jwt.rs:194),
  which loads the whole snapshot (keys and every live revocation) from the database each time.
  Correct, but costly at volume.
- [ ] **A minted `iat` can run ahead of the clock**
  `mint_access_tx` raises `iat` to one second past the latest live cut-off, so each fast
  revoke-then-mint cycle can push it about 2 s ahead. `REVOCATION_SKEW_MARGIN` (60 s) covers it;
  only operator actions repeated faster than that could exhaust it (review of a289af45, 2026-11-27).

### Known limits (oidc-authorization-code step 1)

- [ ] **The Epic login lets a confidential client reach the mint**
  The Epic start does not check the client; a confidential client is refused only at the final
  mint, by `client_allows_tx` (modules/accounts/src/lib.rs:584, clients.rs:280).

### Known limits (oidc-authorization-code step 3)

- [ ] **Unredeemed codes of an ended session stay until the prune**
  Logout sets `oidc_codes.session_digest` to NULL; such a code can no longer be redeemed, but its
  row stays until the prune (modules/accounts/src/codes.rs).
- [ ] **Two boot refusals run after the DDL**
  `refuse_stale_oauth_states` and `refuse_unfamilied_tokens` (modules/accounts/src/lib.rs:352,
  :375) run after `Ddl::run` (:1945-1946), unlike `refuse_stale_codes` (:1848); no test shows they
  still catch the old shape.

### Known limits (oidc-authorization-code step 4)

- [ ] **No index on `token_revocations.family_id`**
  Each code mint scans the table for a family revocation. It holds only about 15 minutes of rows.

### Known limits (oidc-device-grant step 2)

- [ ] **The second lost poll race in a row is untested**
  `settle_device_poll` answers `slow_down` only after a second lost guarded statement; no test drives that arm.
  Source: review of b7fce2ca (2026-11-27).
- [ ] **Two copies of the redeemed-grant "kept" query**
  `redeemed_code_kept_tx` and `redeemed_device_kept_tx` run the same EXISTS query on different tables.
  Source: review of f8ba2bb1 (2026-11-27).
- [ ] **The replay revoke sequence lives in two places**
  `web/token.rs` and the device_codes store each revoke a replayed grant's family.
  Source: review of b7fce2ca (2026-11-27).
- [ ] **Device-code TTL parsing still decodes twice**
  `parse_device_code_ttl` calls `decode_field` (modules/accounts/src/admin.rs:330) before `duration_secs`, and
  `lifetime()` (admin.rs:323-326) hard-codes the m/s/60 units instead of `controlplane::DURATION_UNITS`.
  Source: review of 05673fa5 (2026-11-27).

### Known limits (oidc-device-grant step 3b)

- [ ] **No rate limit on `/accounts/oidc/device_authorization`**
  The endpoint is unauthenticated and writes one `device_codes` row per call for any public client with
  the device grant.
  Source: review of 91f2c2a2 (2026-11-27).
- [ ] **A failed mint burns the device code**
  A transient 503 after the redemption commits leaves the row redeemed; the next poll is a replay and gets
  `invalid_grant`. The code grant behaves the same.
  Source: review of 91f2c2a2 (2026-11-27).

### Known limits (oidc-device-grant step 4)

- [ ] **The enter-code screen shows a lifetime bound, not the lifetime**
  Each client has its own `device_code_ttl_secs` and screen 1 knows no client yet, so it says a code
  expires at most `MAX_DEVICE_CODE_TTL_SECS` after issue. Source: e86554d8; owner deferred (2026-11-28).
- [ ] **A player deleted mid-approval gets a login redirect, not the generic error**
  `found()` in modules/accounts/src/web/device.rs answers `Refused::SignedOut` (a 303 to a bare login,
  no `return_to`) when the signed-in player's row is gone after a code was found. Looking the player up
  before the code removes both. Source: review of e86554d8 (2026-11-28).
- [ ] **No test pins escaping on the device pages**
  User-Agent and client name are attacker-controlled; only the `.html` template names turn on
  minijinja's auto-escape. A test should issue a code with a `<script>` User-Agent and check the confirm
  page. Source: review of e86554d8 (2026-11-28).
- [ ] **The router's expired-decision test stops at the pre-check**
  The expired approve/deny case in tests/device_pages.rs is refused by `found()`, not by the guarded
  UPDATE; the UPDATE's own expiry guard is pinned only at store level (tests/device_codes.rs:465-467).
  Source: review of e86554d8 (2026-11-28).

### Deferred

- [ ] **A consent screen on `/authorize`**
  The A3 plan (docs/plans/2026-11-27-1020-oidc-authorization-code-plan.md, owner answer 2) issues a code
  without asking the player, because every client is made by the operator in the portal. A consent
  screen may be needed later, for example for a third-party client or one asking for more than
  `openid profile offline_access`.
