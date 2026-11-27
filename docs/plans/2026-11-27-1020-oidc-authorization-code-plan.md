# OIDC authorization code, token and revoke endpoints (roadmap A3)

Date: 2026-11-27
Parent: docs/plans/2026-11-24-0004-lore-integration-roadmap-plan.md, Phase A, Step 3.
Builds on: docs/plans/2026-11-26-1848-one-token-mechanism-plan.md (one ES256 `at+jwt` access token,
`accounts.oidc_clients` made in the portal, `token_families`, `token_revocations`, gateway pull verifier).
Status: approved 2026-11-27; implementation not started (owner: "popraw plan, ale narazie nie implementuj")
Plan review: core-reviewer (opus, high), REJECT with 14 items; all applied below, items 1-2 as owner questions.
Owner answers (2026-11-27): 1, 3, 4 as recommended; 2 no consent screen, recorded in TODO as possibly
needed later; 5 follow RFC 6749 §4.1.2 and revoke the first redemption's tokens on a replay (D5).

## Context

Owner (2026-11-27): "mozesz przepisac plan pod A3". This means rewriting the roadmap's Step 3 for the code
that the one-token plan left behind.

Requested outcome, from roadmap Step 3: "Clients, authorization code + PKCE, refresh, revocation". The
portal (roadmap Step 12) and the Lore CLI's token refresh need them. One token mechanism stays. Owner,
2026-11-26: "lepiej to zrobic teraz inaczej bedziemy mieli dwa systemy tokenow"; "jedna sciezka tokenow,
roznie wydawane dla poszczegolnyhc klientow z rozna zawartoscia"; "nie bedziemy robic hardcode".

**What changes against the roadmap's Step 3 text:**
- Roadmap 3(c) says "Swarog audience → the existing opaque session". That is void: the opaque session no
  longer exists. Every audience gets the same ES256 `at+jwt` from the same mint. The client decides only
  the content (`aud`, `root_domains`).
- Roadmap 3(a) says "tables `oidc_clients`". That table exists. This plan adds columns to it (one-token
  plan D1).
- "Refresh reuses the family store" now means `token_families` and the existing rotation
  (`Auth::refresh`, modules/accounts/src/lib.rs:1413).

**Explicit exclusions:**
- The device grant (roadmap Step 4).
- `client_credentials` (Step 5d, needs `authz`).
- Token exchange and Tier 2 `resources` (later). This plan only keeps the `/token` grant dispatch open
  for them.
- A userinfo endpoint.
- A consent screen (owner answer 2; docs/todo/accounts.md records that one may be needed later).
- CORS.
- Client-secret rotation (delete and recreate the client).
- Pushing revocations to loreserver.
- The Epic fragment login (stays as is).

## Facts (verified 2026-11-27 by three researchers; path:line at HEAD 8e4cd752)

**Clients and families:**
- `accounts.oidc_clients`: `client_id` PK, `display_name`, `grant_types text[]`, `audience` NOT NULL,
  `created_at` (modules/accounts/src/lib.rs:239-245).
  - It has no secret, redirect URI or scope column.
  - `insert_client` (modules/accounts/src/clients.rs:122-135) always binds `accountsapi::SWAROG_AUDIENCE`.
  - The admin create form (modules/accounts/src/admin.rs:286-304, handler :1067-1104) takes a display name
    and grant types.
- `grant_types` = `password`, `guest`, `refresh_token` (api/accounts/api/src/lib.rs:100-108).
  `canonical_grants` and the form follow `grant_types::ALL`.

**The mint:**
- The one mint is `issue_session_tx` (modules/accounts/src/lib.rs:489-538):
  `client_allows_tx` → `insert_family_tx` → refresh root if the client has `refresh_token` →
  `mint_access_tx` → `sign_access` → `stamp_identity_used_tx`.
- The identity stamp requires a provider identity. An authorization-code mint has none, because the
  browser login already stamped it.
- `sign_access` (lib.rs:541-551) passes the constant `SWAROG_AUDIENCE` to `Tokens::claims`.
- `AccessClaims` (modules/accounts/src/jwt.rs:24-33) is `iss, sub, aud: String, client_id, sid, iat, exp,
  jti`. The header is ES256 with `typ=at+jwt` and `kid` (jwt.rs:106-111).
