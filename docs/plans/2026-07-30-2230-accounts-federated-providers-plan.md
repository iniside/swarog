# Seq #2a — federated-provider seam, Google, guest/device, promotion, refresh sessions

**Date:** 2026-07-30 22:30 · **Revision 2** (post-review, 2026-07-30 23:10)
**Tracker row:** [feature-tracker.md](../roadmap/feature-tracker.md) seq **#2a**
**Scope decisions:** the four "Open decisions" in the tracker, settled 2026-07-30 (see
[Decisions](#decisions-settled-before-this-plan)).
**Research:** five parallel read-only subagents, 2026-07-30 — contract/codegen surface,
consumer+gate inventory, module internals, session path + registry precedents,
`player.registered` graph. Every file:line below comes from that sweep, not from prose.
**Review:** one adversarial pass (`core-reviewer`, ultrathink) returned 14 findings, 3
blocking. Revision 2 addresses all 14; the material changes are called out in
[Revision 2 changelog](#revision-2-changelog) so the reasoning is not lost.

---

## Context

### Why extend `accounts` rather than add a module

The research-before-planning rule asks for an explicit "why not extend / depend on X" for
anything new. This plan adds **no module, no service, no admin section**. The new artifacts
are: one durable event contract (`player.promoted`), one in-module verifier registry, four
operations on an existing `#[rpc]` trait, and one table in an existing schema. The
overlapping systems were checked:

- **`accounts` itself** — owns `accounts.identities(provider, subject)` with a composite PK
  and no provider whitelist (`modules/accounts/src/lib.rs:96-116`), plus
  `Service::external_login(provider, subject, display_name)`
  (`modules/accounts/src/lib.rs:240`), `Store::link_identity` (`store.rs:182`),
  `Store::identity_lock_key` (`store.rs:46`) and the admin PROVIDERS column
  (`modules/accounts/src/admin.rs:77`) — **all already provider-generic**. The only
  Epic-specific state in the whole module is one field, `Service::epic:
  OnceLock<Arc<OidcVerifier>>` (`lib.rs:159`), and four hardcoded literals. A new module
  would have to re-own the identity table to add a provider. Rejected.
- **`apikeys`** — also mints show-once secrets and hashes them, which is exactly the guest
  device-secret shape. But its subject is an *operator-issued API key scoped to a role*, not
  a player identity; the gateway consults it on **every** request as an authorization
  policy, before any player identity exists (`modules/gateway/src/keys.rs`). A guest
  credential is an authentication factor for one player. Different lifetimes, different
  trust model, different consumer. We **copy its idiom** (server-minted secret, SHA-256
  digest + display prefix, plaintext shown exactly once) and do not extend the module.
- **`core/registry`** — the cross-module capability seam. A verifier is not a cross-module
  capability: no other module may resolve "the Google verifier", and putting it in the
  global registry would let one appear in a process that does not host accounts. It stays
  private to the module, mirroring `core/edge::Server`'s in-struct
  `HashMap<String, Handler>` idiom (`core/edge/src/server.rs:115-116`) including its
  panic-on-duplicate discipline (`server.rs:164-168`).
- **`config`** — the wallet starter grant already reads its knobs from `configapi::Config`
  (`modules/wallet/src/projection.rs:19-29`). Provider *credentials* (client ids, secrets,
  JWKS urls) are startup configuration, not live-reloadable operator knobs, and the module
  already reads them from env. We keep them in env but give them **one typed, validating
  parse authority** instead of seven scattered `env_or` calls (Step 1).

### The seven facts from research that shape every step below

1. **Path parameters work** (`path_args(param = "wildcard")`,
   `tools/rpc-contract-model/src/lib.rs:270-280`, live precedent `characters.delete`
   `/characters/{id}`), **but a wildcard route would collide**: `opsapi::pattern_overlaps`
   (`core/opsapi/src/lib.rs:639`) treats `Wild` as overlapping `Lit` at equal segment
   counts, so `POST /accounts/login/{provider}` overlaps `POST /accounts/login/epic` — a
   startup `bail!` (`modules/gateway/src/lib.rs:1074-1088`) *and* a routecheck OVERLAP
   failure. **Decision 1's chosen shape puts `provider` in the body under the literal path
   `/accounts/login/federated`**, which is 3 literal segments and overlaps nothing.
2. **The trait is the single authority.** `modules/accounts/src/ops.rs:18-26` loops
   `auth_rpc::operations(...)` — a new `#[http]` method automatically produces the op, the
   binding, the local invoker, the edge method, the describe entry and the remote client.
   No wiring step exists to forget.
3. **Contributions must stay unconditional.** `ops.rs:18` contributes all ops regardless of
   gating; gating lives at the impl's first statement (`lib.rs:358`, `:412`, `:475`), so
   monolith `opsapi::SLOT` == split describe-union. Routecheck's FRONT-PARITY invariant
   enforces it and `modules/accounts/src/tests/dev_auth_gate.rs:83` pins it. Every new op
   inherits this rule.
4. **Every `#[http]` op is automatically player-QUIC reachable** — the allow-list *is* the
   `#[http]` route table (`modules/gateway/src/lib.rs:767`). There is no opt-in; the only
   lever is not marking a method `#[http]`. All new ops are deliberately reachable.
5. **`Store::link_identity` owns and commits its own transaction and emits nothing**
   (`store.rs:182-210`). A guest→provider promotion is invisible to the durable plane
   today. This is what forces Step 9's refactor.
6. **`modules/apikeys/src/lib.rs:105` (`DEV_CLIENT_POLICY`) is the only hardcoded
   wire-method list outside golden files.** Policy matching is exact-string with no
   wildcards (`modules/gateway/src/keys.rs:143-145`), so a new op silently 403s for
   `dev-key-client` until it is added there.
7. **There is no session cache.** `modules/gateway/src/verifier.rs:94-121` wraps
   `dyn Sessions` directly — every authenticated request round-trips to accounts (local in
   the monolith, mTLS RPC in the split). Contrast `keys.rs:35-51`, which does cache. This
   is a deliberate pre-existing asymmetry and is a **non-goal** here (see Non-goals).

### Decisions settled before this plan

| # | Decision | Chosen | Consequence carried into the steps |
|:-:|---|---|---|
| 1 | Wire shape | One `login_federated(provider, credential)` + in-module verifier registry | Step 1 (seam), Step 5 (contract) |
| 1b | Fate of `login_epic` | **Replaced**, not kept alongside | Step 5's cascade: BREAKING public-api diff + 6 golden/policy sites |
| 2 | link/unlink policy | link lands in #2a (409 on a foreign identity, never merge); **unlink stays in #2b** | Step 9 |
| 3 | guest × wallet starter grant | Grant fires on **promotion**, not on bare guest registration | Steps 9, 11, 15 |
| 4 | Session model | **Rotating refresh token + short access token**, pinned to #2a | Step 13 |
| 4b | Guest credential | **Server-minted** device secret, shown exactly once | Step 7 |

### Non-goals of #2a — stated so they are not smuggled in

- **Apple** — its ES256 client-secret JWT, multi-audience bundle/service ids, form-POST
  callback and once-only `user` payload are #2b. Note that `"apple"` **is** listed in
  `KNOWN_PROVIDERS` from Step 1 (a declared-but-unconfigured name), because Step 5's status
  taxonomy needs a known name with no verifier and Step 15's `[A6]` needs one in the proof
  fleet. Declaring the name is not shipping the provider.
- **unlink** and the last-identity / merge policy — #2b.
- **A session verification cache in the gateway** — fact 7. Deferred not because the cost is
  unchanged (Step 13 *does* add a cost: a new hourly write path per client, see Known risk
  5) but because a cache has its own staleness semantics — a revoked session would survive
  its TTL, which interacts directly with Step 13's family-kill. That interaction deserves
  its own reviewed change, not a corner of this one.
- **Moving accounts' env parsing into `cmd/*` mains.** accounts reads env inside
  `Module::register`/`init` (`lib.rs:634`, `:687-720`), deviating from the gateway/TLS
  convention. Step 1 stops the deviation from *multiplying* (one typed, validating parse
  authority instead of 7 `env_or` calls) but does not relocate it. **Recorded as a known
  gap** in Step 16 rather than silently left.
- **Google/Apple *web* OAuth (browser redirect).** `epic_oauth.rs` hardcodes the callback
  path (`epic_oauth.rs:71-75`), has no PKCE and no `nonce`, and `accounts.oauth_states` has
  no `provider` column. #2a ships the **ID-token** path only (native/SDK clients). The web
  flow for a second provider is its own rollout.

---

## Step 1 — the verifier seam and a validating config authority `[opus]`

**(a) What.** New file `modules/accounts/src/providers.rs`. Move `VerifyError`
(`epic.rs:41-52`) here as the shared error taxonomy. Add:

```rust
/// Every provider name this build knows, configured or not. The naming authority —
/// separate from the configured-verifier map, so "typo" and "not configured" are
/// distinguishable outcomes rather than one `None`.
pub(crate) const KNOWN_PROVIDERS: &[&str] = &["dev", "epic", "google", "guest", "apple"];

pub(crate) struct VerifiedSubject { pub subject: String, pub display_name: String }

#[async_trait]
pub(crate) trait CredentialVerifier: Send + Sync {
    async fn verify(&self, credential: &str) -> Result<VerifiedSubject, VerifyError>;
    fn max_credential_bytes(&self) -> usize;
}

pub(crate) enum Resolution<'a> { Configured(&'a Arc<dyn CredentialVerifier>), KnownButUnconfigured, Unknown }

pub(crate) struct Providers { verifiers: HashMap<String, Arc<dyn CredentialVerifier>> }
impl Providers {
    pub(crate) fn insert(&mut self, name: &'static str, v: Arc<dyn CredentialVerifier>);
    pub(crate) fn resolve(&self, name: &str) -> Resolution<'_>;
}

pub(crate) struct ProviderConfig { /* one validated entry per present provider */ }
impl ProviderConfig {
    pub(crate) fn from_vars(vars: &BTreeMap<String, String>) -> anyhow::Result<ProviderConfig>;
    pub(crate) fn from_env() -> anyhow::Result<ProviderConfig>;   // thin wrapper over from_vars
}
```

`Providers::insert` **panics** on a duplicate name, with the same message shape as
`core/edge/src/server.rs:164-168`. Wrap `epic.rs`'s `OidcVerifier` in an
`impl CredentialVerifier` producing `display_name = format!("epic:{}", short_id(&subject))`
— the literal currently at `lib.rs:490`.

Replace `Service::epic: OnceLock<Arc<OidcVerifier>>` (`lib.rs:159`) with
`Service::providers: OnceLock<Arc<Providers>>`. `Auth::login_epic` (`lib.rs:468`) keeps its
signature and now reads through `resolve("epic")`, answering `Error::unavailable` on
`KnownButUnconfigured` (`Unknown` is unreachable from this method).

**(b) Why now / order.** This is the only step that can prove the seam **against the
existing contract and the existing test suite**. Every later step is an addition on top of
a registry that already dispatches one real provider correctly. Doing it after the contract
change would mix a refactor with a breaking API diff in one reviewable unit.

**(c) How — the non-mechanical moves.**

- **Seven `Service { … }` literals must be updated**, five of them in test code:
  `lib.rs:628` (production), `conformance.rs:24`, `tests.rs:216`, `tests.rs:600`,
  `tests/dev_auth_gate.rs:20`, `tests/dev_auth_gate.rs:138`, `epic_tests.rs:128`.
- **`ProviderConfig::from_vars` is the fix-the-authority move, and the authority it fixes is
  validation, not error propagation.** `OidcVerifier::new`
  (`epic.rs:91-102`) validates **nothing** — its single `?` is `reqwest::Client::builder()
  .build()`, which does not fail for arbitrary strings. So `EPIC_JWKS_URL=hunter2` today
  yields a silently-enabled provider that 503s forever on the first token, and the
  swallowed-error branch at `lib.rs:694-697` is **dead code**. `from_vars` is where the
  check belongs: per present provider, non-empty client id, `Url::parse(jwks_url)?` with an
  `https` scheme (`http` permitted only for a loopback host, matching
  `epic_oauth.rs:71-90`'s existing precedent), non-empty issuers, non-empty audiences.
  **Present-but-malformed ⇒ `Err` ⇒ startup fails. Absent ⇒ simply not inserted.**
  This is a semantic change (a previously-silent misconfiguration now fails loudly) — name
  it in the commit message and in Step 16's doc note (Fix-the-Authority rule 4).
- **`from_vars(&BTreeMap)` exists so Step 2 can prove the failing branch with zero shared
  state.** `from_env()` is a three-line wrapper. Testing config parsing by mutating process
  env in a parallel test binary is the save/restore hack this repo already carries in two
  places (`modules/audit/src/tests.rs:156`, `modules/admin/src/tests.rs:930`); do not add a
  third.
- Preserve the fail-closed shape: an unconfigured-but-known provider still answers
  `Error::unavailable` (503), because `tools/conformance/src/policy.rs:126-146` pins that
  status and `tests/dev_auth_gate.rs:116-121` asserts it directly.
- Keep `epic_oauth.rs`'s `Arc<OidcVerifier>` handle working — it takes the concrete
  verifier, not the trait object (`epic_oauth.rs:41-53`), so `ProviderConfig` must hand out
  both the trait object (for the registry) and the concrete `Arc<OidcVerifier>` (for the
  OAuth router). Do not force the OAuth flow through the trait — its `exchange_code` path
  needs `OidcVerifier` directly.
- **No dead code.** `clippy -D warnings` is a blocking stage: every method introduced here
  needs a Step-1 caller. `resolve` is called by `login_epic`; if a `names()`-style helper
  has no caller until Step 5, do not add it until Step 5.

**(d) Dispatch:** `[opus]` — `subagent_type: "core-implementer"`, `model: "opus"`. Public
contract unchanged, so **no gate re-bless in this step**; `cargo build`, `clippy -D
warnings` and the existing accounts tests must stay green.

---

## Step 2 — tests for the seam and the config authority `[test-author]`

**(a) What.** Tests covering Step 1's landed diff: `from_vars` rejecting a present-but-
malformed provider (unparseable JWKS url, non-https non-loopback scheme, empty client id,
empty audience list) — **each an `Err`**; an absent provider yielding `Ok` with no entry;
`Providers::insert` panicking on a duplicate name; `resolve` returning all three arms;
epic's end-to-end verification unchanged.

**(b) Why now / order.** Step 1 turns a silently-ignored misconfiguration into a startup
failure. That branch did not exist before and nothing else in the system will ever fail
loudly if it regresses.

**(c) How.** Table-driven over `from_vars(&BTreeMap)` — no process env, no save/restore. The
duplicate-insert test uses `#[should_panic(expected = …)]` against the message shape. The
epic-unchanged assertions reuse `epic_tests.rs`'s local-JWKS harness
(`serve_counting_jwks` `:48`, `token_with_kid` `:110`).

**(d) Dispatch:** `[test-author]`, `model: "sonnet"` — existing fixture patterns, no novel
harness.

---

## Step 3 — issuer/audience semantics decided, then Google registered `[opus]`

**(a) What.** `modules/accounts/src/epic.rs`, renamed to `oidc.rs` as this step's first move
(see (c)) — change the validation authority:

```rust
pub(crate) enum IssuerMatch { Exact(Vec<String>), Prefix(String) }

pub(crate) fn new(jwks_url: &str, issuer: IssuerMatch, audiences: Vec<String>)
    -> anyhow::Result<OidcVerifier>
```

`audience: String` → `audiences: Vec<String>` (`epic.rs:192`'s `set_audience(&[&self.audience])`
becomes `set_audience(&self.audiences)`). Then register a `google` provider through
`ProviderConfig`, with `IssuerMatch::Exact(vec!["https://accounts.google.com",
"accounts.google.com"])`.

**(b) Why now / order.** Adding Google before generalizing would either hardcode one
audience (wrong for a real Google client, which has separate web/iOS/Android client ids) or
add a second verifier type beside the first — the hack-on-hack signal. The generalization
is the authority fix; Google is then pure addition.

**(c) How.**

- **The issuer decision is made here, not discovered by a later test.** Today's check is
  `data.claims.iss.starts_with(&self.issuer_prefix)` (`epic.rs:197`). With
  `"https://accounts.google.com"` as a prefix, `https://accounts.google.com.evil.test`
  **passes** — and Google's key set signs for Google-hosted issuers, so the guard is not
  decorative. Google therefore gets `Exact` with both spellings (the scheme-less legacy form
  is a real Google issuer value, which is why the exact variant holds a list). Epic keeps
  `Prefix` so its behaviour is byte-identical — the variant exists to preserve one provider's
  documented semantics, not as a general-purpose escape hatch.
- **Rename `epic.rs` → `oidc.rs` FIRST, as the opening move of this step** (and
  `epic_tests.rs` → `oidc_tests.rs` with it). Revision 2 said to defer this to #2b on the
  grounds that "a rename churns six import sites for zero behaviour" — both halves are
  wrong. The file contains **zero Epic-specific code**: every occurrence of "epic" in its
  229 lines is a comment, and its own header already states "Epic is the first user, Google
  (also OIDC) is the second". The import sites are four (`epic_oauth.rs:29`,
  `providers.rs:21`, `providers_tests.rs:12`, `tests.rs:7`) plus the two `mod` lines in
  `lib.rs:27,840`. And "zero behaviour" is the wrong measure: the cost of deferring is that
  this step puts `IssuerMatch::Exact(["https://accounts.google.com", …])` inside a file
  named after a different provider, which is a contradiction a reader must resolve before
  trusting anything else in it. Renaming before Google lands is a four-line move; renaming
  after is the same move plus an explanation of why it was ever otherwise.
  While renaming, make the prose provider-neutral — the header, `Claims`'s "for Epic, the
  account/product user id" (`epic.rs:56`), the `MIN_REFRESH_INTERVAL`/`JWKS_CACHE_TTL`
  rotation notes, and `short_id`'s "`epic:<shortID>`" doc (`:222`), which is now the shape
  `providers.rs` builds for BOTH providers. `epic_oauth.rs` keeps its name: it implements
  Epic's browser redirect flow and is genuinely Epic-specific.
- `Claims` (`epic.rs:60-66`) carries only `iss`/`sub`. Google's `email`/`email_verified`
  stay unextracted: display name is `google:{short_id(sub)}`, matching the Epic convention
  at `lib.rs:490`. Extracting email would create an identity-uniqueness question that
  belongs to seq #4 (self-registration), not here.
- Required claims stay `["exp", "aud"]` with `jsonwebtoken`'s default 60s leeway
  (`epic.rs:195`). `nonce` is **not** implemented and is not needed on the ID-token path —
  it is a web-redirect-flow binding, and the web flow is a stated non-goal.
- An empty `audiences` vector must be impossible: `from_vars` (Step 1) already rejects it,
  and `OidcVerifier::new` asserts it — an empty audience list in `jsonwebtoken` means "any".
- **Fix `short_id`'s byte slice while this file is open** (`epic.rs:223`). It returns
  `&s[..8]` for any subject longer than 8 **bytes**, so an IdP whose `sub` carries a
  multibyte character straddling the 8th byte panics on the login path. Google is exactly
  the provider that widens the space of real subject values, and this step is what
  registers it — hence here, not "later". Cut on a character boundary
  (`s.char_indices().nth(8).map_or(s, |(i, _)| &s[..i])`) and keep the byte-length
  fast path if the signature stays `&str`. This is a pre-existing defect, not a Step-3
  regression: name it as such in the commit message.

**(d) Dispatch:** `[opus]` — `subagent_type: "core-implementer"`, `model: "opus"`.

> **Erratum from Step 1 (landed `38c1a7f`) — read before implementing.** Step 1's review
> required `EPIC_ISSUER_PREFIX` to be validated, and the closure chosen was
> `check_absolute_url` (parseable absolute URL + host). That rule **rejects
> `accounts.google.com`**, the scheme-less legacy Google issuer this step puts in
> `IssuerMatch::Exact`. So Step 3 cannot reuse `check_absolute_url` for issuers: each
> `IssuerMatch` variant needs its own rule — `Prefix` keeps the absolute-URL floor (it
> guards a `starts_with`, where a truncated value like `h` accepts every https issuer),
> while `Exact` accepts a bare host form because an exact comparison cannot be widened by
> truncation. The call site in `modules/accounts/src/providers.rs` carries a comment
> pointing here.

---

## Step 4 — tests pinning the issuer/audience semantics `[test-author]`

**(a) What.** Multi-audience accept (token for audience #2 of 3) and reject (audience not in
the list); both Google issuer spellings accepted under `Exact`;
`https://accounts.google.com.evil.test` **rejected** under `Exact`; the same string
**accepted** under `Prefix` — pinning that the variant choice, not an accident, is what
protects Google; Epic's single-prefix behaviour unchanged. Plus `short_id`'s truncation
branch, which nothing in the repo executes today: a subject longer than 8 bytes whose 8th
byte falls inside a multibyte character must yield a shortened name rather than panic
(the Step-3 fix), alongside the pass-through case for a subject at or under the threshold.

**(b) Why now / order.** Step 3 decided the semantics; this step makes the decision
executable, so a later "simplification" back to one shared prefix check fails a test rather
than silently widening a token check.

**(c) How.** Extend `tests.rs`'s self-minted-key harness (`test_key` `:73-182`, `serve_jwks`,
`sign`), table-driven like `oidc_verifier_accepts_valid_and_rejects_bad_claims`. The
`Prefix`-accepts-evil case is deliberately an assertion about the *variant*, not a
vulnerability — it documents why Google may not use it.

**(d) Dispatch:** `[test-author]`, `model: "sonnet"`.

---

## Step 5 — the contract: `login_federated` replaces `login_epic` `[opus]`

**(a) What.** `api/accounts/api/src/lib.rs` — remove `login_epic` (`:92-93`), add:

```rust
#[http(verb = "POST", path = "/accounts/login/federated", auth = "none", success = 200)]
async fn login_federated(&self, provider: String, credential: String) -> Result<Session, Error>;
```

Impl in `modules/accounts/src/lib.rs` dispatches through `Providers::resolve`. Then the full
cascade, in one commit:

| site | change |
|---|---|
| `modules/apikeys/src/lib.rs:105` `DEV_CLIENT_POLICY` | `accounts.loginEpic` → `accounts.loginFederated` |
| `tools/conformance/src/policy.rs:52-58` | input-cap rows re-keyed to `accounts.loginFederated{provider,credential}` |
| `tools/conformance/src/policy.rs:126-146` | the `InfraOutage503` `OutageCase` re-pointed at a **known but unconfigured** provider through the new op |
| `modules/accounts/src/conformance.rs:63-71` | probe renamed + re-pointed |
| `modules/accounts/src/tests/dev_auth_gate.rs:83,116-121` | `METHOD_LOGIN_EPIC` → `METHOD_LOGIN_FEDERATED`; the 503 assertion re-pointed |
| `api/accounts/rpc/src/tests.rs:69`, `modules/accounts/src/tests.rs`, `modules/accounts/src/epic_tests.rs` | code references to the old method |
| goldens | the Step-5 row of the [gate table](#gate-table-per-contract-touching-step) |

**(b) Why now / order.** The registry (Step 1) and both real OIDC providers (Step 3) exist,
so the new op has something to dispatch to on its first commit. Doing the contract first
would ship an op that can only answer 503.

**(c) How.**

- **Status taxonomy, decided here, and implementable because Step 1 built `Resolution`:**
  `Unknown` provider name → `Error::invalid` (400, caller-supplied garbage);
  `KnownButUnconfigured` → `Error::unavailable` (503, preserving the pinned conformance
  case); rejected credential → `Error::unauthorized` (401); IdP infrastructure failure → 503.
  The last two map `VerifyError::{Rejected, Infra}` exactly as `lib.rs:480-488` does today.
  Without `KNOWN_PROVIDERS` this distinction is unrepresentable — that is why it is in
  Step 1 and not here.
- **Input caps become per-provider.** Today the epic `id_token` cap is a single constant
  (`epic_id_token_within_cap`, cap 65536 per `tools/conformance/src/policy.rs`). With N
  providers the cap comes from `CredentialVerifier::max_credential_bytes()` — the verifier
  knows its own credential shape. `provider` itself gets a small fixed cap (64 bytes)
  checked **before** the registry lookup, so an unbounded string never becomes a `HashMap`
  key.
- **This is a BREAKING public-api diff.** `docs/reference/public-api-baseline/accountsapi.txt`
  loses `METHOD_LOGIN_EPIC`, both `LoginEpic{Request,Response}` DTOs and the trait method.
  Licensed by the repo's pre-production wipe-is-migration phase; name it explicitly in the
  commit message.
- **`login_epic`'s removal does not touch the Epic web-OAuth passthrough.**
  `/accounts/epic/start|callback` are HTTP-native browser routes, not operations
  (`epic_oauth.rs:252-272`), and splitproof's `[EP1]`/`[EP2]` keep passing untouched.
- Contribution stays unconditional (fact 3).

**(d) Dispatch:** `[opus]` — `subagent_type: "core-implementer"`, `model: "opus"`.
Gates: the Step-5 row of the gate table; `verifyctl --fast` before the commit.

---

## Step 6 — tests for `login_federated` `[test-author]`

**(a) What.** Dispatch per configured provider; unknown provider → 400; known-but-
unconfigured → 503 (both arms, distinctly); per-provider credential cap rejected **before**
any verifier call; oversized `provider` rejected before the lookup; the extended
`dev_auth_gate` parity assertion.

**(b) Why now / order.** Step 5 replaced a single-branch method with a dispatch table — the
new seam is the table itself, and its failure modes (unbounded key, the 400/503 arms
collapsing into one, cap checked after the network call) are all invisible on the happy path.

**(c) How.** The cap tests must prove the check happens *before* the verifier runs — use a
counting fake `CredentialVerifier` and assert **zero** invocations, the same
proof-by-construction shape as `epic_tests.rs`'s `serve_counting_jwks`. A test that merely
asserts a 400 does not distinguish "rejected early" from "rejected after a network call".

**(d) Dispatch:** `[test-author]`, `model: "sonnet"`.

---

## Step 7 — guest/device provider + `create_guest` `[opus]`

**(a) What.** New op on `accountsapi::Auth`:

```rust
#[http(verb = "POST", path = "/accounts/guest", auth = "none", success = 201)]
async fn create_guest(&self) -> Result<GuestSession, Error>;

pub struct GuestSession {
    pub player_id: String,
    pub token: String,
    pub refresh_token: String,          // see Step 13(c) — guests are the longest-lived clients
    pub access_expires_in_secs: i64,
    pub device_secret: String,
}
```

plus a `guest` entry in the registry whose credential is the opaque `"<subject>.<secret>"`
string returned inside `device_secret`.

**(b) Why now / order.** Minting a credential and verifying one are different operations
with different trust properties; `login_federated`'s dispatch table (Step 5) must exist
first so the *returning* guest needs no new code at all — it is one more registry entry.

**(c) How.**

- **Two ops, not one, and this is not the hack-on-hack smell.** `create_guest` mints and
  reveals a secret exactly once; `login_federated("guest", cred)` verifies it. This is the
  `apikeys` create-vs-use split applied to a player credential. Folding both into one op
  would mean an operation whose response shape changes with its input — the shape this
  repo's typed contracts exist to prevent.
- **`GuestSession` carries the refresh fields from birth.** Step 13 adds them to `Session`;
  a guest is the longest-lived client class and the one that cannot re-authenticate from an
  external IdP, so shipping it a non-renewable token would be the worst case of the new
  model. Declaring the fields now (populated with the pre-Step-13 values: the access token
  and its TTL, refresh empty) costs one golden re-bless and avoids a second DTO change.
- **Storage reuses the existing column.** `accounts.identities.secret_hash` is already
  `text NULL` and used only by the dev/password provider (`lib.rs:103`, written at
  `store.rs:91-120`, read at `store.rs:125-142`). The guest identity stores a SHA-256 digest
  of a 32-byte `OsRng` secret. **SHA-256, not argon2id** — the secret is full-entropy
  machine-generated (no dictionary attack to slow down), and argon2 here would consume the
  module's 2 `argon_permits` (`lib.rs:162`) on every guest login, converting an auth path
  into a scheduling bottleneck. `apikeys` made the same call for the same reason.
- **`store.rs:133`'s `WHERE i.provider = 'dev'` is the one hardcoded provider read.** Add a
  sibling lookup for guest rather than widening that query — the dev/password path resolves
  by *email* and must not accidentally match a guest subject.
- Subject is a server-minted UUID; the credential the client stores and replays is
  `"<subject>.<secret>"`, split on the first `.` (the base64url alphabet contains no `.`).
- **Unknown subject and wrong secret must be indistinguishable** — one 401, no oracle,
  matching the dev/password convention documented at `api/accounts/api/src/lib.rs:85-87`.
- `create_guest` emits `player.registered` with `provider = "guest"` through the existing
  `emit_registered_tx` (`lib.rs:307`), in the same transaction as the player+identity
  insert. This is what Step 11 filters on — and it is also unauthenticated durable-log
  growth, recorded in Known risk 4.

**(d) Dispatch:** `[opus]` — `subagent_type: "core-implementer"`, `model: "opus"`.
Gates: the Step-7 row of the gate table; `verifyctl --fast` before the commit.

---

## Step 8 — tests for guest/device `[test-author]`

**(a) What.** Mint → login round-trip; wrong secret → 401; unknown subject → the **same**
401 (no oracle); malformed credential (no `.`, empty halves) → 401 not a panic; the secret
is not re-derivable from any read path; a second `create_guest` yields a distinct player.

**(b) Why now / order.** The show-once property is a claim about what the *store* cannot
return; it needs an executed proof, because nothing else in the system will fail loudly if a
later change starts returning it.

**(c) How.** Live-DB tests behind `test_pool()`'s clean SKIP with the shared `SCHEMA_READY`
once-cell (`tests.rs:539+`). The no-oracle test asserts the two 401 responses are equal
byte-for-byte, not merely both-401.

**(d) Dispatch:** `[test-author]`, `model: "sonnet"`.

---

## Step 9 — `link` op + the `player.promoted` durable event `[opus]`

**(a) What.** Four coupled changes:

1. `api/accounts/events/src/lib.rs` — new contract:
   ```rust
   pub struct PlayerPromoted { pub player_id: String, pub from_provider: String, pub to_provider: String }
   pub static PLAYER_PROMOTED: LazyLock<EventType<PlayerPromoted>> =
       LazyLock::new(|| define("player.promoted", 1, HistoryPolicy::MinRetention { days: 7 }));
   ```
   plus its **fully-populated** entry in `golden_samples()` (`:46-57`) — `tools/topiccheck/src/golden.rs`
   asserts a bijection — and `of(accountsevents::PLAYER_PROMOTED.contract())` in
   `tools/topiccheck/src/main.rs:191-200`.
2. `modules/accounts/src/store.rs` — `link_identity` (`:182-210`) demoted to dumb
   tx-taking helpers: `link_identity_tx(&mut PgConnection, …) -> Result<LinkOutcome, StoreError>`,
   `has_real_identity_tx(&mut PgConnection, player_id) -> Result<bool, sqlx::Error>`, and
   `player_lock_key(player_id) -> i64` beside the existing `identity_lock_key` (`:46`).
3. `modules/accounts/src/lib.rs` — **`Service::link_identity(player_id, provider, subject)
   -> Result<LinkOutcome, Error>`**: the single tx-owning, lock-taking, emit-owning
   authority, the twin of `Service::external_login` (`:240-303`).
4. `api/accounts/api/src/lib.rs` — new op:
   ```rust
   #[http(verb = "POST", path = "/accounts/link", auth = "player", success = 200)]
   async fn link(&self, identity: Identity, provider: String, credential: String) -> Result<MeView, Error>;
   ```

**(b) Why now / order.** Fact 5: `link_identity` commits its own transaction and emits
nothing, so promotion is invisible to the durable plane. The event **cannot** be emitted
until that function hands its transaction out, and Step 11's wallet change cannot be written
until the event exists. This step is the hinge.

**(c) How.**

- **The lock must be on the PLAYER, not the identity — this is the correctness core.**
  `identity_lock_key` (`store.rs:46`) is keyed on `(provider, subject)` of the *new*
  identity, so it serializes writers of one identity, never writers of one player. Since a
  player may hold several identities (there is no `UNIQUE(player_id, provider)`), two
  concurrent links of *different* providers to the same guest take *different* locks; under
  READ COMMITTED neither sees the other's uncommitted row, `has_real_identity_tx` returns
  false in both, and **both emit `player.promoted`**. The fix is a second
  `pg_advisory_xact_lock(player_lock_key(player_id))`, and a **fixed acquisition order —
  player lock first, then identity lock — at every site that takes both**. `external_login`
  (`lib.rs:246-251`) takes the identity lock only and creates a *new* player, so it never
  takes both; if that ever changes, it adopts the same order.
- **The emit condition, in order, inside one transaction:** player lock → identity lock →
  `has_real_identity_tx` → insert the identity → if the player had no non-guest identity
  before **and** the new provider is not `guest`, `emit_tx` `player.promoted` on the same
  `&mut PgConnection`. Identity row and event append commit together or not at all. Do not
  compute "was a guest" from the request, from a second query after the insert, or from the
  caller.
- **`from_provider` is the constant `"guest"` in #2a**, because guest is the only promotable
  state this release ships. Say so in the field's doc comment rather than letting an
  implementer infer it from `has_real_identity_tx`'s `bool`.
- **Both callers go through `Service::link_identity`, never through a tx of their own.**
  `epic_oauth.rs:409` (`svc.store.link_identity(&p.id, "epic", &subject)`) is migrated to
  the Service method — otherwise the lock/read/emit sequence exists in two places and the
  "one authority" claim is false on the day it is written.
- **Policy (decision 2), as code:** an identity already bound to **this** player is an
  idempotent success (existing behaviour, `store.rs:191-200`) and emits **nothing**; bound to
  **another** player is `Error::conflict` (409) with **no merge** — merging accounts
  post-wallet means merging balances and is its own feature; a player linking a second
  identity of the *same* provider is allowed.

**(d) Dispatch:** `[opus]` — `subagent_type: "core-implementer"`, `model: "opus"`.
Gates: the Step-9 row of the gate table. Note that the advisory `topiccheck` stage
(`tools/verifyctl/src/stages/mod.rs:184-190` — blocking only under `--strict` or the
fortress stage's `--durability-strict`) reports the new topic as unsubscribed until Step 11;
the *blocking* reds in this window are the codegen/golden set, not topiccheck. Land 9 and
11 back-to-back regardless.

---

## Step 10 — tests for link + promotion `[test-author]`

**(a) What.** The previously-wrong branch is *"the event and the identity row can diverge,
or fire twice"*. Cover: a rolled-back link transaction leaves **no** `player.promoted` and
no identity row; a foreign identity → 409 with nothing written; re-linking the same identity
to the same player → idempotent and **no second event**; a guest linking a real provider →
exactly one event; a **non-guest** player linking a second real provider → none; and the
finding-3 case: **two concurrent links of *different* providers to one guest emit exactly
one `player.promoted`**.

**(b) Why now / order.** Step 9 moved a transaction boundary and introduced a two-lock
ordering. Both are invisible on the happy path, and the different-providers race is the one
that the obvious same-identity concurrency test (`tests.rs:896`'s shape) does **not** catch.

**(c) How.** Live-DB, using the existing `wired`/`registered_events` fixtures
(`tests.rs:539+`) to read the event log back. The race test spawns two tasks with two
distinct providers against one player id and asserts a count of exactly 1 after both commit
— outcomes, not timing ([[timing-sensitive-tests-doctrine]]). The rollback test forces the
failure *after* the emit and *before* commit, asserting the log is empty — the branch that
would silently pass if the emit ever moved outside the tx.

**(d) Dispatch:** `[test-author]`, `model: "sonnet"`.

---

## Step 11 — wallet grants on promotion; audit's 8th ledger topic `[opus]`

**(a) What.**

- `modules/wallet/src/lib.rs:216-236` — keep `wallet.player-registered.v1` but **skip when
  `provider == "guest"`**; add `wallet.player-promoted.v1` (`StartPosition::AfterRegistration`)
  on `player.promoted`, calling the same `grant_starter`.
- `modules/wallet/src/projection.rs` — `grant_starter` unchanged; its idempotency key stays
  `format!("starter:{player_id}")` (`:107-113`).
- `modules/audit/src/lib.rs` — `DURABLE_TOPICS` (`:46-54`) and `DURABLE_SPEC_IDS` (`:60-70`)
  gain `"player.promoted"` / `"audit.player-promoted.v1"` (positional zip), and
  `modules/audit/src/tests.rs:84-110`'s `durable_topics_match_events` `want` set follows.

**(b) Why now / order.** Closes the topiccheck hole Step 9 opens, and is the step that
actually implements decision 3.

**(c) How.**

- **Two subscriptions, one key, is the point — not an oversight.** A player who registers
  directly with a real provider must still be granted; a guest must not be granted until
  promotion. Both paths call `grant_starter` with the same deterministic
  `starter:{player_id}`, so a player traversing both gets `Outcome::Duplicate` — a no-op by
  construction (`projection.rs:101-121`). The alternative (moving the grant wholly onto
  promotion) would silently stop granting to every non-guest registration.
- **Do not edit the existing subscription's id or start position.** Both are `spec_hash`-immutable
  (`lib.rs:216-220`); changing behaviour *inside* the same id is legal, changing the id or
  the start is a new subscription.
- The guest filter reads the event's own `provider` field — already on the payload
  (`api/accounts/events/src/lib.rs:24-29`), so no contract change is needed.
- Keep every guard returning `Ok(())` rather than `Err` (`projection.rs:33-38`, `:75-105`):
  an `Err` backs off and, after 20 failures, **pauses the subscription for every subsequent
  player**.

**(d) Dispatch:** `[opus]` — `subagent_type: "core-implementer"`, `model: "opus"`.
Gates: topiccheck now green; no contract change, so no golden cascade.

---

## Step 12 — tests for the grant's moved trigger `[test-author]`

**(a) What.** Guest registration → **zero** ledger rows (the branch decision 3 exists for);
promotion → exactly one `starter:{player_id}` row; direct registration with a real provider
→ still granted (the regression this step most risks); a player traversing both paths →
still exactly one ledger row.

**(b) Why now / order.** The negative assertion is the entire point of decision 3 and no
existing test covers it — `[WL7]` asserts the opposite semantics for the registration path.

**(c) How.** Extend `modules/wallet/src/tests.rs` (starter-grant fixtures at `:966`, `:1091`,
`:1519`). The "zero rows" assertion must be a settled-state check, not a race against
delivery — poll for the *promotion* row to appear, then assert the guest-only count, so the
negative is proven after delivery has demonstrably run.

**(d) Dispatch:** `[test-author]`, `model: "sonnet"`.

---

## Step 13 — rotating refresh tokens + short access tokens `[opus]`

**(a) What.**

- DDL in `modules/accounts/src/lib.rs`'s `SCHEMA_DDL` (`:90-129`):
  ```sql
  ALTER TABLE accounts.sessions ADD COLUMN IF NOT EXISTS family_id uuid;   -- see (c)
  CREATE INDEX IF NOT EXISTS sessions_family_idx ON accounts.sessions (family_id);

  CREATE TABLE IF NOT EXISTS accounts.refresh_tokens (
      token       text PRIMARY KEY,
      player_id   uuid NOT NULL REFERENCES accounts.players(id) ON DELETE CASCADE,
      family_id   uuid NOT NULL,
      issued_at   timestamptz NOT NULL DEFAULT now(),
      expires_at  timestamptz NOT NULL,
      used_at     timestamptz,
      replaced_by text
  );
  ```
  plus `refresh_family_idx (family_id)`, `refresh_player_idx (player_id)`,
  `refresh_expires_idx (expires_at)`.
- `accountsapi::Session` gains `refresh_token: String` and `access_expires_in_secs: i64`
  (additive); `GuestSession`'s equivalents (declared in Step 7) start being populated.
- New op:
  ```rust
  #[http(verb = "POST", path = "/accounts/refresh", auth = "none", success = 200)]
  async fn refresh(&self, refresh_token: String) -> Result<Session, Error>;
  ```
- `store::SESSION_TTL_DAYS = 30` (`store.rs:11`) splits into `ACCESS_TTL_MINUTES = 60` and
  `REFRESH_TTL_DAYS = 30`; `insert_session_tx`'s `make_interval(days => $3)` (`store.rs:223`)
  becomes `mins => $3` with an `i32` minutes bind.
- `modules/accounts/src/epic_oauth.rs:433` and `demos/webui/src/index.html` learn the new
  shape (see (c)).

**(b) Why now / order.** Every mint site must exist before the token model changes under
them — `register`, `login`, `login_federated`, `create_guest` are all in place by Step 7, so
this step touches a closed set.

**(c) How.**

- **Rotation and reuse detection are one UPDATE plus one follow-up read, in one transaction:**
  ```sql
  UPDATE accounts.refresh_tokens
     SET used_at = now(), replaced_by = $2
   WHERE token = $1 AND used_at IS NULL AND expires_at > now()
  RETURNING player_id, family_id
  ```
  Zero rows means one of three things, distinguished by re-reading the row in the same
  transaction: absent (401), expired (401), or **already used** — credential replay.
- **The grace window is what `replaced_by` is for, and it is designed here, not left dead.**
  The common failure is not two concurrent dials but one dial whose *response* was lost — a
  mobile handoff, or gateway-svc's always-on 20 rps/burst 40 limiter, which
  `tools/splitproof/src/main.rs:2789-2812` already retries past. So: a replay where
  `used_at > now() - interval '30 seconds'` returns the recorded `replaced_by` successor plus
  a freshly minted access session, and **does not** kill the family. Outside that window, a
  replay is treated as theft.
- **The family kill needs `family_id` on `accounts.sessions`, or it is not a family kill.**
  Today `accounts.sessions` (`lib.rs:109-116`) has no link to the refresh family, so the only
  implementable revocation is `WHERE player_id = $1` — logging the player out on every
  device because one device replayed. Mint `family_id` in `issue_session_tx` (`lib.rs:171-185`)
  and scope the kill to `WHERE family_id = $1` on both tables.
- **Consumed refresh rows are evidence, not garbage.** `Store::prune_expired_sessions`
  (`store.rs:256-264`) must delete `WHERE expires_at <= now()` **only**. Pruning
  `used_at IS NOT NULL` rows would delete the detector itself, degrading every replay to a
  plain 401 with no error anywhere — the retention-sweep-swallowing-errors class verbatim.
- **A family has a hard 30-day life.** The successor inherits the family's original
  `expires_at`, not a fresh 30 days. A sliding family never expires, which makes the
  "30-day session" in the accounts contract meaningless; the hard cap keeps the documented
  bound honest. Retained-row cost: ~24 rows/device/day × 30 days ≈ 720 rows per active
  device (Known risk 5).
- The rotation, the successor's insert and the new access session's insert are **one
  transaction** — a crash between them must not leave a consumed token with no successor.
- `MAX_SESSION_TOKEN_BYTES` (`api/accounts/api/src/lib.rs:22`) covers refresh tokens too
  (same `store::new_token()`, 43 chars), checked at the three existing enforcement points
  (`lib.rs:79-81`, `verifier.rs:110-112`, `verifier.rs:70-72`).
- **The gateway needs no change** — it verifies access tokens through `dyn Sessions` exactly
  as before; refresh never crosses the verifier seam.
- **Two client surfaces regress silently if skipped.** `epic_oauth.rs:433` hands the browser
  only `Redirect::to("/#token=…")` — after this step it must carry the refresh token too, or
  the only browser login flow mints a refresh row it throws away and dies in 60 minutes.
  `demos/webui/src/index.html` stores the bearer in `localStorage` with no refresh path and
  must call `/accounts/refresh`. Neither is optional; both are one-file changes.

**(d) Dispatch:** `[opus]` — `subagent_type: "core-implementer"`, `model: "opus"`.
Gates: the Step-13 row of the gate table.

---

## Step 14 — tests for rotation and reuse `[test-author]`

**(a) What.** Happy rotation (old token dead, new works); **replay outside the grace window
kills the family and every access session of that family — and leaves other families of the
same player alive**; replay **inside** the grace window returns the successor and kills
nothing; expired refresh → 401; unknown refresh → the same 401; two concurrent refreshes →
exactly one success; a rolled-back rotation leaves the original usable; a family cannot
outlive its original 30-day expiry.

**(b) Why now / order.** Reuse detection is the only reason to prefer rotation over a long
opaque session, and the family scoping is the only thing separating "revoke a stolen token"
from "log the user out everywhere". Both need executed proofs.

**(c) How.** The two-families test is the one that would pass with the naive
`WHERE player_id = $1` kill and must not: mint two families for one player, replay in one,
assert the other still verifies. Concurrency asserts outcomes, not timing — two rotations,
exactly one `Ok`, never a sleep. Expiry is forced by writing `expires_at` in the past.

**(d) Dispatch:** `[test-author]`, `model: "sonnet"`.

---

## Step 15 — split-proof, fleet, and the at-risk topology `[opus]`

**(a) What.** New named assertions in `tools/splitproof/src/main.rs`. **`[A1]`–`[A5]` are
taken** (`:922`, `:935`, `:952`, `:970`, `:1047`, `:1058`), so the new series starts at
`[A6]`; each gets a monolith parity twin.

| id | assertion |
|---|---|
| `[A6]` | `login_federated("apple", …)` — a **known but unconfigured** provider → 503; `login_federated("nope", …)` → 400 |
| `[A7]` | `create_guest` → 201 + device secret; `login_federated("guest", secret)` → 200 |
| `[A8]` | guest links a real identity → `player.promoted` reaches `audit.log` |
| `[A9]` | refresh rotates; replaying the consumed token outside the grace window → 401 and the family is dead |
| `[WL8]` | guest registration credits **nothing**; after promotion, exactly one `starter:` ledger row |
| `[P8]` | the new auth ops are reachable on the player-QUIC plane with correct statuses |

Plus `tools/processctl/src/fleet.rs` (accounts block at `:513`, `:602`, `:609`, `:624-635`):
typed env for Google and guest, `overrideable_env` entries, `FleetFlavor::Proof` values.
Mirror into `weles/fleet.monolith.toml` / `weles/fleet.split.toml`.

**(b) Why now / order.** Everything is implemented; this is where "works in both topologies"
stops being a claim. `[WL8]`'s negative half is the executed proof of decision 3.

**(c) How.**

- **`[A6]`'s 503 arm depends on the proof fleet leaving a *known* provider unconfigured.**
  `fleet.rs:620-635` configures `EPIC_*` in `FleetFlavor::Proof` and Step 15 adds Google, so
  neither can serve as the unconfigured case — `"apple"` is in `KNOWN_PROVIDERS` (Step 1)
  with no verifier until #2b, which is exactly what this assertion needs. Do not configure
  Apple env in the fleet.
- **The harness has no identity-linking helper today** — the only production link path is the
  Epic browser callback, which splitproof does not drive. `[A8]` needs a new helper built on
  the Step 9 `link` op (bearer + `X-Api-Key: dev-key-client`), modelled on `register_capture`
  (`:2816-2858`).
- `[WL8]` must **not** perturb `[WL7]`'s counts — follow the convention at `:2138-2139`,
  which deliberately uses unregistered player ids so grants don't cross-contaminate.
- The `dev-key-client` policy must list every new method (fact 6) or all of these 403 before
  reaching a handler; the edits happen in Steps 5/7/9/13, and this step is where a missed one
  surfaces.
- The fleet-drift preflight fails loudly if the centralized fleet diverges from `cmd/*-svc`
  on disk — no new svc here, so it should stay green.

**(d) Dispatch:** `[opus]` — `subagent_type: "core-implementer"`, `model: "opus"`.
Verification: `cargo run -p verifyctl -- --all --strict`, **one rollout at a time**
(`/safe-verification` first; no second Cargo command while it runs).

---

## Step 16 — docs, tracker, and the false-comment sweep `[sonnet]`

**(a) What.**

- **Prose that becomes false when Steps 5/7/9/13 land** — correct all of it in this rollout,
  not "later" (Comments rule): `api/accounts/api/src/lib.rs:68-72`,
  `api/accounts/rpc/src/lib.rs:42,67`, `modules/accounts/src/ops.rs:1-10`,
  `modules/accounts/src/lib.rs:770-778`, `api/accounts/events/src/lib.rs:17-19` (enumerates
  `"dev"`/`"epic"` only), `cmd/accounts-svc/src/main.rs:9-12`,
  `demos/webui/src/lib.rs:4,23-24`.
- **`CLAUDE.md` and `AGENTS.md` both** (`docs-current` parses both — `tools/verifyctl/src/stages/docs_current.rs:6`
  `ROOT_DOCUMENTS`): the **accounts** paragraph (providers, session model, new ops), the
  **audit** paragraph (7 ledger topics → 8; "an 8th independent subscription" → 9th), and the
  **wallet** paragraph ("reacts to durable `player.registered`" → and `player.promoted`).
  `docs-current` validates paths, not counts, so none of these fail a gate — they simply
  become lying prose.
- `docs/roadmap/feature-tracker.md`: flip seq #2a, fill Module(s)/Landed, update the four
  Identity & accounts rows, add a change-log entry.
- **Record the deliberate deviations as known gaps**, with reasons: accounts still parses env
  inside the module rather than in `cmd/*`; `from_provider` is a constant in #2a. The
  Epic-specific-filename gap is **gone** — Step 3 renames `epic.rs` to `oidc.rs`; check that
  no doc still points at the old path.

**(b) Why now / order.** Last, because it describes what landed.

**(c) How.** `docs-current` (blocking) validates crate/path references in `CLAUDE.md`,
`AGENTS.md` and `docs/reference/*.md` — every path cited must exist at the committed sha.

**(d) Dispatch:** `[sonnet]` — mechanical prose edits against a landed diff.

---

## Gate table per contract-touching step

Four steps change the `#[http]` op surface — 5, 7, 9, 13 — and each pays the **whole**
cascade below. Steps 1, 3, 11 change no contract and pay none of it.

| artifact | authority | how to refresh |
|---|---|---|
| `opscatalog/src/generated.rs` | blocking `codegen-freshness` (`tools/verifyctl/src/stages/codegen.rs:57-80`) | `cargo run -p opscatalog-gen` |
| `clients/csharp/Generated/**` | same stage, byte-diffed against a fresh run | `cargo run -p csharp-client-gen` into `clients/csharp/Generated` |
| `tools/csharp-client-gen/testdata/*.golden.*` + `src/tests.rs:51-53` — **three hand-maintained counts**: `methods.len(), 14`, `dtos.len(), 8`, `statuses.len(), 8`, plus the literal name lists at `:74-84` | blocking `test` | `--emit-manifest testdata/manifest.golden.json`, `--out testdata`; **14 → 17 and 8 → 9 across the four steps** |
| `docs/reference/contract-golden/contracts.txt` | blocking `contract-golden` | `verifyctl --bless-contract-golden` |
| `tools/conformance/src/policy.rs` + `input-fields.golden.tsv` | blocking `conformance`, source-derived (an unpoliced field FAILs) | edit policy, then `verifyctl --bless-input-golden` |
| `docs/reference/public-api-baseline/accountsapi.txt` | `public-api` (advisory; blocking under `--strict`) | `verifyctl --bless-public-api` |
| `modules/apikeys/src/lib.rs:105` `DEV_CLIENT_POLICY` | no gate — a miss is a silent 403 | hand edit, proven by Step 15 |

**Known risk 3** applies to the three csharp counts: they are exactly the hand-maintained-list
class that went red on the 13th process during the wallet rollout. Prefer deriving them from
the manifest over restating them — that was the wallet retrospective's explicit lesson.

## Verification ladder

| after step | command | why |
|---|---|---|
| 1, 3, 11 | `cargo build`, `cargo clippy -- -D warnings`, `cargo test -p accounts` / `-p wallet` | no contract change; fast local proof |
| **5, 7, 9, 13** | `cargo run -p verifyctl -- --fast` | each is a full contract wave — codegen, goldens, conformance and the csharp counts all live outside `-p accounts` |
| 15 | `cargo run -p verifyctl -- --all --strict` | split-proof + public-api + topiccheck together |

**One rollout at a time** — `/safe-verification` before every Cargo-launched run; check
`pgrep -x cargo; pgrep -x rustc`; never dispatch two test-running subagents concurrently.

## Review

Every implementation step gets **one** adversarial pass as `core-reviewer`
(`subagent_type: "core-reviewer"`), class-keyed to
[core-failure-taxonomy.md](../reference/core-failure-taxonomy.md), at a model ≥ the
implementer's tier, by a method different from the implementer's.

Add **`proof-auditor`** to Steps **5, 7, 9, 13 and 15** — each edits a verify-stage surface
(`tools/conformance/src/policy.rs`, the blessed goldens; Step 9 also edits
`tools/topiccheck/src/main.rs`; Step 15 changes what split-proof covers). Steps 2, 4, 6, 8,
10, 12, 14 are ordinary unit tests, where `core-reviewer` already checks branch coverage.

## Known risks

1. **Steps 9→11 leave the new topic sinkless.** Advisory `topiccheck` flags it (blocking only
   under `--strict`/`--durability-strict`); the blocking reds in that window are the
   codegen/golden set. Land them back-to-back either way.
2. **Four contract waves, not one** (Steps 5, 7, 9, 13) — four baseline diffs to review
   honestly. The alternative, one giant contract step, would be a single unreviewable commit.
3. **Three hand-maintained csharp counts** move across those waves — see the gate table.
4. **Guest accounts are creatable without any credential.** `create_guest` is rate-limited
   only by gateway-svc's 20 rps/burst 40, and each call appends a durable `player.registered`
   to the **shared event log**, fanning out to audit's ledger subscription and wallet's. So
   the cost is unauthenticated durable-log growth, not two table rows. Decision 3 removes the
   *currency* incentive only. A per-IP guest-creation limit is a gateway-side concern and a
   separate change — named here so it is not mistaken for covered.
5. **Refresh adds a write path that did not exist.** Every client now POSTs `/accounts/refresh`
   roughly hourly (one UPDATE + two INSERTs), and consumed refresh rows are retained until
   expiry (~720 rows per active device over a 30-day family). This is the cost the "no session
   cache" non-goal does *not* address, and the reason that non-goal is justified by staleness
   semantics rather than by cost.
6. **A replay outside the 30-second grace window logs out one family.** With the grace window
   the lost-response case is handled; a genuinely delayed retry (>30s) still revokes. Recovery
   is provider-specific: guest re-authenticates with its device secret, google/epic with a
   fresh ID token. Both paths exist, which is why the strict outer behaviour is acceptable.

## Revision 2 changelog

Findings from the review pass that changed the plan's substance, not its wording:

- **`KNOWN_PROVIDERS` + `Resolution` added to Step 1** — the 400/503 taxonomy Step 5 promised
  was unrepresentable with a bare `HashMap`, and it is load-bearing for a pinned conformance
  case and for `[A6]`.
- **Step 1's "reversal" re-aimed at the real authority** — `OidcVerifier::new` validates
  nothing (its only `?` is the reqwest build), so the swallowed-error branch was dead code
  and Step 2's headline test had no reachable input. Validation moved into `from_vars`.
- **Step 3 decides issuer matching instead of deferring it to Step 4** — `starts_with`
  accepts `https://accounts.google.com.evil.test`; that is now an `IssuerMatch::Exact`
  decision, not a discovery.
- **Step 9 gains a player-level advisory lock and a fixed acquisition order** — the identity
  lock does not serialize two links of different providers to one player, so the "exactly one
  promotion event" invariant was unenforced.
- **Step 9 gains `Service::link_identity`** — handing the tx to two callers would have
  duplicated the emit condition, contradicting the "one authority" claim.
- **Step 13 gains `family_id` on `accounts.sessions`, a grace window, and a retention rule** —
  the family kill was unscopeable, `replaced_by` was a dead column, and pruning consumed rows
  would have deleted the reuse detector.
- **Step 13 gains the two client surfaces** (`epic_oauth.rs:433`, `demos/webui`) that a 30d→60min
  access TTL silently breaks; the previously-claimed splitproof bearer-reuse risk does not
  exist (`main.rs:286`, `:470` mint their own).
- **Gate table replaces the per-step guesses** — four contract waves, not two; three csharp
  counts, not one; `--fast` after each wave, not only after Step 5.
- **`proof-auditor` scope widened** to every step editing a verify surface.
- **Step 15's assertion ids renumbered** — `[A4]`/`[A5]` are already taken by the garbage-bearer
  and dev-token assertions.
