---
paths:
  - "modules/**"
  - "api/**"
  - "cmd/**"
  - "demos/**"
  - "core/admin/**"
---

# Domain modules

- **accounts** — identity: one `player_id`, many identities (`provider`,`subject`),
  opaque DB sessions: 60-minute access tokens plus rotating 30-day refresh-token
  families (`POST /accounts/refresh`) with reuse detection, a 30s grace window for a
  lost response, and a family-scoped kill — a replay outside the window revokes that
  family only, never the player's other devices. Dev/password auth (argon2id,
  `ACCOUNTS_DEV_AUTH`
  explicit-only — default OFF/fail-closed, loud warn when ON; the `devctl` and
  split-proof development profiles set `ACCOUNTS_DEV_AUTH=1`), Epic OIDC verifier
  (`EPIC_CLIENT_ID`, JWKS, RS256/ES256),
  Epic web OAuth link/login (`EPIC_CLIENT_SECRET`, `/accounts/epic/start|callback`).
  Federated login is one op — `login_federated(provider, credential)` over the
  provider registry (`epic`, `google`, `guest`); `KNOWN_PROVIDERS` means *buildable*,
  so an unknown name is 400 and a known-but-unconfigured one is 503. `create_guest`
  mints an anonymous player + a one-time device ticket replayed through
  `login_federated("guest", …)`; `POST /accounts/link` attaches a verified identity to
  the caller's player, and a guest gaining its first non-guest identity emits durable
  `player.promoted`. Provider name constants are the contract's
  (`accountsevents::providers`), re-exported by the impl. Emits durable
  `player.registered`. The gateway's session verifier resolves
  `accountsapi::Sessions` — a process hosting a gateway without the accounts
  capability FAILS STARTUP unless `ACCOUNTS_DEV_AUTH=1` (dev verifier, loud warn).
  `wallet` grants starter currency on `player.promoted` (and on non-guest
  `player.registered`) when configured. Self-delete is two ops: `POST
  /accounts/delete/begin` mints a server-minted, upserted confirmation ticket
  (a repeat call returns the same live ticket, never a second one); `POST
  /accounts/delete` consumes it and, in one transaction, revokes every session,
  reserves the caller's handle PERMANENTLY (`accounts.handles`, closing
  impersonation-by-recycling), writes a permanent receipt (`accounts.
  deletion_receipts`), deletes the player row (cascading identities/sessions/
  refresh tokens), and emits durable `player.deleted` (`player_id` + handle,
  `MinRetention{90d}`) — an unknown/expired/foreign ticket is `NotFound`, never
  `Forbidden`. `#[retry_safe]` on `delete_account` covers only the
  gateway→accounts internal-edge replay (the receipt answers it with the
  original success); a client re-POST is a 401 by construction, since the
  session died with the player. Seven modules (`characters`, `inventory`,
  `friends`, `groups`, `notifications`, `wallet`, `storage`) each subscribe to
  `player.deleted`, tombstone the player in a per-module `purged_players`
  table, and drain it via a daily `player-purge` scheduler-driven sweep (plus
  `party`, purging in the delivery tx without a tombstone) — see
  the `accounts` self-delete row in `docs/roadmap/feature-tracker.md`; open gaps
  in `docs/todo/account-deletion.md` and `docs/todo/accounts.md`.
- **characters / inventory** — the modularity reference case: plain-id relations,
  sync `Ownership` authz over the wire, starter-grant/wipe via durable
  `character.created/deleted`. `INVENTORY_DEV_GRANT` (explicit-only — default
  OFF/fail-closed, set by the `devctl` and split-proof development profiles) enables the simulated-IAP
  grant route.
- **config** — DB-backed knobs with a monotonic `config.revision`. A row trigger
  (INSERT/UPDATE/DELETE) increments the revision, NOTIFYs `config_changed`, and
  appends durable `config.changed` via `asyncevents.append_event` — a raw psql
  write emits identically to a service write. The NOTIFY payload is value-less
  (`namespace`/`key`/`operation`/`revision` only — `pg_notify` hard-caps at 8000
  bytes and the invalidation callback re-reads the whole snapshot anyway); the
  durable `config.changed` event still carries the full `value`. Snapshot =
  `{revision, settings}` in one statement. Local `Service` and remote
  `CachedConfig` (via `configrpc`) are invalidation callbacks (atomic map swap,
  apply only newer revisions); `CachedConfig` keeps boot-fill-or-fail-startup.
