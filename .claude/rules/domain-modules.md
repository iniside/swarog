---
paths:
  - "modules/**"
  - "api/**"
  - "cmd/**"
  - "demos/**"
  - "core/admin/**"
---

# Domain modules

Per-module mechanics live in each `modules/<name>/src/lib.rs` header and the
`api/<name>/` traits; this file keeps cross-module contracts, seam choices and gotchas.

- **accounts** — Federated login is one op over the provider registry;
  `KNOWN_PROVIDERS` means *buildable*, so an unknown name is 400 and a
  known-but-unconfigured one is 503. Provider name constants are the contract's
  (`accountsevents::providers`), re-exported by the impl. Refresh reuse outside the
  30s grace window kills that family only, never the player's other devices. A guest
  gaining its first non-guest identity emits durable `player.promoted`. The gateway's
  session verifier resolves `accountsapi::Sessions` — a process hosting a gateway
  without the accounts capability FAILS STARTUP unless `ACCOUNTS_DEV_AUTH=1` (dev
  verifier, loud warn); `ACCOUNTS_DEV_AUTH` is explicit-only (default OFF) and the
  `devctl` and split-proof development profiles set it. `wallet` grants starter
  currency on `player.promoted` (and non-guest `player.registered`) when configured.
  Self-delete: `begin` mints/upserts a confirmation ticket (a repeat returns the same
  live one); `delete` consumes it and in one tx revokes sessions, reserves the handle
  PERMANENTLY (closing impersonation-by-recycling), writes a permanent receipt,
  deletes the player row and emits durable `player.deleted` (`MinRetention{90d}`). An
  unknown/expired/foreign ticket is `NotFound`, never `Forbidden`. `#[retry_safe]` on
  `delete_account` covers only the gateway→accounts internal-edge replay (the receipt
  answers it); a client re-POST is a 401 by construction. Seven modules
  (`characters`, `inventory`, `friends`, `groups`, `notifications`, `wallet`,
  `storage`) subscribe to `player.deleted`, tombstone the player in a per-module
  `purged_players` table and drain it via a five-minute `player-purge` sweep (`party`
  purges in the delivery tx without a tombstone). Tracker row and open gaps:
  `docs/roadmap/feature-tracker.md` (`accounts` self-delete),
  `docs/todo/account-deletion.md`, `docs/todo/accounts.md`.
- **characters / inventory** — the modularity reference case: plain-id relations,
  sync `Ownership` authz over the wire, starter-grant/wipe via durable
  `character.created/deleted`. `INVENTORY_DEV_GRANT` (explicit-only, default
  OFF/fail-closed, set by the `devctl` and split-proof development profiles) enables
  the simulated-IAP grant route.
- **config** — The NOTIFY payload is value-less (`pg_notify` hard-caps at 8000 bytes
  and the invalidation callback re-reads the whole snapshot anyway); the durable
  `config.changed` event still carries the full `value`. Local `Service` and remote
  `CachedConfig` (via `configrpc`) apply only newer revisions; `CachedConfig` keeps
  boot-fill-or-fail-startup. Each `config.settings` row carries `revision`, the
  global revision of the write that last wrote it, stamped by the `settings_stamp`
  BEFORE trigger (writers never supply it); the admin edit compares-and-swaps on it.
  The column rolls out by `DROP SCHEMA config CASCADE` and a fresh boot, no data
  migration: an old table boots but fails on its first write.