- `check_token_size` (jwt.rs:146, called at boot from lib.rs:2028) assumes every audience is
  `SWAROG_AUDIENCE`.

**Verifiers:**
- Both verifiers pin `aud = SWAROG_AUDIENCE`: accounts `verify_against` (jwt.rs:213-216) and the
  gateway (modules/gateway/src/verifier.rs:149).
- So a Lore-audience token is refused by the gateway. That is correct and stays.

**Refresh:**
- `Auth::refresh(refresh_token)` takes no `client_id`. The client comes from the family row
  (lib.rs:1087 `refresh_allowed_tx`). `/accounts/refresh` stays a public op with `auth = "none"`
  (api/accounts/api/src/lib.rs:368), and the password/guest ops mint for a client named only by its id
  (`client_allows_tx`, clients.rs:141-158).

**Browser session:**
- `accounts.browser_sessions` with the `swarog_session` cookie (modules/accounts/src/web/mod.rs:25-31) and
  `Web::signed_in` (:180) is the signed-in human.
- `safe_return_to` (:303) accepts a path under `/accounts/oidc/` with its query, up to 2048 bytes.
- `ct_eq` (:332) is private to `web`.
- Every sign-in inserts a new `browser_sessions` row (modules/accounts/src/web/session.rs:63), so its
  `created_at` is the sign-in time. The handle is `store::render_handle(display_name, discriminator)`
  (modules/accounts/src/store.rs:128); `Player` holds only `{id, display_name}` (store.rs:713).
- Module `migrate` is create-only DDL, never an alter (.agents/shared/core-rules.md:99-101;
  core/lifecycle/src/ddl.rs:104-105). No `ADD COLUMN` exists in `modules/` or `core/`.

**Discovery and routing:**
- Discovery (modules/accounts/src/discovery.rs:13-31) serves only `issuer` and `jwks_uri`.
- The issuer path is pinned to `/accounts/oidc` (modules/accounts/src/signing_keys.rs:110-129).
- gateway-svc passes `/accounts/oidc` through to accounts-svc (cmd/gateway-svc/src/addrs.rs:246). Every
  new route goes under that prefix, so no gateway change is needed.

**Single-use state precedent:**
- `accounts.oauth_states`: `new_state` prunes, then inserts (modules/accounts/src/oauth_flow.rs:144-172).
- `take_state` is `DELETE … RETURNING` with a TTL predicate (:183-215); a miss returns `Ok(None)` and
  a DB error returns `Err`.

**Secret precedent:**
- apikeys stores base64url(SHA-256) (modules/apikeys/src/store.rs:39-41).
- It reveals the secret once through `SecretReveal` (modules/apikeys/src/admin.rs:120-150). accounts
  already uses `SecretReveal` (modules/accounts/src/admin.rs:153).
- accounts must copy the digest, not import apikeys.

**Lore v0.14.0** (source at G:/tmp/lore-latest, tag 317f48b9):
- **CLI discovery** (lore-transport/src/auth/oidc/discovery.rs:30-38):
  - Requires `issuer`, `token_endpoint`, `jwks_uri`.
  - Optional: `revocation_endpoint`, `token_endpoint_auth_methods_supported`.
- **CLI refresh** (refresh.rs:18-34): POSTs `grant_type=refresh_token`, `refresh_token`, `client_id`,
  with no secret and no scope.
  - `invalid_grant` means "log in again".
  - The new access token must keep the same `sub`.
  - `token_type` must be `Bearer` (token.rs:20-36).
- **CLI revoke** (revocation.rs:20-40): RFC 7009 `token`, `token_type_hint=refresh_token`, `client_id`.
- **Where the CLI sends a token** (lore-credential/src/jwt.rs:59-137): only to hosts in a `root_domains`
  claim, or hosts derived from an `aud` that is an https URL or a dotted domain. A bare `aud` like
  `swarog` yields no host.
- **loreserver verification** (lore-server/src/auth/jwt.rs:141-172, 413-466; jwk.rs:211-221, 322-372):
  - Requires `kid`, `sub`, `iss`, `iat`, `exp`, `aud`; checks `iss` and `aud` against its
    config.
  - Reads the JWKS through `<issuer>/.well-known/openid-configuration` with no redirects.
  - Needs the JWK's `alg`, which Swarog already publishes.
  - It has no revocation pull.
- Lore itself never uses authorization code + PKCE. The auth-code shape is for the portal only.

