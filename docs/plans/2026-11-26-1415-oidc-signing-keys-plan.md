# OIDC signing keys, discovery and JWKS (roadmap Step 2)

Status: approved by the owner 2026-11-26 ("ok mozesz zaczynac"); runs after the oauth-drift-fixes plan.
Date: 2026-11-26
Parent: docs/plans/2026-11-24-0004-lore-integration-roadmap-plan.md, Phase A, Step 2.
Owner answers (2026-11-26): 1 encrypt the private keys (option b) unless much more work; 2 issuer from
`ACCOUNTS_OIDC_ISSUER`; 3 rotation deferred to roadmap Step 9; 4 discovery publishes only `issuer` and
`jwks_uri` now; 5 signing reads the active key from the DB with a ~60 s cache; 6 no ReadyCheck, boot
guarantees a key.
Plan review: core-reviewer (opus), ACCEPT with changes; P1-P9 folded in below.
Sequencing: after docs/plans/2026-11-26-1352-oauth-drift-fixes-plan.md (both edit
`tools/processctl/src/fleet.rs` env lists).

## Problem

Steps 3 and 4 of the roadmap sign tokens (`at+jwt` for the Lore audience, every `id_token`), and Lore
verifies them through discovery and a JWKS. accounts has no signing key, no issuer and neither route.
The only ES/RS signer in the tree is the split proof's test IdP fixture (tools/splitproof/src/idp.rs:19-90).

## Facts (verified)

- `jsonwebtoken` 9 is ring-backed (`Cargo.toml:242`); `EncodingKey::from_ec_der` takes a PKCS#8 DER
  key. ring 0.17.14 is already in Cargo.lock (via rustls/rcgen) but has no workspace entry;
  `aws-lc-rs` must stay out (Cargo.toml:262-287, core/app/src/lib.rs:1448).
- accounts `migrate` (modules/accounts/src/lib.rs:1545-1624) gets only `ctx.db()`; module migrates run
  under the cross-process `MODULE_MIGRATE_LOCK_KEY` session lock (core/lifecycle/src/app.rs:120-218), so a
  row inserted there is inserted once across replicas. Precedent for seeding in migrate:
  modules/apikeys/src/lib.rs:277. `Accounts::new()` (lib.rs:1485-1491) reads no env and cannot fail;
  env is read in `register` (lib.rs:1511-1519, `Service` built at :1527) and `init` (lib.rs:1627-1655).
  Only cmd/server/src/lib.rs:53 and cmd/accounts-svc/src/lib.rs:14 construct `Accounts`, so accounts-svc is
  the split's only migrator of schema `accounts`. Tests: modules/accounts/src/tests.rs:640-643
  (`ensure_schema`, real `migrate` on schema `accounts`) and modules/accounts/src/tests/delete.rs:449
  (`migrate` with no `register`). Bare `migrate` calls take no advisory lock.
- Five gates run `register`/`init` on accounts with a lazy pool and no accounts env, never `migrate`:
  tools/checkmodules/src/manifests.rs:63-67, tools/routecheck/src/main.rs:197-201,
  tools/topiccheck/src/main.rs:400-404, tools/requirecheck/src/main.rs:187-193,
  tools/conformance/src/main.rs:116-129. Conformance T6 requires a bad env value to fail `App::build()`
  (conformance/src/main.rs:16-17); accounts' `EnvCase` rows are tools/conformance/src/policy.rs:216-258.
  `app::run` always migrates (core/app/src/lib.rs:1094).
- Accounts tests, devctl, the split proof, the C# stage and both weles fleets share the default DSN
  (fleet.rs:452 `DEFAULT_DATABASE_URL`, splitproof/src/main.rs:667, csharp.rs:28, weles `passthrough`),
  so they share one `accounts.signing_keys` table.
- `jsonwebtoken::EncodingKey::from_ec_der` never fails (jsonwebtoken-9.3.1/src/encoding.rs:85); it signs
  through ring `EcdsaKeyPair::from_pkcs8` (crypto/ecdsa.rs:35), which accepts ring's own PKCS#8 output.
