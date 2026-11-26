# OAuth key-list and mount drift fixes

Status: done (2026-11-26). Approved by the owner 2026-11-26 ("ok mozesz zaczynac"); Q1 approved: clear `EPIC_*` by prefix too.
Date: 2026-11-26
Source: owner answer on the google-browser-login leftovers ("ok 1,2 zrobic reszta do todo", 2026-11-26).
Parent: docs/plans/2026-11-26-1123-google-browser-login-plan.md (landed 06b4c873..959671f9).
Plan review: core-reviewer (opus), ACCEPT with changes; folded in below.

## Problem

1. The `GOOGLE_*` key list is written by hand in five places:
   `modules/accounts/src/providers.rs` `GOOGLE_VARS` (:435), `tools/processctl/src/fleet.rs`
   `ACCOUNTS_OVERRIDEABLE_ENV` (:29-30), `MONOLITH_BASE_ENV` (:53-54), `PROOF_CLEARED_GOOGLE_ENV`
   (:89-92) and `tools/processctl/src/fleet_tests.rs` `GOOGLE_ENV` (:250-253). A new key added to
   `GOOGLE_VARS` and the allowlists but not to the clear list leaks the operator's ambient value into
   both proof fleets while every test stays green. The Epic keys repeat the same allowlist copies.
2. `modules/accounts/src/tests/google_browser.rs` `serve` (:88) and
   `modules/accounts/src/tests/epic_browser.rs` `serve_limited` (:65) rebuild by hand the routers
   `init` mounts (`modules/accounts/src/lib.rs` :1680, :1696, :1709). Dropping a real mount from
   `init` keeps those tests green.

## Decisions

- D1. The provider key lists move to the accounts contract crate: `EPIC_ENV_KEYS` and
  `GOOGLE_ENV_KEYS` as `pub const` slices in `api/accounts/api/src/lib.rs`, beside the existing
  harness-read constants `LOGIN_LIMIT_PER_SEC`/`LOGIN_LIMIT_BURST` (:41-49). `providers.rs`
  `EPIC_VARS`/`GOOGLE_VARS` are replaced by them (no second copy). processctl gains `accountsapi` as a
  DEV-dependency only, for an anti-drift test, following the `scheduler`/`asyncevents`/`invalidation`
  dev-dep precedent (tools/processctl/Cargo.toml:38-46, fleet_tests.rs:394-406); verifyctl's
  supported-targets stage does not build dev-deps (tools/verifyctl/src/stages/supported_targets.rs:29-35).
- D2. `PROOF_CLEARED_GOOGLE_ENV` is deleted. Both proof overlays drop every env key that starts with
  one `const GOOGLE_ENV_PREFIX` (`retain` on the service env before the pins), so no list exists to
  drift. With nothing ambient set the proof env is unchanged.
- D3. A new fleet test asserts set equality between the `EPIC_*`/`GOOGLE_*` keys of
  `ACCOUNTS_OVERRIDEABLE_ENV`, the same subset of `MONOLITH_OVERRIDEABLE_ENV`, and
  `accountsapi::EPIC_ENV_KEYS` + `GOOGLE_ENV_KEYS`; a missing or extra key fails and is named.
  `fleet_tests.rs` `GOOGLE_ENV` is replaced by `accountsapi::GOOGLE_ENV_KEYS`, and the `"GOOGLE_"`
  literals at fleet_tests.rs:277/290 use `GOOGLE_ENV_PREFIX`.
- D4. accounts gains one `pub(crate)` builder returning the whole hosted-browser router: the hosted
  pages, the Epic routes when Epic's flow is given, the Google browser callback when Google's flow is
  given, from the service, `cookie_secure`, the shared `LoginLimiter` and the two optional flows.
  Each part keeps its own layers (the security headers stay on `web::router` only, web/mod.rs:245;
  the limiter stays inside the start handlers); nothing is layered on the merged router. `init`
  mounts its result once; `ctx.mount` merges (core/lifecycle/src/context.rs:137-141), so the served
  route set is unchanged. The two browser test files' `serve` helpers call the builder.