## Overlap (why not extend X)

| Need | Candidate | Decision |
|---|---|---|
| Client registration | `accounts.oidc_clients` | **Extended** with columns, never a second table (one-token plan D1) |
| Auth codes | `accounts.oauth_states` | Pattern copied into a new `oidc_codes` table. The Epic table's CHECKs tie it to provider intents (lib.rs:297-311), so it is not reused |
| Client secrets | apikeys digest + `SecretReveal` | Pattern copied into accounts. A module→module import is forbidden |
| Token mint | `issue_session_tx` | **Extended**: its body is split into a mint core and the stamping wrapper. No second signer |
| Refresh grant | `Auth::refresh` rotation | **Wrapped** by `/token`, plus a client-binding check. No second rotation |
| Revoke | `kill_family_tx` | **Reused** for RFC 7009 |
| Lore audience | `oidc_clients.audience` (decorative today) | **Wired** into `sign_access` |
| Gateway audience | the `SWAROG_AUDIENCE` pin | **Kept.** loreserver verifies its own audience |

## Decisions

- **D1. Client columns.** The columns go into the `CREATE TABLE accounts.oidc_clients` DDL
  (modules/accounts/src/lib.rs:239-245):
  - `redirect_uris text[] NOT NULL DEFAULT '{}'`
  - `root_domains text[] NOT NULL DEFAULT '{}'`
  - `scopes text[] NOT NULL DEFAULT '{}'`
  - `secret_digest text`, nullable. NULL means a public client.

  Rollout is `DROP SCHEMA accounts CASCADE` plus a fresh boot. There is no `ALTER` and no data
  migration (core-rules, wipe rule). This corrects the one-token plan's D1 sentence about
  `ADD COLUMN IF NOT EXISTS`; that plan stays as written (it is an archive).

  A boot refusal names the DROP remedy for a stale `oidc_clients` that lacks the columns. It copies
  `refuse_unfamilied_tokens` (lib.rs:344).

  `grant_types::ALL` gains `authorization_code`.
- **D2. Confidential clients** (owner answer 1). The portal create form has a "confidential" choice.
  - **Secret:** a confidential client gets a generated secret: 32 random bytes, base64url, prefix `cs_`.
    Only its base64url(SHA-256) digest is stored, and the secret is shown once via `SecretReveal`.
  - **Grants:** a confidential client may hold only `authorization_code` and `refresh_token`.
  - **Op paths refuse confidential clients.** The rule is enforced inside `client_allows_tx` and
    `refresh_allowed_tx`, so no front door can skip it. It covers `Auth::login`, `Auth::guest`,
    `login_federated`, the Epic start and `Auth::refresh`. A confidential client therefore mints and
    refreshes only through `/token`, where it authenticates.
  - **How a client authenticates at `/token` and `/revoke`:**
    - A confidential client uses `client_secret_basic` (the Basic credentials form-urlencoding-decoded,
      RFC 6749 §2.3.1) or `client_secret_post`.
    - A public client sends `client_id` only (method `none`).
    - Two methods in one request is `invalid_request`.
    - A missing or wrong secret is 401 `invalid_client`. When the request used a Basic header, the 401
      carries `WWW-Authenticate: Basic`.
  - **Compare:** `ct_eq` and the digest move to a new modules/accounts/src/secret.rs used by `web` and
    the new endpoints.
- **D3. Audience per client.**
  - `sign_access` signs with the client's `audience`. It reads the audience on the caller's transaction,
    in both the first mint and the refresh/grace re-sign (lib.rs:521, :1115).
  - When the client's `root_domains` is non-empty, the token gets a `root_domains` claim (an array).
  - Every access token gains `preferred_username`, the player's handle read in the same transaction
    (owner answer 3).
  - The portal form takes the audience. An empty value means `SWAROG_AUDIENCE`.
  - Both verifiers keep pinning `SWAROG_AUDIENCE`.
  - **Size caps:**
    - Field caps: audience ≤ 128 bytes; root_domains ≤ 8 entries of ≤ 253 bytes; redirect_uris ≤ 10
      entries of ≤ 512 bytes.
    - A client whose worst-case token would exceed `MAX_ACCESS_TOKEN_BYTES` is refused at create.
      Worst case means the longest player id, the longest JSON-escaped handle, the `jti`, and the
      client's audience and root_domains.
    - `check_token_size` at boot checks every stored client the same way.

    This closes docs/todo/accounts.md "`oidc_clients.audience` has no length bound".
