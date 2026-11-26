# Google browser sign-in on the hosted login page

Approved by the owner 2026-11-26.

Roadmap: follows docs/plans/2026-11-24-2128-accounts-browser-login-plan.md (owner decision 4).

## Owner answers (2026-11-26)

1. One shared redirect flow with per-provider settings. Epic and Google both use it. Epic's routes and behaviour do not change.
2. Google is browser sign-in only, on the hosted page. The JSON start and the Link intent stay Epic-only. A TODO records that one player should later be able to link more identities (Google and others) whenever they want.
3. Same protection as Epic: state plus the binding cookie. No PKCE, no nonce.
4. Split proof at Epic's level. Unit tests cover success, using a local fake JWKS and token server. `[A6]` is adjusted, because Google stops being the unconfigured provider.

## What exists (anchors)

- The flow is in `modules/accounts/src/epic_oauth.rs`:
  - `EpicOAuth` is built at :77.
  - `new_state` is at :103, and `take_state` (DELETE ... RETURNING) at :140.
  - `authorize_url_for` is at :190, with scope `openid basic_profile` at :195.
  - `exchange_code` is at :203. It uses basic auth (:207).
  - The binding cookie is `epic_oauth_binding` (:38-42 and :233-259).
  - `router` is at :261. `handle_callback` is at :351.
  - `failed` is at :442. Browser intent goes to `LOGIN?epic=error&return_to=`; Login and Link go to `/?epic=error`.
  - `login` is at :456, and `browser_sign_in` at :482, with display name `epic:{short_id}` at :492.
- The state table is `accounts.oauth_states` (`lib.rs:252-263`). It has no provider column. The boot refusal for a table that predates `intent` is at `lib.rs:1586-1598`.
- Provider config is in `providers.rs`:
  - `EpicOAuthConfig` is at :148-176.
  - `EPIC_VARS` and `EPIC_OAUTH_VARS` are at :288-300, and `epic_from_vars` at :309-365.
  - `GOOGLE_VARS` (`GOOGLE_CLIENT_IDS`, `GOOGLE_JWKS_URL`) is at :369. The exact `GOOGLE_ISSUERS` list is at :376, and `google_from_vars` at :378-409.
  - `check_redirect_uri` (:470-479) hard-codes `/accounts/epic/callback`.
  - `provider_env_keys` (:281) is the only list of env keys that gets read.
- The hosted page is `web/mod.rs`:
  - `EPIC_ERROR` is at :34. `render_login` and `LoginView.epic` are at :143-150.
  - The routes are at :218-229, including `/accounts/oidc/epic`.
  - `login_page` maps `?epic=error` at :334. `epic_start` is at :399-446.
  - The template is `web/templates/login.html.tmpl:16-22`.
- Wiring is `lib.rs:1639-1670`. Google gets only a log line there (:1664).
- The gateway passthrough already covers `/accounts/oidc` (`cmd/gateway-svc/src/addrs.rs:231-248`), so the gateway does not change.
- In the fleet, `ACCOUNTS_OVERRIDEABLE_ENV` (`tools/processctl/src/fleet.rs:25-30`) and `MONOLITH_BASE_ENV` (:46-53, sized 18 and joined by `concat_env_18_10` :60-75) both carry the provider keys.
  - `PROOF_UNCONFIGURED_PROVIDER_ENV` (:83) clears Google. It is used for the split at :961 and for the monolith at :1037.
  - The split Proof Epic config is at :965-981. Its token URL is the dead port `http://127.0.0.1:1/token`.
  - The monolith Proof config is at :1043-1046. It sets no Epic secret.
- In splitproof:
  - `[EP1]`/`[EP2]` (`tools/splitproof/src/main.rs:4430-4467`) run in the split `assertions` only.
  - `[A6]` (:6922-6932) and `[P8]` (:7070-7117, the Google call at :7087, which expects `Unavailable`) run in both topologies through `federated_assertions`, called at :503 (monolith, `m`) and :5799 (split).
  - `accounts_browser_checks` runs in the split only (:5847).

## Decisions

- **D1. Module shape.** A new `modules/accounts/src/oauth_flow.rs` holds the provider-neutral flow:
  - the state store (`new_state`/`take_state`/prune);
  - the binding cookie;
  - the authorize URL;
  - the code exchange;
  - the browser callback (exchange, verify, `browser_sign_in`, `failed`).

  It is parameterized by a per-provider spec: provider name, scope, callback path, error query key and display-name prefix. `epic_oauth.rs` keeps only Epic's own surface: the JSON `/accounts/epic/start`, the Login and Link intents, and the `/accounts/epic/*` routes.