- archcheck rule 22 (tools/archcheck/src/main.rs:1894-1933) needs the table in a `*_DDL` const passed to
  `Ddl::sql` and the partial unique index through `Ddl::index`; routecheck needs nothing (routes are
  unconditional, routecheck main.rs:95-113).
- `refuse_stale_oauth_states` (lib.rs:282-304) is the stale-shape refusal pattern.
- The gateway proxy strips `Host` and adds only `X-Forwarded-For` (modules/gateway/src/proxy.rs:41-51,146),
  so the issuer cannot be derived from the request; gateway-svc already passes `/accounts/oidc` through
  (cmd/gateway-svc/src/addrs.rs:231-249).
- Env reaches accounts through processctl `ACCOUNTS_OVERRIDEABLE_ENV` / `MONOLITH_BASE_ENV`
  (tools/processctl/src/fleet.rs:25-31,47), the proof overlays (fleet.rs ~927-1012 split, ~1049-1079
  monolith), weles/fleet.monolith.toml:33, weles/fleet.split.toml:49, compose.dev.yaml:25-26,
  tools/verifyctl/src/stages/csharp.rs:222-226 and compose.yaml:22-34. Lower bound from a grep for the
  existing `ACCOUNTS_*` keys; Step 1 re-greps.

## Decisions

