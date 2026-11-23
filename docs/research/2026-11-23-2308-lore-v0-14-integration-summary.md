# Lore v0.14.0 integration surface: summary

Source: `EpicGames/lore` at tag v0.14.0 (317f48b9), read-only checkout. Lore is
never modified; Swarog and the portal integrate against the stock `loreserver`.
This supersedes §2 and §4–5 of
`docs/research/2026-10-22-1539-lore-hosting-exploration-summary.md` (kept as an
archive). Anchors are paths in the Lore repo. "Confirmed" anchors were re-read by
the researcher; "subagent" anchors were traced by a helper and not re-read. Grep
results are lower bounds.

## Answers

- **No `UrcAuthApi` adapter is needed.** With `[server.auth]` + `[server.auth.oidc]`
  and no `auth_url`, login, refresh, revocation, client credentials and token
  exchange go to a standard OIDC issuer; the user directory, repository registry
  and listing go to `lore.user.v1` at `user_url`. (confirmed)
- **Tier is a server config choice.** No `auth_url`, no `resource_claim` → Tier 1
  (every holder gets `read`); `resource_claim` set → Tier 2. The client exchanges
  tokens only when the server advertises `resource_template`/`scope_template`.
- **Browsing works without a working copy, over several RPCs.** No server-side
  text diff (`ContentDiff` is unimplemented) and no server-side per-line audit.
- **No merge-request primitive and no general server merge.** `BranchPush` can
  fast-forward-merge a pushed revision onto the same branch; `BranchDiff` returns
  conflicts and merge base. Merging A into B needs a client working copy or a
  backend that links `lore-revision` against the stores.
- **Events only via `NotificationService.Subscribe`**: at-most-once, in-memory,
  lossy. No webhooks in the stock binary.
- **Review/comments/MRs are not specified by Lore.** Its roadmap lists a web
  client and code review tools for 2027; external review tools are expected to
  attach application metadata to revisions.

## 1. OIDC contract

Authorizer selection — `lore-server/src/authnz/repository_authorizer.rs:571-602`:

| Config | Authorizer |
|---|---|
| no `[server.auth]`, no `auth_url` | AllowAll |
| `auth_url` | AuthClient (gRPC online check) |
| `resource_claim` | ResourceGrants (Tier 2) |
| neither | GlobalGrants (Tier 1) |

`auth_url` without `[server.auth]` and `auth_url` + `resource_claim` are startup
errors; `implicit_actions` is Tier 1 only. Validation:
`lore-server/src/settings.rs:193-258` (non-empty `jwt_issuer`/`jwt_audience`),
`:260-345` (OIDC issuer = first `jwt_issuer`; templates exclusive and contain
`{id}`; `token_exchange_issuer` requires a template).

Config keys — `settings.rs:394-455` (`AuthSettings`): `jwt_audience`, `jwt_issuer`
(at most two, for cutover), `jwt_typ` (optional, `at+jwt`), `permission_claim`,
`implicit_actions` (default `["write"]`, `:504`), `resource_claim`,
`resource_id_claim` (default `resource_id`), `resource_id_template` (default
`urc-{id}`), `resource_wildcard` (default `urc-*`), `baseline_access`,
`repository_catalog`, `repository_catalog_url`, `oidc`.
`settings.rs:461-484` (`OidcSettings`): `client_id`, `scopes`,
`client_credentials_scopes`, `preferred`, `resource_template`, `scope_template`,
`token_exchange_issuer`.

Server advertisement — `lore-server/src/auth/oidc.rs:11-40`: issuer from
discovery; `preferred` forced true without `auth_url`; `token_exchange_issuer`
only on Tier 2. `EnvironmentGet` is unauthenticated
(`lore-proto/proto/lore/environment/v1/environment.proto:9,60-86`).

Discovery the client reads — `lore-transport/src/auth/oidc/discovery.rs:30-38`:
required `issuer`, `token_endpoint`, `jwks_uri`; optional
`device_authorization_endpoint`, `revocation_endpoint`, `end_session_endpoint`,
`token_endpoint_auth_methods_supported`. Issuer must match (`:292`); endpoints
https or loopback http (`:301-322`).

Grants the client uses (all in `lore-transport/src/auth/oidc/`):