- **D4. `/accounts/oidc/authorize` (GET).**
  - **Parameters:** `response_type`, `client_id`, `redirect_uri`, `scope`, `state` (≤ 256 bytes),
    `code_challenge` (exactly 43 base64url characters), `code_challenge_method=S256`, `nonce`
    (optional, ≤ 128 bytes), `prompt` (only `none` is acted on; other values are ignored).
  - **Errors rendered as a page, never redirected:**
    - an unknown client, or a `redirect_uri` that is not byte-equal to one of the client's
      `redirect_uris`, renders an error page;
    - a DB outage is a 503 page.
  - **Errors redirected** to the `redirect_uri` with `error` and `state` (RFC 6749 §4.1.2.1):
    - `unauthorized_client`: the client lacks the `authorization_code` grant;
    - `unsupported_response_type`: `response_type` is not `code`;
    - `invalid_request`: PKCE is missing or not S256, a parameter is malformed or oversized, or the
      login return URL would exceed `MAX_RETURN_TO_BYTES`;
    - `invalid_scope`: a requested scope is not in the client's `scopes`;
    - `login_required`: `prompt=none` was sent with no session.
  - **No session:** redirect to the hosted login. `return_to` is rebuilt from the parsed parameters
    with canonical percent-encoding, so `safe_return_to` (which refuses a literal `//`) accepts it.
  - **With a session:** issue a code and redirect at once. There is no consent screen (owner answer 2).
  - PKCE S256 is required for every client.
- **D5. `accounts.oidc_codes`.**
  - **Columns:**
    - `code_digest` PK: the SHA-256 of a 32-byte random code;
    - `client_id`: FK to `oidc_clients`, ON DELETE CASCADE;
    - `player_id`: FK to `players`, ON DELETE CASCADE;
    - `redirect_uri`, `code_challenge`, `scope`, `nonce`, `auth_time`, `created_at`;
    - `redeemed_at` and `family_id` (both nullable, set together at redemption).
  - **TTL** is 60 s.
  - **Prune and insert** are two separate autocommit statements: prune expired rows, then insert. They
    never share a transaction, so a prune can't hold one player's code rows while the insert waits on
    another player's key-share lock.
  - **Redemption** is `UPDATE … SET redeemed_at = now(), family_id = $fresh_uuid WHERE code_digest = $1
    AND redeemed_at IS NULL AND created_at > now() - TTL RETURNING`, run in its own committed
    transaction. The family id of the tokens this code will mint is chosen here and stored on the row.
    - Every later check failure (client, redirect, PKCE) is `invalid_grant`, and the code stays
      consumed.
    - The mint runs in a second transaction with that family id. A vanished player or client (FK
      violation or missing row) is `invalid_grant`.
    - The code's row lock is therefore never held across the player lock, so it can't deadlock with a
      player delete.
  - **Retention:** a redeemed row is kept until `created_at + REPLAY_WINDOW` (10 min), so a replay can
    still find it; the prune deletes rows past that. A code is redeemable only within the 60 s TTL.
  - **Removed with the player's families.** Password reset, admin Revoke all and player delete delete
    that player's codes in the same transaction as the families (`kill_player_families_tx`,
    `revoke_player_families_tx`; delete by cascade). A client delete cascades.
  - **Replay (RFC 6749 §4.1.2, owner answer 5).** A redemption that finds the row already redeemed
    answers `invalid_grant` and revokes the tokens of the first redemption: in its own transaction it
    takes `pg_advisory_xact_lock` on the stored family id, writes a family revocation for it
    (`token_revocations.family_id` has no FK, so the row is valid before the family exists) and runs
    `kill_family_tx` when the family exists.
    - **The race with the first redemption's mint:** the mint transaction takes the same advisory lock
      first, then refuses with `invalid_grant` when a family revocation for its id already exists. The
      two serialize on the lock, so either the mint commits first and the replay kills the family, or
      the replay commits first and the mint never creates it. No access token of a replayed code
      survives the next gateway pull.
    - The advisory lock is taken before any row lock and only by these two paths, so it adds no
      deadlock with a player delete.
    - A replay after `REPLAY_WINDOW` finds no row and is plain `invalid_grant` without a revocation.