- **admin** — GameOps portal at `/admin` (minijinja over the embedded Go-era theme).
  **Session auth** (owns schema `admin`: users/sessions/login_attempts): argon2id
  passwords, opaque token + per-session CSRF in an `HttpOnly`/`SameSite=Strict`/
  `Path=/admin` cookie (`Secure` unless `ADMIN_COOKIE_SECURE=0` — dev opt-out, loud
  warn), 12h TTL; asymmetric lockout (user locks at 5 fails, IP at 20,
  `least(2^fails,900)s` backoff, trusted-proxy client IP via `TRUSTED_PROXY_CIDRS`);
  one generic 401 for wrong-pass/unknown-user/locked (no username oracle); CSRF
  checked BEFORE the local/remote editability decision; security headers on the
  admin router only. Login admission is bounded at 32 concurrent requests and
  5 rps/burst 20 per resolved client IP; Argon2 runs in `spawn_blocking` behind
  2 permits, owned BY the blocking closure (not the async handler frame) so a
  cancelled request can't release its permit while the hash keeps running
  detached — login admission (`login_slots`/`IpLimiter`) still releases on
  cancel by design. Username input is capped at 128 bytes, password input at 1024 bytes,
  and stale unlocked `login_attempts` rows older than 24h are deleted in batches
  of 256. The `admin_login_attempts_updated_idx` addition rolls out by `DROP SCHEMA
  admin CASCADE`, fresh boot, then user reseed with `adminctl` — no data migration,
  backfill, or compatibility bridge. Admin users are created by **`cargo run -p adminctl`**
  (`create-user` upsert = also password reset, `--password-stdin`/`ADMINCTL_PASSWORD`,
  never argv) wrapped by the **`install`** script for your shell; zero-user boot warns
  instead of failing; `ADMIN_OPEN=1` bypasses sessions AND CSRF (deliberately open
  local portal, loud warn). `ADMIN_USER`/`ADMIN_PASS` no longer exist. Emits durable
  `admin.action` (login-succeeded/login-locked/logout — local in BOTH topologies —
  plus form-submit where the form's module is co-hosted; field names only, never
  values). Renders contributed `adminapi::Item`s; remote items fan out over QUIC via
  `admin.adminData` (`adminrpc::admin_remote_factory`). Remote forms are read-only.
  admin-svc has a DB (schema `admin` + the durable plane) — no longer planeless.
- **audit** — append-only ledger (`audit.log`), zero-coupling raw durable sinks for
  all 16 ledger topics (`character.created/deleted`, `player.registered`,
  `player.promoted`, `player.deleted`, `config.changed`, `match.finished`,
  `admin.action`, `wallet.changed`, `friend.requested/accepted/removed`,
  `group.created/member_joined/member_left/role_changed`) — sixteen
  independent subscriptions (`audit.<topic-kebab>.v1`), each
  with its own checkpoint, plus a 17th independent subscription for prune reacting
  to `scheduler.fired{audit-prune}`
  (`AUDIT_RETENTION_DAYS`, default 30).
- **scheduler** — data-driven schedules (`scheduler.schedules`), 1s tick, per-name
  `pg_try_advisory_lock` + still-due re-check + `UPDATE`+`emit_tx` in one tx,
  commit-before-unlock. Each fire runs on its own DEDICATED connection (derived
  from the pool's connect options — dropping it closes the session, so an abort
  can never strand the advisory lock in the pool), acquire/connect waits are
  bounded (5s), the whole tick shares ONE 30s budget (exhaustion skips the
  remaining due schedules to the next tick), and `stop()` grace-then-aborts its
  tasks (4s, under the app-level 5s) instead of joining forever.
  `SCHEDULER_ENABLED`.