- **D1a. Who owns what.**
  - The full `Intent` enum (`login`/`link`/`browser`, the row shape) lives in `oauth_flow`.
  - The neutral callback runs `take_state`, then the exchange, then the verify, once. It handles `Browser` itself.
  - Any other intent goes to a provider hook. Only Epic supplies that hook, which keeps Epic's Login and Link handling and its `/?epic=error`/`/?epic=linked` redirects in `epic_oauth.rs`. D4's CHECK makes the hook unreachable for Google.
  - Nothing is exchanged or verified twice, and no `/?epic=…` string appears in `oauth_flow.rs`.
  - Each provider has ONE callback-path constant. The route and `check_redirect_uri` both use it, so the duplication between `epic_oauth.rs:281` and `providers.rs:472` is removed.
- **D2. Epic does not change.** The routes, the scope `openid basic_profile`, the `epic=error` keys, the `/?epic=error` and `/?epic=linked` redirects, the cookie name `epic_oauth_binding` and its path `/accounts` all stay the same. The binding cookie is shared by both providers under that name. Renaming it is not done: it would change Epic's behaviour for no gain.
- **D3. Google settings.**
  - Scope is `openid` only. Swarog needs nothing but `sub`, so no email or profile is requested.
  - Client auth is HTTP basic, the same as Epic. Google's token endpoint supports `client_secret_basic`.
  - The callback is `GET /accounts/oidc/google/callback` and the start is `POST /accounts/oidc/google`. Both are under the existing passthrough.
  - The error key is `google=error`, giving `LOGIN?google=error&return_to=...`.
  - The display name is `google:{short_id}`.
  - The issuer check is the existing exact `GOOGLE_ISSUERS` match.
- **D4. Google intents.** Google rows are only ever the `browser` intent. The DB enforces this with `CHECK (provider = 'epic' OR intent = 'browser')`.
- **D5. The state is scoped to its provider.**
  - `oauth_states` gets `provider text NOT NULL CHECK (provider IN ('epic','google'))`.
  - `take_state` deletes only `WHERE provider = $p`. A state minted for the other provider is a miss: 400, and the row is not consumed.
  - Rollout is DROP plus a fresh boot. The existing boot refusal (`lib.rs:1586`) is extended to refuse a table without `provider`, with the same remedy text.
  - The column check becomes a function that takes the schema name, so a test can run it against a scratch schema and never reshape the shared `accounts` schema.
  - The provider values are written from the `EPIC`/`GOOGLE` constants (`providers.rs:30`), never as literals.
  - **Sanctioned local step:** after Step 1 lands, run `DROP TABLE accounts.oauth_states;` once on the local test database. Without it every accounts test, and the split proof, fails at the refusal. Never work around this with an ALTER.
- **D6. Config.**
  - `GOOGLE_CLIENT_SECRET` turns the flow on.
  - `GOOGLE_OAUTH_CLIENT_ID` is required when the secret is set. It must be one of `GOOGLE_CLIENT_IDS`; if it is not, startup fails.
  - `GOOGLE_REDIRECT_URI` defaults to `http://localhost:8080/accounts/oidc/google/callback`. Its path must be exactly `/accounts/oidc/google/callback`, so `check_redirect_uri` takes the expected path.
  - `GOOGLE_AUTHORIZE_URL` defaults to `https://accounts.google.com/o/oauth2/v2/auth`.
  - `GOOGLE_TOKEN_URL` defaults to `https://oauth2.googleapis.com/token`.
  - The endpoint values are validated whether or not the secret is set, following Epic's rule. An OAuth var set without the secret is a startup error.
  - All five keys join `GOOGLE_VARS`, so `provider_env_keys` and the "any key present means Google is being configured" rule cover them.
  - `GOOGLE_OAUTH_VARS` holds `GOOGLE_OAUTH_CLIENT_ID`, `GOOGLE_REDIRECT_URI`, `GOOGLE_AUTHORIZE_URL` and `GOOGLE_TOKEN_URL`.
  - Checks run in this order: empty values, then the JWKS URL, then the OAuth endpoints, then client-id completeness and the list parse, then the secret against the OAuth vars, then membership.
  - The browser flow reuses the Google verifier (`GoogleConfig.verifier`, whose audiences are all of `GOOGLE_CLIENT_IDS`).
