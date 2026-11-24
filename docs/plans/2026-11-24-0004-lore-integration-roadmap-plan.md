# Lore integration: roadmap

Status: waiting for approval

## Context

**Requested outcome (owner, 2026-11-23):** "research do planu co bedzie potrzbne do
zrobienia i w jakiej kolejnosci". This is the work needed for Swarog's first real
extension, Lore support, and its order. Swarog manages users, issues tokens and
provides what Lore expects from an external service. Lore Portal lives in this repo
as a separate zero-sharing project in the style of weles, and covers browsing,
review, comments and merge requests.

**Decided, closed (owner's words in the decision records):**
- Tier 2: per-repo `urc-{id}` tokens via token exchange (2026-11-23).
- Review, comment and merge-request state in Swarog tables ("tabele oczywiscie").
- Merge from the Lore client; the portal is review-only and detects merges from the
  push. The merge worker and the upstream PR are deferred (`docs/todo/lore.md`).
- Binary asset previews come in the 2nd or 3rd portal iteration.
- "weles narazie zostaw w spokoju. jak kto sobie zainstaluje lore to jego problem":
  no Weles or installer work. Swarog only knows the loreserver address.
- "samego serwera lore nigdy nie bedziemy modyfikowac": stock `loreserver` v0.14.0.
- RBAC is a separate module and service, `authz` (2026-11-24): "ok to rbac
  bedzie osobnym modules/service". Modules own their action catalogs (code);
  `authz` owns roles (named action sets, operator data) and grants (global or
  per resource), and answers "which actions has user U on resource R of kind K".
  Modules never see role names.

**Research leads, not decisions.** October P1–P5
(`docs/research/2026-10-22-1539-lore-hosting-exploration-summary.md`) is research.
Each phase plan re-checks a lead before relying on it. The owner corrected
2026-11-24: "to nie byla decyzja tylko resreach". The leads are:
- wipe (already a repo-wide rule, AGENTS.md);
- `Subscribe` bridged to `lore.*`;
- one studio per deployment with no tenant module;
- grants in the `lore` module (superseded by `authz` above);
- a provider-independent device approval page plus production password login
  and admin-created accounts.

**Explicit exclusions:**
- Weles/installer integration.
- Modifying loreserver.
- A merge button.
- Asset previews.
- Tenants.
- The `player_id → user_id` rename. October §7 records it as independent of Lore
  ("Lore potrzebuje tylko stabilnego `sub`"), so it stays a separate plan, outside
  this roadmap.

**Shape of this document.** The owner asked what is needed and in what order, so
this is a roadmap. Each step below is one plan-sized phase. It names what it
touches, why it comes at that point, the non-mechanical decisions, and the default
lane. As in October (§8, "Każdy krok to osobny plan"), each phase gets its own
detailed plan with research, review and approval before anything is dispatched.
This document fixes the order, the seams and the decisions every phase plan
inherits.

Sources:
- the three researcher reports from 2026-11-24 (API surface, usages, patterns);
- `docs/research/2026-11-23-2308-lore-v0-14-integration-summary.md`;
- `docs/research/2026-11-23-2325-lore-server-side-merge-summary.md`;
- the Lore v0.14.0 source (tag 317f48b9).

## What exists today (verified)

- **Tokens.** `accounts` issues only opaque tokens, 32 random bytes stored in
  Postgres:
  - access TTL 60 min, rotating refresh families with a 30-day cap and 30 s grace
    (`modules/accounts/src/store.rs:12-23`, `:191`);
  - one verifier, the gateway's `verify_bearer` → `accountsapi::Sessions`
    (`modules/gateway/src/lib.rs:997`, `api/accounts/api/src/lib.rs:165`), with a
    128-byte token cap (`:25`).
- **No issuer pieces.** There is no JWT signing, signing key, JWKS,
  `.well-known`, OAuth client, device grant, client_credentials, revocation
  endpoint or token exchange. `oidc.rs` is verify-only, the inbound
  Google/Epic id_token path (`modules/accounts/src/oidc.rs:1-25`).
- **No hosted login.** Swarog has no human login page. Password login is dev-only
  behind `ACCOUNTS_DEV_AUTH`; production has `epic`, `google` and `guest`
  (`modules/accounts/src/providers.rs:40`). Epic web OAuth is the only module-owned
  browser flow: `ctx.mount` (`accounts/src/lib.rs:1371`), state in
  `accounts.oauth_states`, and in the split a gateway-svc passthrough prefix
  (`cmd/gateway-svc/src/addrs.rs:229-243`, `modules/gateway/src/proxy.rs`).
- **No gRPC.** The workspace has no tonic or prost. The internal edge is QUIC + mTLS
  JSON (`core/edge/src/lib.rs:1-20`). The HTTP front runs on axum-server 0.8, which
  serves through hyper-util's `auto::Builder`
  (`axum-server-0.8.0/src/server.rs:11`). That builder speaks HTTP/2 prior
  knowledge on a plain listener, so a tonic service mounted on a module's routes is
  reachable over h2c without a new listener. loreserver dials `user_url` over
  `http://` or `https://` (`lore-server/src/authnz/auth.rs:37-49`).
- **lore-proto cannot be linked cleanly.** It always depends on `lore-base`,
  whose `build.rs` compiles rpmalloc C with `cc`, and tonic's `router` feature
  pulls axum 0.8 beside Swarog's axum 0.7. The committed generated files
  (`lore-proto/src/grpc/*.rs`) need protoc only to regenerate.
  `lore.user.v1.rs` is self-contained. thin_client, storage, revision,
  repository and notification have `crate::` paths (16, 27, 7, 4, 3 hits) to
  sibling generated modules.
- **Admin seam.** `controlplane::Provider` on `PROVIDER_SLOT`, with
  `Manifest`/`ContributionDecl`/`Point` (`core/controlplane/src/lib.rs:96-744`).
  admin-svc reaches peers through the `peer_face` hand list
  (`cmd/admin-svc/src/lib.rs:13-40`). The old ExtensionPoint seam was razed in
  b8435c9a. The nearest page precedent is apikeys
  (`modules/apikeys/src/admin.rs:592-955`). Admin mockups exist in
  `UILayout/redesign/lore-*.html` (repos, repo, permissions, locks, events, server,
  billing).
- **Events.** Nothing turns an external stream into bus events today. The nearest
  shape is the scheduler's `fire_locked`: an advisory lock, then a state update
  and `emit_tx` in one tx (`modules/scheduler/src/lib.rs:465,490-525`). Workers
  use spawn + catch_unwind + `Liveness` (`scheduler/src/lib.rs:703-744`,
  `mail/src/worker.rs:458-485`). topiccheck's `ALLOW_UNSUBSCRIBED` is empty
  (`tools/topiccheck/src/main.rs:80`), so every new topic needs a durable
  subscriber. audit's `DURABLE_TOPICS` is a required-edit list
  (`modules/audit/src/lib.rs:47`).
- **Notifications.** There is no "send to user" contract. notifications
  subscribes to producers' topics and inserts rows in `deliver_on`
  (`modules/notifications/src/lib.rs:4`).
- **Account deletion.** `player.deleted` has seven tombstone-and-sweep
  subscribers (characters precedent: `modules/characters/src/lib.rs:229-245`).
  There is no suspension event (`modules/accounts/src/admin.rs:58`).
- **weles.** It is a workspace member sharing the root `Cargo.lock`
  (`Cargo.toml:164-171`). Zero-sharing is a convention: archcheck classifies it
  `Kind::Other` with no dependency rule (`tools/archcheck/src/main.rs:268-319`).
  The house frontend precedent is embedded static files with no JS build
  (`core/admin/src/`, `demos/webui/src/index.html`).

## Overlap: why not extend X

| Need | Candidate | What it does / how it differs | Decision |
|---|---|---|---|
| OIDC issuer | `accounts/src/oidc.rs` | Verifies external IdPs' id_tokens, the opposite role | Not extended. A new `issuer` area inside `accounts` may share JWK types only |
| OIDC issuer | a new `oidc` module | Would need the users, sessions and refresh families that `accounts` owns, over the wire, for every grant | Rejected. The issuer lives in `accounts`, which owns the identities and refresh families (reuse-detection and family kill already exist, `accounts/src/lib.rs:961-1019`) |
| Swarog-API tokens for the portal | the existing opaque sessions | Already verified by the gateway | **Kept.** The token endpoint returns the opaque session for audience = Swarog, so the gateway stays unchanged. Only tokens whose audience is the Lore host are ES256 JWTs (loreserver verifies JWTs, `jwt.rs:315-389`) |
| Auth-code / device state | `accounts.oauth_states` (Epic) | Client-side Epic state, replica-safe via `DELETE … RETURNING` | Pattern copied into new issuer tables; the Epic table is not reused |
| client_credentials secrets | `apikeys` | Keys for kinds of client, with a gateway method allow-list | Not used. Lore service accounts need `is_service_account` and Lore actions. Clients live in an `accounts` table; only the digest-and-one-time-reveal pattern is copied |
| Per-repo grants | apikeys roles; `controlplane::Role` | Gateway method allow-lists; operator ordering for the admin panel | Not used. Grants live in the new `authz` module (owner decision) |
| Grants inside token exchange | grants in `lore`, served to `accounts` | `lore` would have to serve a trait from `accountsapi`, which archcheck forbids: `modules/X` may not depend on a foreign rpc crate (`tools/archcheck/src/main.rs:6`) | Rejected. `accounts`, `lore` and `review` consume `authzapi`, the ordinary consumer direction (like `friends` → `accountsapi::Directory`) |
| RBAC in `core/` | a core contract like `controlplane::Provider` | Makes it mandatory in every process; `core/` holds mechanisms, not operator-configured data | Not now. Only if `core/admin` itself consumes RBAC (operators); then the contract moves to `core/` and `authz` provides it |
| Review state | `lore` module tables | Review is a separate domain (MRs, comments, reviews) and a separate deploy unit | A `review` module. It reads lore facts only from `lore.*` events and `loreapi`, never by import |
| Subscribe bridge | scheduler loop | Ticks and emits; holds no stream | Copies the supervision and advisory-lock pattern; no dependency |
| gRPC for `user_url` | edge transport | QUIC JSON, not gRPC | A tonic service mounted with `ctx.mount` on lore's HTTP routes (h2c, verified above) |
| Lore wire types | linking `lore-proto` | C build (rpmalloc) + axum 0.8 | Vendored generated code (Step 6) |
| Portal UI | `core/admin` templates | Swarog-internal, operator-facing | The portal is zero-sharing and copies the embedded-static precedent, not the code |

## Order

Fifteen phases in four groups, A–D. The identity work (A) gates everything, because
loreserver accepts nothing without a token. The `lore` module (B) gates review
(C), because review subscribes to `lore.*`. The portal (D) comes last, because it
consumes the public surface of A, B and C. Each phase keeps both topologies working
and proves the split.

**One prefix for all browser and OIDC routes.** Every route below that a browser or
an OIDC client reaches lives under `/accounts/oidc/…` (login page, discovery,
JWKS, authorize, token, revoke, device, approval). No op uses that prefix:
`POST /accounts/login` is an existing op (`api/accounts/api/src/lib.rs:188`),
and a `ctx.mount` route on the same path would turn its POST into 405 in the
monolith only. In the split, one gateway-svc passthrough entry for the prefix goes
in Step 1 (`cmd/gateway-svc/src/addrs.rs:239-245`). From Step 2 on, every phase's
split proof checks its endpoints through gateway-svc.

### A. Identity and OIDC issuer (accounts)

**Step 1: Studio users can sign in to Swarog in a browser.**
- (a) `modules/accounts`:
  - a production email-and-password identity. The existing argon2 `dev`
    identity and its register and login ops (`accounts/src/lib.rs:4-6,168,804`)
    are promoted and renamed in this step, not kept beside the new one. The
    `ACCOUNTS_DEV_AUTH` gate goes in the same step, apart from the gateway
    dev-verifier part, which gets its own flag;
  - operator-created accounts through the accounts admin `Provider` (a
    `PageHead` form; precedent apikeys create + `SecretReveal` for the initial
    password);
  - a hosted login page with the configured federated providers next to it.
  - Routes under `/accounts/oidc/` via `ctx.mount`, plus the single
    gateway-svc passthrough entry for that prefix.
- (b) Device approval (P5) and portal login (auth code) both need a human session
  in the browser. Nothing provides one today.
- (c) A browser session cookie distinct from the player bearer, stored
  server-side in `accounts`. Self-registration with email verification uses
  `mail.send_requested`. It is P5's second item and can split out as Step 1b.
- (d) `[opus]`.

**Step 2: Signing keys, discovery, JWKS.**
- (a) `accounts` schema `signing_keys` (kid, ES256 private key, status
  active/retiring/retired, created_at). Routes:
  `/accounts/oidc/.well-known/openid-configuration` and
  `/accounts/oidc/jwks` (same prefix as Step 1), under the issuer `https://<host>/accounts/oidc`.
- (b) Every later grant signs with these keys.
- (c) Keys live in the shared Postgres, so every accounts-svc replica signs and
  serves the same JWKS. Rotation keeps a retiring key in the JWKS until its last
  token expires. Schema changes are wipe (P1); a wipe invalidates outstanding
  tokens, which is acceptable.
- (d) `[opus]`.

**Step 3: Clients, authorization code + PKCE, refresh, revocation.**
- (a) `accounts` tables `oidc_clients` (public portal client, the Lore CLI
  client id, confidential service clients) and `oidc_codes`; endpoints
  `authorize`, `token` (`authorization_code`, `refresh_token`) and `revoke`.
- (b) The portal (Step 12) and the Lore CLI refresh need them.
- (c) The audience decides the format:
  - Swarog audience → the existing opaque session plus refresh family;
  - Lore audience → an ES256 `at+jwt` with `sub` = user id,
    `preferred_username` = handle. The audience and `root_domains` belong to
    the client registration in `accounts` (`oidc_clients.audience`). The Lore
    CLI's device request sends only `client_id` and `scope`
    (`lore-transport/src/auth/oidc/device.rs`), so the client decides the
    audience. `accounts` never reads `lore` config;
  - `id_token` is always a JWT.

  Refresh reuses the family store.
- (d) `[opus]`.

**Step 4: Device grant and client_credentials.**
- (a) `device_authorization` and the approval page (mockup screen 2), plus
  `client_credentials` for confidential clients (`is_service_account`).
- (b) `lore login` uses the device flow. The lore module's own calls to
  loreserver (bridge, locks, branch protect) need a service token.
- (c) A service token's `resources` claim is the whole Tier 2 story for
  services: loreserver denies a token with no matching entry
  (`resource_grants_authorizer.rs:20-21`), and `is_service_account` is only a
  forwarding marker. A confidential client registration therefore carries its
  resource grants (`urc-*` pattern plus actions). An operator sets them in the
  accounts admin, and `accounts` puts them in the token. `modules/lore` gets its
  client id and secret through typed config.
- (d) `[opus]`.

### B. RBAC and the `lore` module

**Step 5a: `authz` module (RBAC).**
- (a) Built per `add-game-module`: `api/authz/{api,rpc,events}`,
  `modules/authz` and `cmd/authz-svc`, plus every registration point.
  - Schema `authz`:
    - `catalog` (kind → actions), declared by each consuming module at boot
      through `authzapi` as an idempotent upsert;
    - `roles` (kind, name, actions[]), operator data;
    - `global_roles` (kind, user, role);
    - `grants` (kind, resource, user, role);
    - purge tombstones.
  - Wire API:
    - `actions(user, kind, resource)`;
    - `grant`/`revoke`;
    - `drop_resource(kind, resource)`;
    - `declare_kind(kind, actions)`.
  - Admin `Provider`: role matrix per kind, global assignment, per-resource
    overrides, effective preview (from `UILayout/redesign/lore-permissions.html`,
    made kind-generic).
  - `player.deleted` subscription: Genesis tombstone + `player-purge` sweep.
- (b) `lore`, the token exchange and `review` all ask it. It holds no knowledge
  of Lore.
- (c) The result is the global role's actions plus the resource role's actions.
  A role may only hold actions from its kind's catalog. Kept minimal: no groups,
  conditions or inheritance until a real need appears. Changing a role re-scopes
  every holder at once. Already issued Lore tokens keep their actions until they
  expire, so per-repo tokens are short-lived (5–15 min, fixed in the 5c plan).
- (d) `[opus]`.

**Step 5b: `lore` skeleton and repositories.**
- (a) Per `add-game-module` (the ~12 touch points the patterns report lists):
  - `api/lore/{api,rpc,events}`, `modules/lore`, `cmd/lore-svc`;
  - registration in `cmd/server`, `checkmodules`, the processctl fleet,
    conformance, admin-svc `peer_face` and splitproof.
  - Schema `lore`: `repositories` (id, name, description, default branch).
    It holds no permission tables.
  - Typed config: loreserver gRPC address, service client id and secret.
  - Declares kind `lore` in `authz` at boot.
  - A repository's creator gets an `owner` grant through `authzapi`. Creating a
    repository requires the `create-repo` action. Removing a repository calls
    `drop_resource`.
- (b) Step 5c's exchange and every later lore phase need the repository inventory.
- (c) Action catalog: `read, write, owner, admin, migrate, push-protected,
  obliterate, presign, create-repo`.
- (d) `[opus]`.

**Step 5c: Token exchange (Tier 2).**
- (a) RFC 8693 exchange with RFC 8707 `resource` in `accounts`. The exchange
  maps the resource (`urc-42`) to kind `lore` and resource `42` and asks
  `authzapi::actions`.
- (b) It needs `authz` (5a) and repositories (5b). Splitting it from Step 4
  lets the split proof run accounts-svc against a real authz-svc.
- (c) Error answers:
  - `authz` absent: `invalid_target`, so accounts boots without it (optional
    lookup `registry::try_require`, `core/registry/src/lib.rs:103`);
  - `authz` unreachable: `temporarily_unavailable`. Never a token without
    grants;
  - empty action list: `access_denied`.

  The minted token carries `resources` per loreserver's
  `resource_claim`/`resource_template`.
- (d) `[opus]`. The split proof covers accounts-svc → authz-svc.

**Step 6: `lore.user.v1` server for loreserver's `user_url`.**
- (a) A vendored crate `vendor/lore-wire` (outside `api/`, `core/` and
  `modules/`) holding Lore's committed generated code, laid out in the same
  module tree, so `crate::` and `super::super::` paths resolve. Step 6 adds
  `lore.user.v1`. Each later step adds the packages it calls:
  - Step 8: notification, revision `BranchList`, `urc.lock`;
  - Step 9: `BranchProtect`, force unlock;
  - Step 10: `RevisionList`;
  - Step 12: thin_client and storage. It depends on tonic and
  prost without tonic's `router` feature.
  - Service impl in `modules/lore`: `UserGet`/`UserFind` via
    `accountsapi::Directory`; `RepositoryList`/`Register`/`Unregister` over
    `lore.repositories`.
  - Mounted with `ctx.mount`.
- (b) loreserver resolves users and the repository inventory here before any push
  works.
- (c) **Authentication.** Every `lore.user.v1` RPC carries the caller's bearer
  token (`user.proto:5-7`). lore checks it against the Swarog JWKS through a
  new wire contract in `accountsapi` (lore → accounts, the allowed direction):
  `verify_lore_token`. `RepositoryRegister` needs the `create-repo` action
  and makes the caller the owner. `RepositoryUnregister` answers
  `PERMISSION_DENIED` itself unless the caller has `owner`.
  **`UserFind`** by display name: the phase plan decides between an empty
  answer and a new `Directory` method (`api/accounts/api/src/lib.rs:144,151`
  has only id and handle lookups).
  **Timeouts:** the shared router's `TimeoutLayer` answers 408
  (`core/app/src/lib.rs:1216`). The gRPC routes must instead return a gRPC
  status and must not cut off the streaming `RepositoryList`; the phase plan
  decides how.
  **Split:** loreserver's `user_url` points at lore-svc's HTTP port
  directly, not at the gateway: gateway passthrough copies headers and body only,
  so gRPC trailers would be lost (`proxy.rs:173`). Invariants:
  - no second axum version in `Cargo.lock`;
  - no `cc`/`*-sys` from Lore crates;
  - `aws-lc-rs` never enters the tree.

  Licence and notice for the vendored code follow Lore's `about.toml`/`notices`.
- (d) `[opus]`.

**Step 7: Live milestone (owner check).**
- (a) No code. A manually installed loreserver v0.14.0 configured with:
  - `[server.auth.oidc]` issuer and `client_id`;
  - `jwt_audience`;
  - `user_url`;
  - `resource_claim` and `resource_template` (without the template the
    client never exchanges);
  - `repository_catalog`/`repository_catalog_url`. Run
  `lore login` → `lore clone` → `lore push` → `lore repo list`, with a denied
  push for a read-only grant, in the monolith and then the split.
- (b) It proves A + 5a–5c + 6 against the real server before building on them.
- (c) Swarog side: a short setup note in `docs/reference/` (`lore.md`).
- (d) `[docs]` plus an owner check.

**Step 8: Event bridge `Subscribe` → durable `lore.*`.**
- (a) `api/lore/events` topics: `lore.branch_pushed`,
  `lore.branch_created`/`deleted`, `lore.lock_changed`,
  `lore.repository_changed` (final names in the phase plan). One bridge in
  `modules/lore` runs a task per repo and authenticates with a service token
  from Step 4. Each topic gets its audit sink (`DURABLE_TOPICS`) in the same
  step, so `ALLOW_UNSUBSCRIBED` stays empty.
- (b) review (Step 10) and notifications consume these.
- (c) `Subscribe` is lossy (broadcast capacity 200), so events are hints. On
  every (re)connect the worker reconciles from `BranchList` and lock `Query`
  against a `lore.branch_heads` table and emits the differences, so a gap never
  loses a push. A single leader lock (one advisory lock or a lease row, not one per
  repo, which would pin a pool connection per repo) keeps one bridge across
  replicas. The emit dedup key is `(repo, branch, previous head, new head)`, so
  a legitimate A→B→A move is not swallowed. Supervision copies
  scheduler/mail.
- (d) `[opus]`.

**Step 9: Admin pages for lore.**
- (a) `modules/lore/src/admin.rs` `Provider`, ported from `UILayout/redesign/lore-*.html`:
  - repos and repo tabs (branches + protect, locks, access, events);
  - the repo Access tab reads effective actions from `authz` and links to the
    `authz` permissions page (Step 5a); lore itself has no permissions page;
  - locks with force unlock;
  - events;
  - server (HTTP health probe only, no process control: Weles is excluded);
  - billing (seats = users with a Lore grant, store size if loreserver reports it,
    hourly `lore.billing_snapshot` schedule).
- (b) Operators assign grants before studio members can work. It can run in
  parallel with Step 8 after Step 5b.
- (c) Writes such as force unlock and BranchProtect call loreserver with the
  service token. Mockup items that need process control (restart) are dropped and
  listed for the owner.
- (d) Through the `mockup-implementation` skill, one page per step: backend
  `[opus]`, view `mockup-implementer` `[sonnet]`.

### C. Review (Swarog tables)

**Step 10: `review` module.**
- (a) `api/review/{api,rpc,events}`, `modules/review`, `cmd/review-svc`.
  - Schema `review`: merge_requests (repo, source, target, base head, state,
    draft, labels), reviewers + review state, comments (MR, file, line, revision,
    thread, resolved), viewed files.
  - `#[http]` player ops for list, create, comment, review, close and reopen.
  - Durable `review.*` topics, each with its audit sink in this step.
  - `player.deleted` subscription: Genesis tombstone + `player-purge` sweep,
    the same as `lore`.
- (b) The portal's review screens need it.
- (c) Merged detection: a `lore.branch_pushed` subscription marks an open MR
  merged when the target's new head contains the source head (ancestry checked
  over loreserver `RevisionList` with a service token). Authorization: the caller
  needs `read` on the repo, resolved over `authzapi` (kind `lore`), never by import. Approvals are advisory ("2 approvals expected", lore repo
  setting).