- **match / rating / leaderboard** — match records `match.matches` from a
  `/match/report` HTTP request body (a REQUIRED `ReportId` idempotency key —
  a duplicate `ReportId` with the SAME winner/loser is a 202 no-op, a duplicate
  `ReportId` with a DIFFERENT winner/loser is a 409 Conflict, and `report` is
  explicitly `#[retry_safe]`, so a replay after an ambiguous result can't double-commit —
  plus Go-parity keys `Winner`/`Loser`) and emits a
  durable `match.finished` event (snake_case payload keys `winner`/`loser` — a
  distinct shape from the HTTP body); rating is a persistent MMR projection
  (`rating.ratings`, ±15 from 1000, upserted in the delivery tx — restarts
  preserve MMR) provided as wire-only `MmrReader`; leaderboard upserts wins in
  the delivery tx, serves `GET /leaderboard`.
- **apikeys** — per-key API access policy (à la Supabase anon/service key): normalized
  schema `apikeys.roles(name, policy, revision)` + `apikeys.keys(name, secret_hash,
  prefix, role → roles.name, revision, revoked_at)` (sessions-token trust model, CAS by
  `revision`). Secrets are server-generated and stored ONLY as a SHA-256 digest plus a
  display prefix — the plaintext is shown exactly once on create, never re-derivable
  from a read. A key references a role; editing a role's policy immediately re-scopes
  every key pointing at it (JOIN, no denormalized policy). Provides `apikeysapi::Keys`
  (`apikeys.keys`); the gateway REQUIRES an `X-Api-Key` header
  (HTTP) / `api_key` envelope field (player-QUIC) on every op-dispatched request
  and enforces the key's policy post-match (401 missing/invalid, 403 denied;
  503 — distinct from 401 — when the verifier itself is load-shedding, e.g. a
  store blip or the flight-table saturated, so an uncached-but-valid key is
  never reported as invalid), behind a 5s TTL cache (never caches infra
  errors). Non-goals: `/healthz`,
  `/metrics`, passthroughs stay keyless. Dev keys `dev-key-client`
  (player-facing list, NO `match.report`) + `dev-key-server` (`full`) seed ONLY
  when `APIKEYS_DEV_SEED` is explicitly truthy (self-healing upsert); a gateway
  process without the capability FAILS STARTUP unless `APIKEYS_DEV_ALLOW=1`
  (allow-all, loud warn). Admin page "API Keys" is a rich, remotely-editable
  configurator (role + key CRUD, typed fields, show-once secret reveal via
  `adminapi::AdminSubmit`/`admin.adminSubmit` — editable in both topologies).
- **wallet** — virtual currency: an operator-owned catalog (`wallet.currencies`),
  per-player balances (`wallet.balances`) and an append-only `wallet.ledger`, the
  authority for every movement. `credit`/`debit` are wire-only (no `#[http]`, no
  `Identity`) — a client can never move its own money; `Player::my_balances`/
  `list_currencies` are the read-only player face. A movement's idempotency
  identity is `(player_id, currency, signed delta, reason)` under a REQUIRED,
  ledger-`UNIQUE` `idempotency_key`, licensing `#[retry_safe]` on `credit`/`debit`
  the same way `match.report` licenses it on `ReportId`. An optional, config-driven
  starter grant reacts to durable `player.promoted` and to non-guest
  `player.registered` (`AfterRegistration`, since the topics retain 7 days) — two
  subscriptions sharing one `starter:{player_id}` key, so a guest who registers and is
  later promoted is granted exactly once, and a guest who never promotes is never
  granted. It emits durable `wallet.changed` in the same
  transaction as the balance update and ledger row. Dev currencies `gold`/`gems`
  seed ONLY when `WALLET_DEV_SEED` is explicitly truthy (explicit-only,
  default OFF/fail-closed, loud warn when ON); the seed is insert-if-absent, so an
  operator's catalog edit survives a restart. Admin page "Wallet" is a
  remotely-editable configurator (create-currency/grant/revoke) under Economy.