- **D6. `/accounts/oidc/token` (POST, form, 8 KiB body limit).**
  - **`authorization_code`:**
    1. Authenticate the client (D2).
    2. Redeem the code (D5).
    3. Check that the `client_id` and `redirect_uri` match the code.
    4. Check that the `code_verifier` is 43-128 unreserved characters (RFC 7636 §4.1) and matches the
       challenge.
    5. Mint through the mint core (D8).
  - **`refresh_token`:** authenticate the client, then run the existing rotation.
    - If the family's client is not the presented `client_id`, the answer is `invalid_grant` and the
      family is **not** killed.
    - Reuse and expiry keep today's semantics.
  - **Any other grant** is `unsupported_grant_type`. The dispatch is a match on `grant_type`, so token
    exchange adds an arm later.
  - **Response:** JSON `{access_token, token_type:"Bearer", expires_in, refresh_token?, scope?,
    id_token?}` with `Cache-Control: no-store`.
    - `refresh_token` follows the client's grant.
    - `scope` is the granted scope on the code exchange; the refresh response omits it, which RFC 6749
      §5.1 allows because it is unchanged.
  - **Errors:** RFC 6749 §5.2 JSON. A DB outage is 503, never `invalid_grant`.
  - **No per-IP limiter:** a 256-bit code or refresh token can't be guessed.
- **D7. `id_token`.**
  - Issued only by the `authorization_code` grant, and only when the granted scope includes `openid`.
  - Claims: `iss`, `sub`, `aud = client_id`, `iat`, `exp` (the access TTL), `auth_time` (from the
    code), `nonce` when given, `preferred_username`.
  - Same active key, header `typ=JWT`. Both Swarog verifiers refuse it as an access token by `typ`.
  - The D11 reference doc tells loreserver operators to set `jwt_typ = ["at+jwt"]`.
- **D8. Mint core.** The body of `issue_session_tx` after the grant check becomes one function that
  every mint calls: family, optional refresh root, `mint_access_tx`, `sign_access`.
  - The password, federated and guest paths keep `stamp_identity_used_tx` and its lock-order comment.
  - The code grant calls the core after `client_allows_tx(…, authorization_code)`, in the existing order:
    client `FOR KEY SHARE`, then the player through the family FK (lib.rs:480-488). Before them it
    takes the advisory lock on the code's family id and checks for a replay revocation (D5). It takes no
    `oidc_codes` row lock, because the code was redeemed in its own transaction.
- **D9. `/accounts/oidc/revoke` (POST, form, 8 KiB body limit, RFC 7009).**
  - Authenticate the client as in D2.
  - A refresh token of that client's family runs `kill_family_tx`.
  - An access token is checked with a revoke-side verify: signature, `iss` and `typ` against the
    snapshot, `aud` equal to the presenting client's audience, and `client_id` equal to the presenting
    client. If it passes, its `sid` family is revoked. This works for Lore-audience tokens too.
  - An unknown or foreign token gets 200, a malformed request 400, `invalid_client` 401, and a DB outage
    503.
- **D10. Discovery adds:**
  - `authorization_endpoint`, `token_endpoint`, `revocation_endpoint`;
  - `response_types_supported ["code"]`;
  - `grant_types_supported ["authorization_code","refresh_token"]` (only the `/token` grants);
  - `code_challenge_methods_supported ["S256"]`;
  - `token_endpoint_auth_methods_supported ["none","client_secret_basic","client_secret_post"]` and
    `revocation_endpoint_auth_methods_supported` (the same list);
  - `id_token_signing_alg_values_supported ["ES256"]`;
  - `subject_types_supported ["public"]`;
  - `scopes_supported`.
- **D11. Lore revocation is bounded by the TTL.** loreserver does not pull revocations, so a revoked
  Lore-audience token stays valid there until its `exp` (at most 15 min; owner answer 4). The
  reference doc states this.
- **D12. Scopes.** The vocabulary is `openid`, `profile` and `offline_access`.
  - `openid` adds the `id_token`.
  - `profile` and `offline_access` are accepted and change nothing. Lore's device request (A4) sends
    `offline_access`. `preferred_username` is always present, and a refresh token follows the client's
    `refresh_token` grant.
  - An omitted `scope` grants none, so there is no `id_token`.
  - Scope is not stored on the family.