- (d) `[opus]`.

**Step 11: Review notifications and audit.**
- (a) Edits inside `modules/notifications`: subscriptions to `review.*`
  (review requested, mentioned, comment, merged) and `lore.lock_changed`. audit
  topic list.
- (b) Mockup screen 16.
- (c) Payloads denormalize handles, as `friend.*` does, because the delivery tx
  has no stubs. `AfterRegistration` subscriptions.
- (d) `[opus]`.

### D. Lore Portal

**Step 12: Portal skeleton (zero-sharing).**
- (a) `portal/` workspace member (BFF on axum 0.7 with embedded static pages,
  the weles precedent). It depends on `vendor/lore-wire` and nothing else
  from the repo (owner question 4). A new archcheck kind `Portal` with a deny-all rule for
  `core/`, `modules/`, `api/`, `cmd/` and `weles/` makes zero-sharing
  enforced, not a convention.
  - Login with auth code + PKCE against Swarog.
  - Repositories list, Files with folder view, file view (text; binaries as
    metadata), History, Revision, Branches.
  - The design-system primitives from the v2 delta.
- (b) It consumes everything above.
- (c) The BFF holds the user's Swarog token and exchanges it per repo (Step 4) for
  loreserver calls (ThinClient `RevisionTree`/`RevisionInfo`/`RevisionDiff`,
  `StorageService.Get`, `BranchList`, `RevisionList`, `LockService.Query`).
  Text diff is computed in the BFF (`ContentDiff` is unimplemented in Lore).
  Per-line attribution is out of the first cut, because there is no server audit.