| Grant | Anchor | Notes |
|---|---|---|
| Device (RFC 8628) | `device.rs:21,42-47,54,118-140` | scopes + `offline_access`; standard pending/slow_down/denied/expired; 5 s poll; prefers `verification_uri_complete` |
| Refresh | `refresh.rs:18-34` | no `scope` |
| Client credentials | `client_credentials.rs:18-77` | `client_secret_basic` unless only `post` listed |
| Token exchange, partition (RFC 8693 + 8707) | `exchange.rs:26-28,52-145` | access_token → access_token; one `resource` or one `scope` |
| Token exchange, multi-resource | `mod.rs:394-406` | one `resource` per id |
| Token exchange as login | `exchange.rs:153-186` | caller-supplied `subject_token_type` |

Token response — `oidc/token.rs:23-36`; display identity from `id_token`
(`:192`). Tier 1 sends the login token unexchanged (`exchange.rs:85-95`).

**Send check** — `lore-credential/src/jwt.rs:100-137,248-266`: the client sends a
token only to hosts named by `root_domains` / `https://lore.org/claims/root_domains`,
else derived from `aud` (https URL → host; domain → suffix).

Server verification — `lore-server/src/auth/jwt.rs:144-173`: `sub`, `iss`, `iat`,
`exp`, `aud`, optional `env`, `name`, `preferred_username`, `client_id`,
`resources`, `groups`, `is_service_account`, `idp`. Identity: `identity` →
`https://lore.org/claims/identity` → `sub` (`:53-56,560-581`). Key by `kid`
(`:339-394`).

Authorizer claims: Tier 1 `global_grants_authorizer.rs:33-64` (string array at
`permission_claim`); Tier 2 `resource_grants_authorizer.rs:14-82` (array of
`{resource_id: "urc-<id>", permission: [...]}`). Actions
(`repository_authorizer.rs:28-38`): `read`, `write`, `owner`, `admin`, `migrate`,
`push-protected`, `obliterate`, `presign`.

Logout: `oidc/revocation.rs:20-40` revokes the refresh token; `end_session_endpoint`
is never called.

## 2. `user_url` (`lore.user.v1`)

`lore-proto/proto/lore/user/v1/user.proto`: `UserService.UserGet`, `UserFind`
(`:9-14`); `RepositoryRegistryService.RepositoryList` (stream),
`RepositoryRegister`, `RepositoryUnregister` (`:21-34`). `PartitionList` no
longer exists.

| Caller | RPC | Token | Anchor |
|---|---|---|---|
| Client | `UserGet` | partition-scoped | `lore-transport/src/auth/user_service_v1.rs:65-88` |
| Client | `UserFind` (username, then display name) | partition-scoped | `user_service_v1.rs:44-60,90-115` |
| Server | `RepositoryRegister` before create; failure refuses create | caller's login token | `authnz/repository_registry.rs:57-97` |
| Server | `RepositoryUnregister` before delete | scoped or login token | `repository_registry.rs:101-135`, `user.proto:110-115` |
| Server | `RepositoryList` for the listing catalog | caller's token | `authnz/repository_catalog.rs:318,466` |

Without `user_url`: client falls back to `auth_url`, then TokenOnly
(`user_directory.rs:24-34`); registry falls back to no registration; the catalog
falls back to `baseline_access` (default `denied`, `settings.rs:522-532`).

## 3. Browsing RPCs for a web BFF

Mounting — `lore-server/src/grpc/server.rs:812-894`; partition services are
wrapped by JWT + `PartitionAccessLayer`
(`grpc/tower/partition_access.rs:29-31,144`; partition from `lore-partition-bin`
or `urc-repository-id-bin`). `grpc_public_services` defaults not checked.

| Need | RPC | Anchor |
|---|---|---|
| Repositories | v1 `RepositoryService.RepositoryList` | `repository/v1/repository.proto:25` |
| Branches | v1 `RevisionService.BranchList`, `BranchGet` | `revision/v1/revision.proto:18,20` |
| History | v1 `RevisionList` | `revision.proto:30` |
| Tree | `ThinClientService.RevisionTree` (path_prefix, max_depth, per-file last revision) | `thin_client/v1/thin_client.proto:23` |
| One revision | `ThinClientService.RevisionInfo` | `thin_client.proto:17` |
| Revision diff | `ThinClientService.RevisionDiff` (paths, addresses, conflicts; no text) | `thin_client.proto:20` |
| Text diff | `ContentDiff` — unimplemented | `thinclient/v1/service.rs:99-106` |
| Branch diff | legacy `urc` `BranchDiff` (conflicts, `revision_base`) | `model.proto:222-250` |
| File bytes | `StorageService.Get` | `lore/storage/v1/storage.proto:23` |
| Locks | `urc.lock.LockService.Query` | `lock.proto:42` |