- **admin** — `core/admin` (a core module, not under `modules/`): GameOps portal at `/admin` (minijinja over the embedded Go-era theme),
  owning schema `admin`. CSRF is checked BEFORE the local/remote editability
  decision; one generic 401 for wrong-pass/unknown-user/locked (no username oracle);
  `Secure` cookie unless `ADMIN_COOKIE_SECURE=0` (dev opt-out, loud warn). The argon2
  permit is owned BY the blocking closure (not the async handler frame) so a
  cancelled request can't release it while the hash keeps running; login admission
  (`login_slots`/`IpLimiter`) still releases on cancel by design. The
  `admin_login_attempts_updated_idx` addition rolls out by `DROP SCHEMA admin CASCADE`,
  fresh boot, then user reseed with `adminctl` — no data migration. Users are created
  by **`cargo run -p adminctl`** (`create-user` upsert = also password reset,
  `--password-stdin`/`ADMINCTL_PASSWORD`, never argv) wrapped by the **`install`**
  script; zero-user boot warns instead of failing; `ADMIN_OPEN=1` bypasses sessions
  AND CSRF (deliberately open local portal, loud warn). `ADMIN_USER`/`ADMIN_PASS` no
  longer exist. Emits durable `admin.action` (login events local in BOTH topologies;
  form-submit where the form's module is co-hosted; field names only, never values).
  Remote providers are reached over the QUIC `cp.*` face
  (`controlplanerpc::register_face` / `remote_factory`; providers contribute to
  `controlplane::PROVIDER_SLOT`). admin-svc has a DB (schema `admin` + the
  durable plane).
- **audit** — zero-coupling raw durable sinks for the 16 ledger topics
  (`character.created/deleted`, `player.registered`, `player.promoted`,
  `player.deleted`, `config.changed`, `match.finished`, `admin.action`,
  `wallet.changed`, `friend.requested/accepted/removed`,
  `group.created/member_joined/member_left/role_changed`): sixteen independent
  subscriptions (`audit.<topic-kebab>.v1`) plus a 17th reacting to
  `scheduler.fired{audit-prune}` (`AUDIT_RETENTION_DAYS`, default 30). A new logged
  topic is a required edit to `DURABLE_TOPICS`.
- **scheduler** — each fire runs on its own DEDICATED connection so an abort can
  never strand the advisory lock in the pool; the whole tick shares ONE 30s budget
  (exhaustion skips the remaining due schedules to the next tick); `stop()`
  grace-then-aborts (4s, under the app-level 5s). `SCHEDULER_ENABLED`.
- **match / rating / leaderboard** — `/match/report` requires a `ReportId`
  idempotency key: a duplicate with the SAME winner/loser is a 202 no-op, with a
  DIFFERENT winner/loser a 409; `report` is explicitly `#[retry_safe]`. HTTP body keys
  are Go-parity `Winner`/`Loser`; the durable `match.finished` payload is snake_case
  `winner`/`loser` — a distinct shape. Rating is a persistent MMR projection provided
  as wire-only `MmrReader`; restarts preserve MMR.
- **apikeys** — A key references a role; editing a role's policy re-scopes every key
  pointing at it. Secrets are stored ONLY as a SHA-256 digest plus display prefix;
  plaintext is shown once on create. The gateway REQUIRES `X-Api-Key` (HTTP) /
  `api_key` (player-QUIC) on every op-dispatched request: 401 missing/invalid, 403
  denied, 503 (distinct from 401) when the verifier itself is load-shedding, so an
  uncached-but-valid key is never reported as invalid; 5s TTL cache never caches infra
  errors. `/healthz`, `/metrics` and passthroughs stay keyless. Dev keys
  `dev-key-client` (NO `match.report`) + `dev-key-server` (`full`) seed ONLY when
  `APIKEYS_DEV_SEED` is explicitly truthy; a gateway process without the capability
  FAILS STARTUP unless `APIKEYS_DEV_ALLOW=1` (allow-all, loud warn). The admin page
  is a `controlplane::Provider` in `PROVIDER_SLOT`, remotely reachable via the
  `cp.*` face in both topologies.
- **wallet** — `credit`/`debit` are wire-only (no `#[http]`, no `Identity`): a client
  can never move its own money. A movement's idempotency identity is `(player_id,
  currency, signed delta, reason)` under a REQUIRED, ledger-`UNIQUE`
  `idempotency_key`, licensing `#[retry_safe]` on `credit`/`debit` the way
  `match.report` licenses it on `ReportId`. The optional starter grant reacts to
  `player.promoted` and non-guest `player.registered` (`AfterRegistration`, topics
  retain 7 days) through two subscriptions sharing one `starter:{player_id}` key, so
  a guest who registers then promotes is granted exactly once and one who never
  promotes never. Dev currencies `gold`/`gems` seed ONLY when `WALLET_DEV_SEED` is
  explicitly truthy; the seed is insert-if-absent so an operator's catalog edit
  survives a restart.
- **notifications** — the first module whose job is to *consume* other modules'
  durable events. Content subscriptions are `AfterRegistration` (`Genesis` would
  replay retained history into every existing inbox): `wallet.changed` (only
  `delta > 0`) and `player.promoted` (welcome row), plus a prune reacting to
  `scheduler.fired{notifications-prune}`. List cursor is opaque keyset in the request
  BODY (the `#[http]` grammar has no query-parameter source); `DELETE` is not
  `#[retry_safe]` (a replay is 404). Another player's row is `NotFound`, never
  `Forbidden` — a 403 is an enumeration oracle. Operator mail's idempotency key
  shares the `source_event_id` column with event dedup, disjoint by an
  `admin-send-mail-` prefix; a resubmit with an edited body is 409.
  `NOTIFICATIONS_RETENTION_DAYS` unset takes the default, present-but-unusable FAILS
  STARTUP. Known gap: `match.finished` produces no inbox row — its `winner`/`loser`
  are opaque contestant strings, not `player_id`s.
- **mail** — ingress is the durable `mail.send_requested` topic, which `mail` both
  defines and subscribes to — a deliberate deviation from "publisher owns the event,
  consumer owns the subscription": the topic is a *command*, not a fact about another
  domain. `MAIL_PROVIDER`/`MAIL_FROM` are required together; an unconfigured channel
  still enqueues while its `/readyz` reports permanently not-ready. Enqueue is
  exactly-once per `idempotency_key`; delivery is at-least-once. `mail` stores no
  address, so nothing resolves a `player_id` to a recipient, and `cancel` cannot
  recall a message the relay already accepted. Open gaps: `docs/todo/mail.md`.