- **notifications** — per-player inbox (schema `notifications`, table `messages`),
  the first module whose job is to *consume* other modules' durable events. Two
  content subscriptions, both `AfterRegistration` (`Genesis` would replay retained
  history into every existing inbox): `notifications.wallet-changed.v1` (only when
  `delta > 0`) and `notifications.player-promoted.v1` (a welcome row); plus
  `notifications.prune-on-scheduler.v1` reacting to
  `scheduler.fired{notifications-prune}`. Player face: `POST /notifications/list`
  (opaque keyset cursor in the request BODY — the `#[http]` grammar has no
  query-parameter source), `POST /notifications/{id}/read` (idempotent, keeps the
  first `read_at`), `DELETE /notifications/{id}` (not `#[retry_safe]` — a replay is
  404). Another player's row is `NotFound`, never `Forbidden` — a 403 is an
  enumeration oracle. Admin page "Inbox" under a new "Player Support" section
  (remotely editable via `admin.adminSubmit`); operator mail's idempotency key
  shares the `source_event_id` column with event dedup, disjoint by an
  `admin-send-mail-` prefix, and a resubmit with an edited body is 409, never a
  silent success. `NOTIFICATIONS_RETENTION_DAYS` (default 30, range 1..=3650) —
  unset takes the default, anything present but unusable FAILS STARTUP. Known
  gap: `match.finished` produces no inbox row, because its `winner`/`loser` are
  opaque contestant strings, not `player_id`s.
- **mail** — the backend's first outbound-to-the-world channel: a durable outbox
  (schema `mail`, table `outbox`) drained by a retry worker against a configured
  provider (`log` dev sink or real `smtp` via `lettre`; `MAIL_PROVIDER`/`MAIL_FROM`
  required together, and an unconfigured channel still enqueues while its
  `/readyz` reports permanently not-ready). Ingress is the durable
  `mail.send_requested` topic, which `mail` both defines and subscribes to — a
  deliberate deviation from "publisher owns the event, consumer owns the
  subscription": the topic is a *command* ("send this"), not a fact about another
  domain. Enqueue is exactly-once per `idempotency_key`; delivery to the
  recipient is at-least-once (a crash between the relay's `250` and the status
  commit re-sends). Scheduled pruning via `mail.prune-on-scheduler.v1`
  (`MAIL_RETENTION_DAYS`, default 30). Admin page "Mail" under Platform, remotely
  editable via `admin.adminSubmit` (send/cancel/requeue/`requeue-all-parked`).
  `mail` stores no address, so nothing resolves a `player_id` to a recipient, and
  `cancel` cannot recall a message the relay already accepted. Open gaps:
  `docs/todo/mail.md`.
- **friends** — the social graph: schema `friends`, one table `edges` with a
  canonical ordered pair (`low_id < high_id` CHECK + unique index, so symmetry
  and pair-uniqueness live in the database) plus a separate `requester_id`,
  since the *transitions* are not symmetric. States are `pending`/`accepted`
  (open gaps: `docs/todo/friends.md`). Six `#[http]` ops (`request`/`accept`/`decline`/`remove`/
  `list`/`pending`, all `auth = "player"`, mutations addressed by `edge_id`);
  reads are `#[retry_safe]`, mutations are not. Another player's edge is
  `NotFound`, never `Forbidden` — a 403 is an enumeration oracle. `request`
  resolves a target by handle (`Name#1234`, minted in `accounts` via the new
  wire-only `accountsapi::Directory`, `players_by_id` batched + `find_by_handle`).
  Three durable topics at 30-day retention
  (`friend.requested/accepted/removed`, `Removed.reason` folding decline/unfriend/
  withdraw into one topic), each denormalizing both parties' handles because
  `notifications`' handler runs inside the delivery transaction with no accounts
  stub available; consumed by `audit` (three raw sinks) and `notifications` (two
  `AfterRegistration` subscriptions). Presence is session-derived — `online_until`
  (RFC3339, empty = no live session) computed from `accounts.sessions`, a
  timestamp rather than a socket-backed `online` bool, since a quit player reads a
  future value for up to an hour (60-minute access tokens); socket presence did
  not ship (gateway has no DB, presence is per-process RAM, the offline
  transition runs in a `Drop` that cannot `await`, and there is no fan-in
  primitive). Read-only admin page "Friends" under Player Support plus a "View
  Friends" entry on the Players row menu.