- **D13. Dev seed and form defaults.**
  - `seed_dev_client` (clients.rs:68) binds an explicit `password`, `guest`, `refresh_token` instead of
    `grant_types::ALL`, so its shape does not change.
  - The create form pre-picks the same three.
  - Picking `authorization_code` requires at least one redirect URI.

## Owner answers (2026-11-27)

1. **Confidential clients (D2):** yes, only `authorization_code` and `refresh_token`; every op path refuses them.
2. **Consent (D4):** no consent screen now; docs/todo/accounts.md records that one may be needed later.
3. **`preferred_username` (D3):** yes, in every access token.
4. **Lore revocation (D11):** accepted, bounded by the 15 min TTL.
5. **Code replay (D5):** follow RFC 6749 §4.1.2: refuse and revoke the first redemption's tokens.

## Steps

### Step 1 [core-implementer, opus, high]: client columns, secrets and the portal form

- **(a) What is touched:**
  - modules/accounts/src/lib.rs: D1 DDL and the stale-shape boot refusal.
  - modules/accounts/src/clients.rs: `ClientRow`; `insert_client` with audience, redirect URIs,
    root_domains, scopes and an optional secret digest; D13.
  - modules/accounts/src/secret.rs (new): secret generation, digest, `ct_eq` moved from web/mod.rs:332.
  - api/accounts/api/src/lib.rs: `grant_types::AUTHORIZATION_CODE` in `ALL`; the scope vocabulary
    (D12).
  - modules/accounts/src/admin.rs: create form fields, validation, `SecretReveal`, and the table's new
    columns.
  - D2's confidential refusal inside `client_allows_tx` and `refresh_allowed_tx` (lib.rs:1087).
  - Tests.
- **(b) Why first:** every later step reads these columns, and the op-path refusal must exist before any
  confidential client can.
- **(c) How:**
  - Redirect URIs must be absolute `https`, or `http` on a loopback host, with no fragment.
  - root_domains entries must be lowercase DNS names. Scopes must come from D12. Field caps and the
    worst-case size check follow D3.
  - Tests:
    - create a confidential client: the secret shows once and the digest matches;
    - a public client stores no digest;
    - an invalid redirect URI is a field error;
    - an oversized audience is refused;
    - `authorization_code` without a redirect URI is refused;
    - a confidential client is refused on `Auth::login`, `Auth::guest` and `Auth::refresh`;
    - the dev seed keeps its three grants.

### Step 2a [core-implementer, opus, high]: mint core and audience

- **(a) What is touched:**
  - modules/accounts/src/lib.rs: `issue_session_tx` split per D8; `sign_access` and the refresh
    re-sign at :1115 read the audience, root_domains and handle on the caller's transaction.
  - modules/accounts/src/jwt.rs: `AccessClaims` gains `preferred_username` and an optional
    `root_domains`.
  - modules/accounts/src/store.rs: the mint returns what `sign_access` needs on the same connection.
  - Tests.
- **(b) Why now:** the code grant needs the mint core. lib.rs (91 KB) and store.rs (69 KB) are large, so
  this step does only the mint.
- **(c) How:**
  - Never read on a second pool connection (the 51c9fe8b rule).
  - Tests:
    - a Lore-audience client's token carries its `aud` and `root_domains`, and so does its refreshed
      token and its grace-replay token;
    - the gateway verifier and `verify_access` refuse it;
    - a Swarog client's token is unchanged apart from `preferred_username`.

### Step 2b [core-implementer, opus, high]: id_token signer and size check

- **(a) What is touched:** modules/accounts/src/jwt.rs: an `IdClaims` type and a `typ=JWT` signer;
  `check_token_size` over stored clients (D3). Tests.
- **(b) Why now:** `/token` (Step 4) signs id_tokens.
- **(c) Tests:**
  - an id_token decodes with `typ=JWT` and is refused by both access verifiers;
  - boot refuses an oversized stored client.

### Step 3 [core-implementer, opus, high]: `/authorize` and `oidc_codes`

- **(a) What is touched:**
  - modules/accounts/src/web/authorize.rs (new), mounted in `hosted_browser_router` (web/mod.rs:236-264);
  - the `oidc_codes` DDL and store functions (D5);
  - the code deletes in `kill_player_families_tx` and `revoke_player_families_tx`;
  - tests using the `serve_web` harness (modules/accounts/src/tests/hosted_login.rs:9-35).