- D5. Everything else from the leftover list goes to `docs/todo/accounts.md` under
  `### Known limits (google-browser-login step 4)`, each with a source.

## Owner question

- Q1. Epic has the same ambient-leak gap: the split proof pins six `EPIC_*` keys, so an ambient
  `EPIC_AUTHORIZE_URL` survives; the monolith proof pins three (fleet.rs:1077-1079), so an ambient
  `EPIC_CLIENT_SECRET`/`EPIC_REDIRECT_URI`/`EPIC_AUTHORIZE_URL`/`EPIC_TOKEN_URL` survives.
  Recommended: clear `EPIC_*` by prefix too in Step 1 (one more prefix, same mechanism, no change when
  nothing ambient is set). Otherwise it goes to the TODO in Step 3.

## Steps

### Step 1 [core-implementer, opus, high]: one key list, cleared by prefix, pinned by a test

D1, D2, D3 (and Q1 if approved). Files: `api/accounts/api/src/lib.rs`, `modules/accounts/src/providers.rs`,
`tools/processctl/Cargo.toml` (dev-dep + its comment naming `accountsapi` and why),
`tools/processctl/src/fleet.rs` (incl. the doc link at :95-96 to the deleted list),
`tools/processctl/src/fleet_tests.rs`, `tools/verifyctl/src/stages/supported_targets.rs` (comment
:31-32 naming the dev-deps).
Invariants:
- With no ambient provider keys the proof fleet env is identical to today's in both topologies; the
  existing `proof_monolith_drops_every_ambient_google_key` and
  `proof_split_pins_google_to_the_oidc_fixture_over_every_ambient_key` stay green.
- A key added to `GOOGLE_ENV_KEYS` without both allowlists fails the new test; once in both
  allowlists it is cleared in both proof overlays with no other edit.
- No new normal dependency in processctl.
Verify: `cargo test -p processctl`, `cargo test -p accounts --lib providers` (the provider parse tests),
`cargo clippy -p processctl -p accounts -p accountsapi --all-targets`.

### Step 2 [core-implementer, opus, high]: one builder for the hosted-browser router

D4. Files: `modules/accounts/src/lib.rs`, `modules/accounts/src/web/mod.rs` (or another accounts
file where the builder fits), `modules/accounts/src/tests/google_browser.rs`,
`modules/accounts/src/tests/epic_browser.rs`.
Invariants:
- `init` serves the same routes, with the same limiter shared by the hosted form and the Epic routes;
  neither callback is limited and neither gains the hosted pages' headers.
- New test: with the shared bucket emptied, the Google and the Epic callbacks answer non-429.
- The two browser test files reach the routes only through the builder.
Verify: `cargo test -p accounts` (live local Postgres, one rollout), `cargo clippy -p accounts --all-targets`.
At-risk tests: `tests/google_browser.rs`, `tests/epic_browser.rs`, `tests/hosted_login.rs`.

### Step 3 [main session, docs]: record the rest as TODOs

D5. `docs/todo/accounts.md`, `### Known limits (google-browser-login step 4)`:
- the proof redirect URI (gateway port 8082) is written out at fleet.rs:990, fleet_tests.rs:298 and
  tools/splitproof/src/accounts_browser_checks.rs:73 (Epic's likewise);
- the `browser_router` non-browser 500 arm (oauth_flow.rs:332-333) has no test;
- `google.oauth` is checked twice (providers.rs:194 in `browser_flow` and lib.rs:~1699 in `init`);
- the Google and Epic callbacks send no `Cache-Control: no-store`;
- no split run with a deliberately broken `provider = $3` predicate has watched `[BG3]` go red;
- `init` alone decides which flows reach the builder, so dropping a flow there keeps the browser
  tests green;
- (if Q1 is declined) the ambient `EPIC_*` leak above.
compose.yaml/.env.example lacking the provider keys is already PROCESSCTL-09 (docs/todo/processctl.md:58);
no edit there.

## Not covered

- `tests/hosted_login.rs:11-12` still builds `web::router` alone; it tests the hosted pages only.
- No split proof run: neither step changes proof behaviour with a clean shell (fleet_tests cover Step 1).