- **D7. The hosted page.**
  - `Web` holds the configured flows.
  - The Epic start handler becomes one start handler shared by both providers: limiter, then 404 if unconfigured, then CSRF, `replaces`, the state row, and a 303 with the binding cookie.
  - A "Continue with Google" button shows only when Google's flow is configured.
  - `login_page` maps `google=error` to its own message.
  - The Google callback mounts with the flow's router, like Epic's callback. It is not rate-limited, the same as Epic's.
- **D8. Proof topology.**
  - `PROOF_CLEARED_GOOGLE_ENV` lists all seven `GOOGLE_*` keys. Both overlays clear it first, so ambient shell values never reach a proof fleet.
  - The split overlay then pins Google against the loopback OIDC fixture:
    - `GOOGLE_CLIENT_IDS` and `GOOGLE_OAUTH_CLIENT_ID` are both `PROOF_GOOGLE_CLIENT_ID` (exported through processctl `lib.rs` next to `PROOF_OIDC_*`);
    - `GOOGLE_JWKS_URL` is `PROOF_OIDC_JWKS_URL`;
    - `GOOGLE_CLIENT_SECRET` is `test`;
    - `GOOGLE_REDIRECT_URI` is `http://127.0.0.1:8082/accounts/oidc/google/callback`;
    - `GOOGLE_TOKEN_URL` is `http://127.0.0.1:1/token` (the dead port).
  - The monolith overlay pins nothing for Google, so Google stays unconfigured there.
  - So the known-but-unconfigured 503 arm stays proven in the monolith: `[A6m]` stays 503 and `[P8m]` stays `Unavailable`.
  - In the split, `[A6]` asserts that Google is now configured: the garbage credential gets 401, and `nope` still gets 400. `[P8]` expects `Unauthorized` at the Google position.
  - **Cost:** the 503 arm is then proven in the monolith only. No other known provider is unconfigured in the split, and adding a provider only to keep this arm is out of scope.

## Steps

### Step 1 `[core-implementer, opus]` Generalize the redirect flow (Epic only, no behaviour change)
- **Do:** D1, D2 and D5, with Epic as the only provider.
  - Move the neutral parts into `oauth_flow.rs` with the provider spec.
  - Add the `provider` column, its CHECKs and the provider-scoped `take_state`.
  - Extend the boot refusal.
  - Leave `web/mod.rs` behaviour unchanged.
  - Follow D1a for ownership.
- **Tests:**
  - All existing `epic_browser.rs`, `hosted_login.rs` and the Epic unit tests in `tests.rs` stay green with no assertion edits. If an assertion has to change, stop and report.
  - New: a state row of another provider is refused by `take_state` and is not consumed. Write it as a direct store test: a `google` spec built in the test, with a Browser intent.
  - New: the boot refusal fires on an `oauth_states` without `provider`. The test runs the schema-parameterized check against a scratch schema it creates and drops. This also closes the Known limit "The boot refusal on an old `oauth_states` table has no test" in `docs/todo/accounts.md`; move it to the done file.
- **Verify:** first run the sanctioned `DROP TABLE accounts.oauth_states;` on the local database (D5), then `cargo test -p accounts` and `cargo clippy -p accounts --all-targets`.

### Step 2 `[core-implementer, opus]` Google flow config
- **Do:** D6 in `providers.rs`.
  - Add a `GoogleOAuthConfig` built through the same validation path as `EpicOAuthConfig`.
  - Make `check_redirect_uri` take the expected path.
  - Add the five keys to the fleet allowlists `ACCOUNTS_OVERRIDEABLE_ENV` and `MONOLITH_BASE_ENV`. The size goes from 18 to 23, so the concat helper is renamed to match.
- **Tests:** `providers_tests.rs` covers:
  - secret enables the flow;
  - OAuth var without the secret is an error;
  - secret without `GOOGLE_OAUTH_CLIENT_ID` is an error;
  - `GOOGLE_OAUTH_CLIENT_ID` not in `GOOGLE_CLIENT_IDS` is an error;
  - wrong callback path is an error;
  - empty values are errors;
  - HTTP to a non-loopback endpoint is an error;
  - the defaults.

  Epic's redirect-path tests stay green.
  - Add a conformance case for `GOOGLE_AUTHORIZE_URL` that goes through `from_env`, mirroring the Epic one at `tools/conformance/src/policy.rs:233-237`. This proves the new keys are actually read: `providers_tests` goes through `from_vars`, so it cannot catch a key missing from `GOOGLE_VARS`.
- **Verify:** `cargo test -p accounts -p processctl -p conformance` and clippy on all three.