- **(b) Why now:** `/token` redeems these codes.
- **(c) Implement D4 and D5. Tests cover every D4 branch:**
  - a bad client or redirect URI renders a page and does not redirect;
  - each redirect error carries `state`;
  - `prompt=none` with no session gives `login_required`;
  - with no session, login runs and returns to the same request, including an unencoded
    `redirect_uri=https://…` in the original query;
  - oversized parameters give `invalid_request`;
  - a session issues a code bound to the client, redirect URI, challenge, nonce and auth_time;
  - the code is redeemable once and not after the TTL, with `now()` pinned in the transaction and never
    a real-clock race;
  - a redeemed row survives until `REPLAY_WINDOW` and the prune removes it after;
  - a password reset and an admin Revoke all delete the player's codes;
  - a player delete during redemption gives `invalid_grant` and does not deadlock (two connections,
    ordered by locks, not sleeps).

### Step 4 [core-implementer, opus, high]: `/token`

- **(a) What is touched:** modules/accounts/src/web/token.rs (new), mounted under `/accounts/oidc`;
  client authentication (D2); the two grants (D6); the id_token (D7). Tests.
- **(b) Why now:** it needs the codes (Step 3), the mint core (2a) and the signer (2b).
- **(c) Tests:**
  - a full code + PKCE exchange returns an access token, a refresh token per the client's grant, and an
    id_token only with `openid`, carrying `nonce` and `auth_time`;
  - a wrong verifier, a malformed verifier, a wrong redirect URI or a wrong client gives
    `invalid_grant` or `invalid_request`, and the code stays consumed;
  - a confidential client without its secret gives 401 `invalid_client` with `WWW-Authenticate` on
    Basic;
  - two auth methods in one request gives `invalid_request`;
  - a public client's refresh with `client_id` only rotates and keeps `sub`;
  - a refresh by the wrong client gives `invalid_grant` and the family survives;
  - an unknown grant gives `unsupported_grant_type`;
  - a replay of a redeemed code gives `invalid_grant` and the first redemption's family is revoked
    (revocation row written, family deleted, its refresh token refused);
  - a replay that wins the advisory lock before the first mint makes that mint `invalid_grant` and
    creates no family (two connections ordered by the lock, not sleeps);
  - a DB outage gives 503.

### Step 5 [core-implementer, sonnet, high]: `/revoke` and discovery

- **(a) What is touched:** the revoke handler (D9) next to `/token`; modules/accounts/src/discovery.rs
  (D10). Tests.
- **(b) Why now:** it reuses Step 4's client authentication.
- **(c) Tests:**
  - revoking a refresh token writes its family revocation row;
  - revoking a Lore-audience access token by its client revokes its family;
  - an unknown token or another client's token gives 200 and revokes nothing;
  - a confidential client without its secret gives 401;
  - a DB outage gives 503;
  - discovery lists every D10 field, with the endpoints under the issuer.

### Step 6 [test-author, opus, high]: split proof

- **(a) What is touched:** tools/splitproof/src/oidc_code_checks.rs (new), called from
  tools/splitproof/src/main.rs in both topologies. The SK discovery checks gain the D10 fields.
- **(b) Why now:** the code is final.
- **(c) Assertions, through gateway-svc and in the monolith:**
  - a client created through the portal action;
  - browser session → `/authorize` with PKCE → `/token` → the access token works on `/accounts/me`;
  - the refresh grant through `/token`;
  - `/revoke` refuses the token within the pull bound (`accountsapi::TOKEN_STATE_PULL_INTERVAL`);
  - a Lore-audience token is refused by the gateway;
  - a confidential client's refresh token is refused on `/accounts/refresh`.

  Previously-wrong branches, each shown red with a hand-injected fault: the redirect URI not checked,
  PKCE not checked, the refresh client binding missing, the op-path confidential refusal missing.
- **(d)** test-author, opus, high, followed by a proof-auditor.

### Step 7 [main session, docs]

- .claude/rules/domain-modules.md: the accounts entry.
- docs/reference: the accounts OIDC endpoints; D11 with the `jwt_typ` advice.
- docs/todo/accounts.md: move the audience-bound item to done.
- At the plan end:
  - the workspace suite;
  - verifyctl;
  - re-blessed public-api, contract and input goldens.

  The C# client does not change, because no op signature changes.