- **friends** — `request` resolves a target by handle (`Name#1234`) via the wire-only
  `accountsapi::Directory`. Another player's edge is `NotFound`, never `Forbidden`.
  Three durable topics (`friend.requested/accepted/removed`, `Removed.reason`
  folding decline/unfriend/withdraw), each denormalizing both parties' handles
  because `notifications`' handler runs inside the delivery transaction with no
  accounts stub available. Presence is session-derived `online_until` (RFC3339,
  empty = no live session), a timestamp rather than an `online` bool, since a quit
  player reads a future value for up to an hour; socket presence did not ship
  (gateway has no DB, presence is per-process RAM, the offline transition runs in a
  `Drop` that cannot `await`, no fan-in primitive). Open gaps: `docs/todo/friends.md`.
- **groups** — `MAX_MEMBERS` is enforced under a per-group `pg_advisory_xact_lock`,
  not a count-then-insert (two concurrent joins would both pass under READ
  COMMITTED). The wire-only `role_of` predicate (`groupsapi::Membership`, internal
  edge only, never the front door) lets `chat` authorize a group channel. `list_mine`
  doubles as the invitation inbox (all three states). A group the caller cannot see
  is `NotFound`, never `Forbidden`. A prune sweep deletes stale `invited`/`requested`
  rows past `GROUPS_RETENTION_DAYS` and emits `group.member_left`
  (`reason = "expired"`) per swept row rather than leaving the ledger silent. Open
  gaps: `docs/todo/groups.md`.
- **storage** — Values are stored as `jsonb` and read back normalized; byte identity
  is not preserved. The owner object cap and the purge sweep share an owner advisory
  lock. Publishes no events and has no synchronous domain dependencies. Known limits
  and acceptance status: the storage section of `docs/roadmap/feature-tracker.md`.
- **party** — Another player's party or invite is `NotFound`, never `Forbidden`;
  `invite` resolves the handle through `accountsapi::Directory` (outage = 503). Every
  change to an EXISTING party takes a per-party `pg_advisory_xact_lock`; `create`
  relies on the `members.player_id` primary key alone. `PARTY_LEASE_SECS`: only an
  UNSET variable takes the default, a blank one fails startup; a stalled or dead
  sweep turns the `"party"` readiness check red. `revision` comes from ONE global
  sequence and a "no party" view carries that player's own last
  `party.player_revisions` value (`0` if none). Client rule: apply a snapshot only
  when `revision >= hw`, then `hw = revision` (`hw` resets per connection). Pushes
  (`party.state`, `party.invite`) are best-effort via `ctx.push()`, so in the split
  `party-svc` holds a `remote::PushSender` to gateway-svc; a purge removal is not
  pushed. `party.player-deleted.v1` applies the leave rule in the delivery tx; there
  is no tombstone table. No durable topics, no admin page. Known gaps: the Parties
  row of `docs/roadmap/feature-tracker.md`.
- **gateway** — Local vs Remote is decided purely by slot presence; peer addresses
  are injected by `cmd/*` via `remote::Stub` → `opsapi::PEER_SLOT` contributions and
  the module never reads env. Passthrough origins (`/admin`, `/accounts/epic`) are
  passed in by `cmd/gateway-svc` via `Gateway::with_passthrough`. A `/push` hub is
  just a topic-prefix label (no per-hub subscription or authz); a client may `invoke`
  any `#[http]`-bound player op over the socket under the same API-key policy and
  session check. It contributes the inbound `push.deliver` face to `edge::EDGE_SLOT`
  so a producer elsewhere in the split can reach its sockets
  (`docs/reference/push-hub.md`). The FrontDoor is hosted ONLY by `cmd/gateway-svc`
  and the monolith `cmd/server` (`archcheck`: only those may depend on the `gateway`
  crate); a domain svc serves ops over the internal mTLS edge. Native TLS termination
  is `core/app` (`Config::with_tls(TlsFronts)`); env is read in each public front
  main and parsed by `app::TlsFronts::from_values` (`TLS_MODE=off|files|acme`,
  default off); rustls-acme is ring-pinned — `aws-lc-rs` must never enter the tree.
  The player QUIC front's certificate is chosen once at boot by `resolve_player_tls`:
  `PLAYER_TLS_CERT_PATH`+`PLAYER_TLS_KEY_PATH` (both or neither) > the HTTPS front's
  cert > a dev-CA leaf with a boot `warn!` when `TLS_MODE=off`. Admission is gated
  on a validated source address: an unvalidated `Incoming` gets a stateless QUIC
  Retry and reserves no slot, so a spoofed-source flood can't exhaust the admission
  budget.

Not a module: **`demos/webui`** — dev demo SPA at `/` exercising the accounts flow
from a browser. Non-shipping, monolith-only (registered in `cmd/server` only;
archcheck forbids any other consumer of a `demos/*` crate).