- D1. Table `accounts.signing_keys`: `kid text PRIMARY KEY`, `alg text` (CHECK = `ES256`),
  `sealed_key bytea` (AEAD ciphertext of the PKCS#8 DER private key), `nonce bytea`, `public_jwk jsonb`,
  `status text` (CHECK in `active`/`retiring`/`retired`), `created_at timestamptz`. A partial unique
  index allows at most one `active` row. A table of another shape is refused at boot with the
  `DROP TABLE accounts.signing_keys` remedy (`refuse_stale_oauth_states` pattern). DROP + boot fresh
  invalidates issued tokens; acceptable (roadmap Step 2 (c)).
- D2. Encryption (owner answer 1, option b): the PKCS#8 key is sealed with ring `AES_256_GCM` under a
  key-encryption key from the new required env `ACCOUNTS_SIGNING_KEK` (standard base64 of exactly 32
  bytes, e.g. `openssl rand -base64 32`). A fresh random 96-bit nonce per key; the AAD is the `kid`, so
  a ciphertext moved to another row does not open. Missing, blank or malformed fails startup naming the
  variable; there is no default and no dev fallback inside accounts. The KEK never appears in a log or
  error text.
- D2a. Lifecycle phase (review P1): `Accounts::new()` keeps reading nothing. `register` parses
  `ACCOUNTS_OIDC_ISSUER` and, when set, `ACCOUNTS_SIGNING_KEK`; a malformed value fails `App::build()`.
  A missing KEK fails in `migrate` (which reads it itself when `register` did not run, as in
  tests/delete.rs:449), so the five `register`/`init`-only gates stay green with no accounts env and a real
  boot still fails. The KEK is held by `Accounts`, not `Service`. Tests pass the KEK through a
  constructor such as `Accounts::with_signing_kek`; no test mutates process env.
- D2b. One dev KEK (review P2): `testdb::DEV_ACCOUNTS_SIGNING_KEK` (accounts already dev-depends on
  testdb, modules/accounts/Cargo.toml:61). Every accounts test that touches schema `accounts` seals with
  exactly it, so a test run and a dev fleet never lock each other out of the shared table. processctl
  mirrors it as a plain const pinned by an anti-drift test through a `testdb` dev-dependency (the
  asyncevents/invalidation/scheduler mirror precedent, tools/processctl/Cargo.toml:38-46). The copies in
  the weles tomls, compose.dev.yaml and csharp.rs are each pinned to it by a test in a crate that can
  read them. Tests that need an empty table (first boot, stale shape, wrong KEK) run on a scratch
  schema, so the key code takes a schema parameter like `refuse_stale_oauth_states(pool, schema)`
  (tests/oauth_flow.rs:81).
- D3. Boot guarantees a key (owner answer 6): `migrate`, after the DDL and the stale-shape refusal,
  inserts, with `INSERT … ON CONFLICT DO NOTHING` against the one-active partial index and then a
  re-read (review P4: bare test `migrate` calls hold no lock), an ES256 key (ring `EcdsaKeyPair::generate_pkcs8`, `ECDSA_P256_SHA256_FIXED_SIGNING`) when no
  `active` row exists. `kid` is the RFC 7638 JWK thumbprint (base64url SHA-256). Then it opens every
  `active`/`retiring` row with the KEK and parses it with ring `EcdsaKeyPair::from_pkcs8` (not
  `EncodingKey::from_ec_der`, which cannot fail); a row that does not open fails
  startup with the remedy "restore the previous ACCOUNTS_SIGNING_KEK, or DROP TABLE
  accounts.signing_keys (invalidates issued tokens)". No ReadyCheck.
- D4. Issuer (owner answer 2): `ACCOUNTS_OIDC_ISSUER`, default `http://localhost:8080/accounts/oidc`
  (the monolith's port, matching `DEFAULT_REDIRECT_ORIGIN`, providers.rs:422). It must be an absolute
  `http`/`https` URL whose path is exactly `/accounts/oidc` with no query, fragment or trailing slash;
  anything else fails startup. The split proof pins `http://127.0.0.1:8082/accounts/oidc` (gateway-svc),
  the monolith proof `http://127.0.0.1:8080/accounts/oidc` (fleet.rs:1039).
- D5. Routes on the existing prefix via `ctx.mount`, both topologies:
  `GET /accounts/oidc/.well-known/openid-configuration` → `{"issuer": <issuer>, "jwks_uri": <issuer>/jwks}`
  only (owner answer 4; A3/A4 add their endpoints);
  `GET /accounts/oidc/jwks` → `{"keys": [...]}` with every `active` and `retiring` key as
  `kty EC, crv P-256, x, y, kid, use sig, alg ES256`, never a private member. A DB error is 503, not an
  empty key set.
- D6. One `SigningKeys` component reads the key rows from the DB behind a ~60 s cache (owner answer 5)
  and serves the JWKS. The cache clock is injected or a paused tokio clock in tests (no real-clock
  race). An expired cache plus a DB error answers 503; a stale set is never served. Its `sign` (active key, ES256 compact JWS, `kid` header) lands with its first
  caller in A3; A2 ships only what a route or boot uses, so no dead code.
- D7. Launchers: both new keys join the accounts allowlists; every dev and proof launcher pins a fixed
  dev KEK of D2b and the proof issuers of D4. `compose.yaml` passes both keys through bare (no `:?`, which
  would also fire under `-f compose.yaml -f compose.dev.yaml`; precedent compose.yaml:9); accounts' own
  boot refusal is the guard. `compose.dev.yaml` sets the dev values. The operator checklists in
  `install.sh:52` and `install.ps1:66` name both keys.
- D8. Rotation and KEK rotation are not in A2: the `retiring`/`retired` statuses exist so Step 9 needs
  no schema change (owner answer 3). Recorded in the roadmap Step 9 and docs/todo/accounts.md.

## Steps

### Step 1 [core-implementer, opus, high]: launchers carry the two new keys

D7, the launcher half of D2b. Files: testdb (the constant), `tools/processctl/Cargo.toml` (testdb
dev-dep), `tools/processctl/src/fleet.rs`, `tools/processctl/src/fleet_tests.rs`, `weles/fleet.monolith.toml`,
`weles/fleet.split.toml`, `weles/master/src/manifest_tests.rs` (pins the accounts env at :113-114,
:367-368), `compose.dev.yaml`, `compose.yaml`, `install.sh`, `install.ps1`,
`tools/verifyctl/src/stages/csharp.rs`, plus any launcher a fresh grep for `ACCOUNTS_COOKIE_SECURE`
finds that boots accounts.
Invariants:
- accounts ignores both keys until Step 2, so nothing changes at runtime.
- devctl, both proof fleets, both weles fleets, compose.dev and the C# stage all set the dev KEK; both
  proof fleets set their issuer.
- fleet tests pin the new keys in both topologies; every copy of the dev KEK is pinned to the testdb
  constant by a test.
- No new normal dependency in processctl.
Verify: `cargo test -p processctl -p devctl -p weles-master -p weles -p verifyctl` (one rollout at a
time), `cargo clippy` on those crates `--all-targets`.

### Step 2 [core-implementer, opus, high]: sealed signing keys created and opened at boot

D1, D2, D2a, D2b (test half), D3. Files: `Cargo.toml` (workspace `ring = "0.17"`), `modules/accounts/Cargo.toml`,
`modules/accounts/src/lib.rs`, a new accounts file for the keys, accounts tests, `tools/conformance/src/policy.rs` (`EnvCase`s for both keys).
Invariants:
- First boot on an empty schema leaves exactly one `active` row; a second boot adds none; two concurrent
  migrates leave one.
- A wrong KEK, a missing/blank/malformed KEK and a stale table shape each fail startup with the named
  remedy and write no row.
- A malformed KEK or issuer fails `App::build()`; a missing KEK fails only at `migrate`; the five
  `register`/`init`-only gates pass with no accounts env.
- After `cargo test -p accounts`, a dev fleet with the dev KEK still boots on the same database.
- Seal/open round-trips; a changed `kid` (AAD), nonce or ciphertext does not open.
- No `aws-lc-rs` in `cargo tree -p accounts`.
At-risk tests: modules/accounts/src/tests.rs:640-643 and tests/delete.rs:449; the gates of the Facts list.
Verify: `cargo test -p accounts` (live local Postgres, one rollout), then
`cargo test -p conformancecheck -p routecheck -p topiccheck -p requirecheck -p checkmodules`,
`cargo clippy -p accounts -p conformancecheck --all-targets`.
Review: core-reviewer, plus proof-auditor on the conformance `EnvCase` change (a verify gate).

### Step 3 [core-implementer, opus, high]: issuer, discovery and JWKS

D4, D5, D6. Files: `modules/accounts/src/lib.rs`, the keys file, a routes file, accounts tests.
Invariants:
- The discovery document holds exactly `issuer` and `jwks_uri`, `jwks_uri` = issuer + `/jwks`.
- The JWKS lists the active and retiring keys and never a `d` member; a retired key is absent; the
  listed public key verifies an ES256 signature made in the test with the opened private key.
- A JWKS read inside the cache window does not query the DB (injected or paused clock); a DB error
  past the window is 503.
- Every malformed `ACCOUNTS_OIDC_ISSUER` form in D4 fails startup.
Verify: `cargo test -p accounts`, `cargo clippy -p accounts --all-targets`.

### Step 4 [test-author, opus, high]: split proof through gateway-svc

After Steps 1-3 are final. `tools/splitproof`: through gateway-svc in the split and the monolith front,
discovery returns the pinned issuer and a `jwks_uri` that resolves; the JWKS has one ES256 key whose
`kid` matches the stored row; restarting accounts-svc keeps the same `kid`. The previously-wrong branch is
a missing or unproxied route (404).
Verify: one split proof run; report new failures against the current expected-red list.

### Step 5 [main session, docs]

`docs/todo/accounts.md` (signing key rotation and KEK rotation, sourced to owner answer 3), the roadmap
Step 9 line naming key rotation, `docs/roadmap/feature-tracker.md` accounts row, `.claude/rules/domain-modules.md`
accounts entry (the two new env keys).

## Not covered

- No token is signed in A2; the signer and its first caller come in A3.
- No KEK rotation tool; changing the KEK means DROP TABLE + boot fresh until Step 9.
- Discovery is not a full OpenID Provider metadata document until A3/A4 add the endpoints.