All reads need `read`; Lock/Unlock need `write`; force unlock needs `owner` or
`admin` (`grpc/lock_service.rs:232-410`). Per-line audit is client-side only
(`lore-revision/src/file/audit.rs`; server RPC deferred in
`docs/proposals/2026-09-23-per-line-attribution-audit-api.md`), so the portal
computes text diffs and attribution itself.

Crates: `lore-proto` (tonic clients incl. ThinClient; the BFF sets the partition
header itself); `lore-revision` `RepositoryContext::new_server_context` works on
stores without a filesystem (`lore-revision/src/repository.rs:1306`).

## 4. Merge, conflicts, protection

- Server merge exists only inside push: `fast_forward_merge` on `BranchPush`
  (`revision.proto:130-132`; `grpc/handlers/branch_push.rs:1362`, declined on
  conflict `:1414`). It replays a pushed revision onto the same branch head.
- Conflicts and merge base: `BranchDiff` (`handlers/branch_diff.rs:106-146`),
  ThinClient `RevisionDiff`; `resolve_diff3_base` is library-only
  (`lore-revision/src/branch.rs:4521`). (subagent)
- Client merge (`merge_start`, `merge_into`, `merge_resolve`,
  `lore-revision/src/branch/merge.rs`) starts from local staged state. (subagent)
- `BranchProtect` needs `owner`/`admin` (or the creator with `write`)
  (`grpc/mod.rs:496-526,569`). Push to a protected branch needs `push-protected`
  (`branch_push.rs:847-871,1061-1067`, subagent). No ref patterns, no
  required-review rules.

## 5. Events

`lore-proto/proto/lore_notification.proto:50-91`: `BranchPushed`, `BranchCreated`,
`BranchDeleted`, `ResourceLocked`, `ResourceUnlocked`, `Obliterate`,
`ExtensionEvent`. One in-memory broadcast per repository, capacity 200
(`notification/local.rs:35,50`); laggers lose events, no cursor or ack.
Subscribe needs `read`. Hooks need a derived binary; the stock server has none.

## 6. Lore docs on review

`docs/roadmap.md:18,81-85` (web client and code review, 2027);
`docs/glossary.md:52-54` (change request); `docs/explanation/system-design.md:664-665,1228-1246,1444-1446`
(review tools are CLI peers; application metadata like `review-id`/`review-status`
on revisions; per-type diff rendering).

## Consequences for Swarog

- Swarog implements a standard OIDC provider (discovery, JWKS with `kid`, device,
  refresh, client credentials, revocation, RFC 8693 exchange with RFC 8707
  `resource` for Tier 2) plus `lore.user.v1` at `user_url`. The October
  `UrcAuthApi` adapter phase is dropped.
- Access tokens must name the Lore host in `root_domains` or `aud`, or the CLI
  refuses to send them.
- `RepositoryRegistryService` is Swarog's repository inventory; no second
  inventory.
- The `lore.*` event bridge must tolerate gaps (re-read branch tips after
  reconnect).
- The portal computes text diffs and line attribution itself from file bytes.

## Open

- Whether `lore-revision`'s merge functions are `pub` and usable against remote
  stores (decides whether a Swarog-side "merge A into B" is possible).
- `subject_token_type` values the CLI sends for API-key login (`lore/src/auth.rs`).
- `grpc_public_services` defaults; whether legacy `BranchDiff` stays reachable.
- Owner decisions: Tier 1 vs Tier 2; server-side merge (link `lore-revision`) vs
  client-completed merge; review state in Swarog tables, revision metadata, or both.

## Not checked

Catalog filtering beyond selection code; `branch_unprotect.rs`; `StorageService.Get`
reassembly of fragmented files; QUIC/HTTP content routes; the full OIDC proposal
text; ADR 00018; plugin traits; `lore`/`lore-client` remote-read APIs.