- **groups** — social groups: schema `groups`, a `groups` row plus a `memberships`
  row per `(group_id, player_id)` with three states (`member`/`invited`/
  `requested`) and two roles (`admin`/`member`); `MAX_MEMBERS` (500) is enforced
  under a per-group `pg_advisory_xact_lock`, not a count-then-insert, which two
  concurrent joins would both pass under READ COMMITTED. Nine `#[http]` ops
  (`create`/`list_mine`/`members`/`pending`/`join`/`leave`/`invite`/`respond`/
  `decide`) plus a wire-only `role_of` predicate (`groupsapi::Membership`,
  reachable ONLY over the internal edge, never the front door) for `chat` to
  authorize a group channel against. `list_mine` doubles as the invitation inbox
  — it returns all three states, not just `member`. Another player's view of a
  group it cannot see is `NotFound`, never `Forbidden`. Four durable topics at
  30-day retention (`group.created`, `group.member_joined`, `group.member_left`,
  `group.role_changed`), consumed by `audit` (four raw sinks); a
  `groups.prune-on-scheduler.v1` sweep deletes stale `invited`/`requested` rows
  past `GROUPS_RETENTION_DAYS` (default 30) and emits `group.member_left`
  (`reason = "expired"`) for each swept row rather than leaving the ledger
  silent. Admin page "Groups" under Player Support includes an operator
  member→admin promotion, emitting `group.role_changed`. Open gaps:
  `docs/todo/groups.md`.
- **storage** — player cloud-save in schema `storage`, addressed by
  `(owner_id, collection, key)`. Four player ops (`put`/`get`/`delete`/`list`)
  provide owner-only writes, `private`/`public_read` reads, and keyset paging
  of the caller's own collection. `put(expected_version = 0)` creates or
  returns 409; positive versions use compare-and-swap, with no blind
  overwrite. Values are JSON text (65,536 input bytes), stored as `jsonb` and
  read back normalized; byte identity is not preserved. Collection/key
  segments use `[A-Za-z0-9._-]`, capped at 64/128 bytes in the service across
  transports. The 10,000-object owner cap and purge sweep share an owner
  advisory lock. `storage.player-deleted.v1` plants permanent tombstones;
  `storage.purge-on-scheduler.v1` drains up to 64 owners per daily
  `player-purge` fire, with a 5s budget checked between owners. The seventh
  purge module and eighth `player.deleted` consumer (including `audit`),
  with no published events or synchronous domain dependencies. Read-only
  admin page "Cloud Saves" under Player Support at `/admin/cloud-saves`.
  Known limits and acceptance status live in the storage section of
  `docs/roadmap/feature-tracker.md`.
- **party** — short-lived player parties, at most `MAX_MEMBERS` (8), one leader, one
  party per player (the `members.player_id` primary key; a racing second insert is
  409). Postgres is the authority (schema `party`); every change to an EXISTING
  party's membership takes a per-party `pg_advisory_xact_lock`, while `create`
  relies on the `members.player_id` primary key alone. Eleven `#[http]` ops (`create`/`get`/`invite`/
  `invites`/`accept`/`decline`/`leave`/`kick`/`promote`/`set_ready`/`heartbeat`);
  `get`/`invites`/`heartbeat` are `#[retry_safe]`, mutations are not. Another
  player's party or invite is `NotFound`, never `Forbidden`; `invite` resolves the
  handle through `accountsapi::Directory` (outage = 503). Membership is a lease
  (`PARTY_LEASE_SECS`, default 60, range 10..=600 — only an UNSET variable takes
  the default, a blank one fails startup): every member op renews it, a module-owned
  5s sweep removes expired members under the party lock, and a stalled or dead
  sweep turns the `"party"` readiness check red. `revision` comes from ONE global
  sequence; the stamping ops are create/accept/leave/kick/promote/set_ready, a lease
  removal and a purge removal, each stamping the party and every player whose
  membership it changed, and a "no party" view carries that player's own last
  `party.player_revisions` value (`0` if none). Client rule: apply a snapshot only
  when `revision >= hw`, then `hw = revision` (`hw` resets per connection). After
  commit the module pushes `party.state` to every member (and a "no party" view to a
  removed one) and `party.invite` to the invitee on hub `party` — best-effort, via
  `ctx.push()`, so in the split `party-svc` holds a `remote::PushSender` to
  gateway-svc; a client can invoke the ops over `/push` as well as HTTP. A purge
  removal is not pushed. `party.player-deleted.v1` (`AfterRegistration`) applies the leave rule to
  a deleted player in the delivery tx and deletes their invites and revision row;
  there is no tombstone table, and `party` is not one of the seven purge modules.
  No durable topics and no admin page. Known gaps are in the Parties row of
  `docs/roadmap/feature-tracker.md`.