- (d) Several steps in the phase plan: `[opus]` BFF, `mockup-implementer` views.

**Step 13: Portal review screens.**
- (a) Compare, merge-request list and repo merge requests, new MR, MR detail
  (conversation, files changed with inline threads, revisions, MergeBox with
  commands), repo locks and my locks, notifications, profile (devices = refresh
  families, PATs = confidential clients).
- (b) It needs Steps 10–12.
- (c) The BFF calls Swarog `review` ops through the gateway with the user's
  session plus an `X-Api-Key` (`modules/gateway/src/lib.rs:21-25`). Step 12
  creates an apikeys role for the portal, covering the review and lore methods,
  and the key comes from the portal's config.
- (d) `[opus]` BFF + `mockup-implementer` views.

**Step 14: Docs, tracker, verify gates.**
- (a) `docs/reference/lore.md` and the feature-tracker row,
  `.agents/shared/gamebackend.md` (portal as a project category), splitproof
  assertions for lore and review.
- (b) It closes the roadmap.
- (d) `[docs]` + `[test-author]`.

## Owner questions before Phase A starts

Resolved 2026-11-24: grants live in a separate `authz` module (Step 5a).

1. ~~**Grants seam for token exchange (Step 5b, structural).** `accounts` signs the
   per-repo token, but the grants belong to `lore`. `lore` cannot serve a trait
   from `accountsapi` in the split, because archcheck rule 2 forbids a module
   depending on another module's rpc crate. Options:
   - **(A, recommended)** A core contract `core/resourcegrants` with
     core-served rpc glue, the same shape as `controlplane::Provider`
     (`core/app` serves `controlplanerpc`). Any module can be a grants source
     (lore today, others later), and `accounts` looks it up with
     `try_require`. It follows Open/Closed and an existing precedent.
   - **(B)** A projection: `lore` publishes grant changes onto a topic, and
     `accounts` keeps a `resource_grants` table. The exchange is local, but
     `accounts` holds a copy of lore's data, with a lag window.
   - **(C)** An explicit archcheck exception for this one dependency. Not
     recommended: it is the first such exception.~~ Resolved: `authz` module.
2. **Resolved 2026-11-24: yes.** **Login for studio members (Step 1).** P5 lists email-and-password
   self-registration and operator-created accounts. Proposal: build
   operator-created accounts first and self-registration as Step 1b. Federated
   Google/Epic stays available on the same page. OK?
3. **Resolved 2026-11-24: vendor.** **Vendoring Lore's generated code (Step 6) vs linking `lore-proto`.** Linking
   brings a C build (rpmalloc) and a second axum. Proposal: vendor.
4. **The portal shares the vendored crate with Swarog.** It is third-party code,
   not Swarog code. Proposal: allow this one shared dependency in the portal's
   archcheck rule.

## Not in this roadmap

- `player_id → user_id` (October §7: independent of Lore; a separate plan).
- The merge worker, the upstream Lore merge and asset previews (`docs/todo/lore.md` Deferred).
- Weles/installer and loreserver process control.
- Operator roles (`controlplane::Role`) moving onto `authz`. When that happens, the
  `authz` contract moves to `core/` so `core/admin` can consume it.
- Account suspension (no `player.suspended` exists; lore would react to it when it lands).