### Step 3 `[core-implementer, opus]` Google on the hosted page
- **Do:** D3, D4 and D7.
  - Build the Google flow in `lib.rs` wiring when `config.google` has the OAuth half.
  - Add the start route, the callback, the button and the error message.
- **Tests:** a new `modules/accounts/src/tests/google_browser.rs` uses a local fake Google with JWKS, `/token` and issuer `https://accounts.google.com`, mirroring `epic_browser.rs`. It covers:
  - success: the session cookie is set, an identity `(google, sub)` exists, the redirect goes to `return_to`, and the replaced session is deleted;
  - a bad code gives a 303 to `LOGIN?google=error&return_to=...`, and `login_page` shows the Google message;
  - an Epic state on the Google callback gives 400, and the reverse also gives 400;
  - a missing or mismatched binding gives 400;
  - CSRF mismatch gives 403;
  - unconfigured gives 404 and no button;
  - the shared limiter refuses a drained client on the Google start.
- **Verify:** `cargo test -p accounts` and clippy.

### Step 4 `[test-author, opus]` Split proof
- **Do:** D8 in `fleet.rs`, then the checks.
  - Add the Proof constant `PROOF_GOOGLE_CLIENT_ID` next to `PROOF_OIDC_*`.
  - Re-point `[A6]` and `[P8]` for each topology as D8 says. Update the `federated_assertions` doc (main.rs:6909-6911), which says both topologies run identical assertions, and the comment at :6922-6925.
  - fleet_tests: the monolith Proof has no `GOOGLE_*` key even with all seven set in the ambient environment, and the split Proof has exactly its pinned values. The precedent is `proof_gateway_rate_limit_survives_an_ambient_override` (fleet_tests.rs:232).
  - Update the fleet.rs:78-83 doc.
  - New split checks in `accounts_browser_checks.rs`, all through gateway-svc:
    - `[BG1]`: the form shows the Google button. POST `/accounts/oidc/google` with the CSRF gives a 303 to an authorize URL carrying `state`, `client_id` and `redirect_uri`, and sets the binding cookie.
    - `[BG2]`: the callback with a bad code (dead-port token URL) gives a 303 to `LOGIN?google=error&return_to=...`.
    - `[BG3]`: a state minted by the Epic JSON start, presented on the Google callback, gives 400. The same state then redeems on Epic's callback with a 303 to `/?epic=error`, which proves the row was not consumed.
    - Each BG check uses its own X-Forwarded-For client, like the BR checks (`accounts_browser_checks.rs:75-77`), so the shared sign-in bucket's state cannot make it flaky.
- **Verify:** this is the only split-proof run, after Step 3 has landed and been reviewed. Run the full split-proof (both topologies). `cargo test -p processctl` runs first. If the local `accounts.oauth_states` still predates `provider`, run the sanctioned DROP (D5) first.

### Step 5 `[docs-writer, sonnet]` Docs and TODO
- `docs/roadmap/feature-tracker.md`:
  - Update the existing row at :148 ("Google and Apple browser-redirect OAuth"): Google is done, Apple stays open, and the PKCE/nonce wording is dropped (owner answer 3).
  - Fix :155, which says the proof fleet leaves Google unconfigured.
- Add the Google browser flow (keys and routes) to the reference doc that describes the accounts provider config, if one exists. Archives are never edited: `docs/plans`, `docs/reviews`, `docs/2026-07-07-2015-unified-operation-transport-status.md`, `experiments/`.
- `docs/todo/accounts.md`:
  - Move "Google server-side OIDC redirect flow on the hosted login page" to `accounts-done.md`.
  - Add **Link more identities to one player**: a signed-in player can attach further identities (Google, Epic, others) from the hosted page whenever they want. Today Google is sign-in only and Link is Epic's JSON flow. Source: this plan, owner answer 2 (2026-11-26).

## Not doing
- PKCE or a nonce. Owner answer 3.
- A Google JSON start or a Google Link. Owner answer 2; recorded as a TODO.
- Renaming `epic_oauth_binding` (D2).
- Email or profile claims from Google (D3).
- A third provider only to keep the 503 arm on the split (D8).

## Risks
- **Step 1 is a move-and-parameterize of security code.** Its review checks every point in this list:
  - the binding comparison;
  - single redemption;
  - the TTL prune;
  - `replaces`;
  - `failed` targets;
  - JSON Epic Login and Link still working.
- **Google accounts become sign-in identities.** A Google sign-in for an unknown `sub` creates a new player through `external_browser_login`, the same as Epic. Studio starter currency applies (owner decision 5).