- **gateway** — the front-door module: HTTP ops routing (Local vs Remote purely by
  slot presence; peer addresses are injected by `cmd/*` via `remote::Stub` →
  `opsapi::PEER_SLOT` contributions — the gateway module itself never reads env),
  a `GET /push` WebSocket hub (a hub is just a topic-prefix label — no
  per-hub subscription or authz; server-minted connection id,
  `Target::Player|Group|All`, ephemeral non-authorizing groups, front-local
  presence default off; each `message` frame carries a `hub` label and rides one
  of at most 16 event lanes (the first 15 named hubs get their own, the rest share lane
  `""`), and a client may `invoke` any `#[http]`-bound player op over the
  socket and get a correlated `result` under the same API-key policy and, for
  player-auth ops, session check) resolving `push::Target`s against the sockets it owns
  and contributing the inbound `push.deliver` face to `edge::EDGE_SLOT` so a
  producer elsewhere in the split can reach them (`docs/reference/push-hub.md`),
  player-QUIC plane (bearer-in-envelope, exact-method allow-list), HTTP passthrough
  (`/admin`, `/accounts/epic` → origins passed in by `cmd/gateway-svc` via
  `Gateway::with_passthrough`, env read in the main, not the module), always-on
  rate limit in gateway-svc (20 rps/burst 40), and **native TLS termination**
  (mechanism in `core/app` — `Config::with_tls(TlsFronts)`; env read in
  each public front main (`cmd/gateway-svc`, `cmd/server`) and parsed by
  `app::TlsFronts::from_values`: `TLS_MODE=off|files|acme` (default off),
  `TLS_CERT_PATH`+`TLS_KEY_PATH`, `ACME_DOMAINS`/`ACME_CONTACT`/`ACME_CACHE_DIR`;
  rustls-acme TLS-ALPN-01 auto-renew, ring-pinned — `aws-lc-rs` must never enter
  the tree). The player QUIC front's certificate is chosen once at boot by
  `resolve_player_tls`: `PLAYER_TLS_CERT_PATH`+`PLAYER_TLS_KEY_PATH` (both or
  neither) > the HTTPS front's cert (`files`, or the one shared ACME state) > a
  dev-CA leaf with a boot `warn!` when `TLS_MODE=off`. The FrontDoor is hosted ONLY by the front
  processes (`cmd/gateway-svc`, the monolith `cmd/server`); a domain svc NEVER hosts it —
  it serves ops over the internal mTLS edge and gateway-svc dispatches Remote. Enforced by
  `archcheck` (only gateway-svc + server may depend on the `gateway` crate).
  Player QUIC request buckets use `PLAYER_RATE_LIMIT_RPS` (default 20),
  `PLAYER_RATE_LIMIT_BURST` (40), `PLAYER_CONN_RATE_LIMIT_RPS` (10), and
  `PLAYER_CONN_RATE_LIMIT_BURST` (20): the first pair limits a source IP across
  reconnects and the second limits each persistent connection. Admission
  itself is gated on a validated source address: an unvalidated `Incoming`
  gets a stateless QUIC Retry and reserves no connection slot, so an off-path
  source-spoof flood can't exhaust the admission budget — a slot is taken only
  once the dial re-arrives with the Retry token echoed back.

Not a module: **`demos/webui`** — dev demo SPA at `/` exercising the accounts flow
from a browser. Non-shipping, monolith-only (registered in `cmd/server` only;
archcheck forbids any other consumer of a `demos/*` crate).
