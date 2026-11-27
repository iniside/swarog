# Accounts OIDC issuer

`accounts` is an OpenID Connect issuer for browser and CLI clients. This page lists its endpoints
and the rules a client or a relying party must know. Source: `modules/accounts/src/web/`,
`modules/accounts/src/discovery.rs`, `modules/accounts/src/jwt.rs`.

## Issuer and routes

The issuer is `ACCOUNTS_OIDC_ISSUER` (default `accountsapi::DEFAULT_OIDC_ISSUER`). Its path is
always `/accounts/oidc`, and every route below sits under it. In the split, gateway-svc passes the
whole prefix through to accounts-svc (`cmd/gateway-svc/src/addrs.rs`), `Authorization` header
included.

| Route | Purpose |
| --- | --- |
| `GET /accounts/oidc/.well-known/openid-configuration` | Discovery (RFC 8414 fields) |
| `GET /accounts/oidc/jwks` | Public ES256 signing keys; 503 when the keys cannot be read |
| `GET /accounts/oidc/authorize` | Authorization endpoint (code flow, PKCE S256) |
| `POST /accounts/oidc/token` | Token endpoint: `authorization_code` and `refresh_token` |
| `POST /accounts/oidc/revoke` | Token revocation (RFC 7009) |
| `GET/POST /accounts/oidc/login`, `POST /accounts/oidc/logout` | Hosted sign-in page and sign-out |

Discovery lists only the `/token` grants in `grant_types_supported`. The client authentication
methods of `/token` and `/revoke` are `none` (public client), `client_secret_basic` and
`client_secret_post`.

## Clients

Every client is made by an operator in the admin portal (Clients page); none is hardcoded.
`ACCOUNTS_DEV_SEED` (explicit-only) inserts `dev-game` for development.

- A **public** client authenticates by `client_id` alone.
- A **confidential** client gets a secret (shown once, stored as a digest) and may hold only the
  `authorization_code` and `refresh_token` grants. The op path (`/accounts/login`,
  `/accounts/refresh`) refuses it.
- A client with `authorization_code` must register at least one redirect URI.
- `urn:ietf:params:oauth:grant-type:device_code` (`grant_types::DEVICE_CODE`) is in the grant
  picker. A public client only: the portal refuses it for a confidential client, and `/token`
  answers `unauthorized_client` for a client that does not list it.
- Each client has a device code lifetime (`oidc_clients.device_code_ttl_secs`), from
  `MIN_DEVICE_CODE_TTL_SECS` to `MAX_DEVICE_CODE_TTL_SECS` (`modules/accounts/src/clients.rs`);
  a new client gets `DEFAULT_DEVICE_CODE_TTL_SECS` unless Create client's optional "Device code
  lifetime" field sets one. The "Device code lifetime" row action on the Clients page changes it.
  Both forms take a duration (digits and `s`, `m`, `h` or `d`) and refuse a value outside the
  bounds with a field error; the table's CHECK holds the same bounds.
- The client's `audience` becomes the `aud` of its access tokens.

## Authorization code flow

1. The client sends the browser to `/authorize` with `response_type=code`, `client_id`,
   `redirect_uri`, `code_challenge` (S256, required for every client), `scope`, and optionally
   `state`, `nonce` and `prompt`.
2. `redirect_uri` must equal one of the client's registered URIs exactly. An unknown client or URI
   gets an error page, never a redirect.
3. A signed-out browser is sent to the hosted sign-in page and comes back to the same request. There
   is no consent screen.
4. The code lives 60 s, is single-use and is bound to the browser session: signing out makes an
   unredeemed code unusable.
5. The client posts the code to `/token` with `grant_type=authorization_code`, the same
   `redirect_uri` and the PKCE `code_verifier`.

A code redeemed a second time is refused, and the tokens of its first redemption are revoked
(RFC 6749 §4.1.2).

## Token endpoint

The answer is JSON `{access_token, token_type: "Bearer", expires_in, refresh_token?, scope?,
id_token?}` with `Cache-Control: no-store`.

- `refresh_token` is present when the client holds the `refresh_token` grant.
- `scope` is returned on the code exchange; the refresh answer omits it.
- `id_token` is returned on the code exchange when the granted scope includes `openid`.

Refresh tokens rotate on every use. A client may refresh only tokens of its own families. A reuse
of an already rotated token outside the 30 s grace window kills that family only.

Errors follow RFC 6749 §5.2: `invalid_request`, `invalid_grant`, `unauthorized_client` and
`unsupported_grant_type` are 400; `invalid_client` is 401 (with `WWW-Authenticate: Basic` only
when the request used Basic). A database outage is 503 `temporarily_unavailable`, never
`invalid_grant`.

## Revocation

`/revoke` takes a refresh token or an access token of the calling client and ends its whole token
family. An unknown token, or another client's, also gets 200 and revokes nothing.

## Tokens

**Access token:** ES256 JWT, header `typ=at+jwt`, 15 minutes (`accountsapi::ACCESS_TTL_MINUTES`).
Claims: `iss`, `sub` (player id), `aud` (the client's audience), `client_id`, `sid` (token
family), `iat`, `exp`, `jti`, `preferred_username` (the player's handle, always present), and
`root_domains` when the client has any.

**ID token:** the same key, header `typ=JWT`, `aud` = `client_id`, the same lifetime. Claims:
`iss`, `sub`, `aud`, `iat`, `exp`, `auth_time`, `nonce` (when the request sent one),
`preferred_username`. Swarog's verifiers refuse it as an access token by its `typ`.

## Relying parties outside Swarog (Lore)

loreserver verifies access tokens itself against the JWKS.

- Set `jwt_typ = ["at+jwt"]` in its OIDC settings, so an ID token can never pass as an access
  token.
- loreserver does not pull Swarog's revocations. A revoked Lore-audience token stays valid there
  until its `exp`, at most 15 minutes. Inside Swarog the gateway refuses it within one
  `token_state` pull (5 s).
