# Feature tracker — closing the gaps from the BaaS analysis

**Last update: 2026-09-10-1200**

**Living document, updated in place** (no date prefix in the filename — it is the
current state, not a dated snapshot; the date above moves instead). Source of the
feature list:
[docs/reference/game-backend-feature-gaps.md](../reference/game-backend-feature-gaps.md)
(analysis 2026-07-22). That doc is the *analysis* and is frozen; this doc is the
*tracker* and moves.

Tracker opened: 2026-07-28.

## How to use this

- **Bump "Last update" at the top on every edit** — `YYYY-MM-DD-HHMM`, the same stamp
  format the repo's plan/status filenames use.
- One row per feature from the gap matrix. **Never delete a row** — flip its status.
- When a feature lands, set status ✅, fill **Module(s)** with the real crate names and
  **Landed** with the commit sha + date.
- A feature is ✅ only when it works in **both topologies** (monolith + split) and has a
  named `tools/splitproof` assertion — monolith-only is not done
  ([[never-monolith-only-features]]).
- Partial (⚠️) rows must say *what* is missing in the Notes column, not just "partial".
- New gaps discovered during implementation get appended to the matrix with a
  `(added YYYY-MM-DD)` note in Notes — the gap analysis is not re-run.

Status legend: ✅ done · 🚧 in progress · 📝 planned (plan doc exists) · ⚠️ partial ·
❌ not started · ⛔ decided against (link the decision).

---

## Agreed sequence (decided 2026-07-28)

Rationale in the decision notes below; the order deviates from the gap doc's own
"suggested reading order" deliberately.

| # | Feature | Status | Plan doc |
|:-:|---|:--:|---|
| 1 | Virtual currency wallet + ledger | ✅ | [2026-07-28-2125-wallet-module-plan.md](../plans/2026-07-28-2125-wallet-module-plan.md) |
| 2a | Federated-provider seam + Google OIDC + guest/device | ✅ | [2026-07-30-2230-accounts-federated-providers-plan.md](../plans/2026-07-30-2230-accounts-federated-providers-plan.md) |
| 2b | Apple OIDC + identity link/unlink | ❌ | — |
| 3a | Notifications: in-app player inbox | ✅ | [2026-08-31-0032-notifications-inbox-3a-plan.md](../plans/2026-08-31-0032-notifications-inbox-3a-plan.md) |
| 3b | Outbound email channel | ✅ | [2026-08-31-1650-mail-outbound-channel-3b-plan.md](../plans/2026-08-31-1650-mail-outbound-channel-3b-plan.md) |
| 3c | Push notifications (FCM/APNs) | ❌ | — |
| 4 | Self-registration promoted to production (email verify, password reset) | ❌ | — |
| 5 | Leaderboard seasons / reset / rotation | ❌ | — |
| 6 | Steam auth (ticket verifier) | ❌ | — |
| 7 | Store + IAP receipt validation | ❌ | — |
| 8 | Real-time push hub (`GET /push` WebSocket, SignalR-shaped) | ✅ | [2026-09-03-2022-push-hub-websocket-plan.md](../plans/2026-09-03-2022-push-hub-websocket-plan.md) |

**Why this order (not the gap doc's):**

- **Wallet first** — it is the dependency floor for store, IAP, tournaments, battle
  pass, and *rewarded* leaderboard seasons. Textbook fit for the transactional model
  (balances + append-only ledger in its own schema, `WalletWriter` sync capability,
  `wallet.changed` durable event). Nothing new in the plumbing.
- **OIDC providers before Steam** — `OidcVerifier::new(jwks_url, issuer, audiences)`
  (`modules/accounts/src/oidc.rs`) is already provider-generic, so Google/Apple are
  configuration + Apple's signed-JWT client secret. Guest/device is a provider row with
  no verifier. Steam needs its own ticket verifier and outbound HTTP to Valve — separate
  rollout, separate error model.
- **#2 split into 2a/2b (decided 2026-07-30)** — the original single row bundled four
  items with unrelated risk profiles. 2a carries the *seam* plus the two cheap providers;
  2b carries the only real cryptography (Apple's ES256 client-secret JWT) and the only
  real policy work (link/unlink). Splitting keeps the public HTTP shape decision (below)
  in one rollout instead of hostage to Apple's key handling.
- **Notifications before self-registration** — self-registration needs email
  verification and password reset, and this backend has **no outbound mail channel at
  all**. That cost is not priced in the gap doc. Notifications is the place that channel
  belongs, and it independently unblocks friend invites, rewards, and moderation
  messages.
- **Seasons after wallet, not first** — the gap doc ranks seasons #1 as the cheapest
  "toy vs. real" tell, but seasons without rewards are half a feature. After wallet they
  cost the same and ship complete.
- **#3 split into 3a/3b/3c (decided 2026-08-31)** — the original single row bundled three
  items with unrelated risk profiles. 3a is durable-plane consumer work (a new fortress
  fanning inbox rows in from existing events); 3b is outbound I/O to the world with
  operator secrets and a different error model, and the hard prerequisite for #4; 3c is a
  third-party push transport. Splitting keeps 3a's plan from being hostage to the mail
  provider decision.

### Open decisions — settled by seq #2a (landed 2026-08-30)

These were the questions a plan for #2 could not leave to implementation; recorded
here so the sequence row stayed a one-liner while they were still open. All four are
now resolved — see the Identity & accounts table below for the landed shape.

1. **Per-provider method vs. one federated op.** Today the contract is a method per
   provider: `login_epic(id_token)` (`api/accounts/api/src/lib.rs:93`), with
   `Error::unavailable("epic provider not configured")` decided inside the service
   (`modules/accounts/src/lib.rs:476`). Adding Google and Apple this way copies that
   method three times, and Steam (seq #6) makes a fourth — a feature added by *editing*
   existing code, against the repo's Open/Closed-by-new-code rule. The alternative is one
   `login_federated(provider, credential)` op over an in-module verifier registry, so a
   later provider is new code, not another arm of a match. This is a #2a decision, not a
   #2b one: it changes the public HTTP surface, which is pinned by the `public-api`
   baseline and the contract-golden stage, so reversing it later is a contract migration.
   The store layer needs nothing either way — `accounts.identities` and
   `Store::link_identity` (`modules/accounts/src/store.rs:182`) are already
   provider-generic.
   **Settled:** one federated op, `login_federated(provider, credential)` — `login_epic`
   is gone.
2. **link/unlink policy.** Three rules the ops cannot infer: (a) unlinking the *last*
   identity — refused, or does it orphan the player; (b) linking an identity already
   bound to another player — `Conflict` (409), or an account merge (merge is its own
   feature, and post-wallet it means merging *balances* — never the default); (c) whether
   linking/unlinking emits a durable `player.identity-linked` event (audit has no sink for
   one today, and its 7 ledger topics are enumerated in `modules/audit`).
   **Settled for link, deferred for unlink:** `POST /accounts/link` ships — a foreign
   identity is (b) `Conflict` (409), no merge; no `player.identity-linked` event, since
   (c) linking now emits durable `player.promoted` instead when it's a guest's first
   non-guest identity. (a) unlink itself does not exist yet — carried to seq #2b. Audit
   is 8 ledger topics as of this rollout (`player.promoted` joined the set), not 7.
3. **Guest/device × the wallet starter grant.** Wallet (seq #1) grants currency on
   `player.registered`. Anonymous accounts-on-demand make every dial a free grant, so #2a
   must state the mitigation explicitly (throttled guest registration, or the grant firing
   only on promotion to a real provider). Neither feature has this hole alone; the
   combination creates it.
   **Settled:** the starter grant moved onto `player.promoted` (plus non-guest
   `player.registered`) — a guest is granted only once it gains a real identity, never on
   guest creation itself.
4. **Session/refresh model stops being deferrable.** The "Session + refresh-token model"
   row below is ⚠️ on its own merits, but guest/device makes the opaque 30-day session the
   *only* device identity — losing it loses the account. Either pin the decision to #2a or
   defer it deliberately with a reason; do not let it slide by omission.
   **Settled:** pinned to #2a — 60-minute access tokens plus rotating 30-day refresh-token
   families, reuse detection, a 30s grace window, and family-scoped revocation.

---

## Identity & accounts

| Feature | Status | Module(s) | Landed | Notes |
|---|:--:|---|---|---|
| Multi-provider auth (Steam/Apple/Google/Facebook/console) | ⚠️ | accounts | 2026-08-30 | Epic + Google OIDC, guest/device, dev password. One wire shape for all of them — `login_federated(provider, credential)` over a verifier registry; `login_epic` is gone. Seq #2a landed; #2b (Apple), #6 (Steam) remain — each is a new verifier, not a new op. |
| Anonymous / guest / device auth | ✅ | accounts | 2026-08-30 | `POST /accounts/guest` mints a player + a show-once device ticket; replayed through `login_federated("guest", …)`. Open decision 3 resolved by moving the wallet starter grant onto `player.promoted` — a guest is granted only when it gains a real identity. |
| Account linking / unlinking | ⚠️ | accounts | 2026-08-30 | `POST /accounts/link` ships (verified credential → identity attached to the caller's player; a foreign identity is 409, no merge). **Unlink is still missing** — seq #2b. |
| Session + refresh-token model | ✅ | accounts | 2026-08-30 | 60-minute opaque access tokens + rotating 30-day refresh-token families: reuse detection, a 30s grace window for a lost response, family-scoped revocation (never all the player's devices), and a hard family life a rotation cannot extend. Open decision 4 resolved inside #2a. |
| Self-registration (production-grade) | ⚠️ | accounts | — | `POST /accounts/register` + `/accounts/login` exist (`api/accounts/api/src/lib.rs:79,85`, argon2id) but gated behind `ACCOUNTS_DEV_AUTH` (default OFF). Missing: email verification, password reset, per-IP/per-account throttling, password policy, email-as-identity uniqueness, **and an outbound mail channel**. Seq #4, depends on #3b. |
| Account self-delete + GDPR export | ❌ | accounts | — | Only server-side prune today. |
| User metadata / profile (display name, avatar, lang) | ⚠️ | accounts | `922cc63`, 2026-09-06 | Handles landed as part of P0#4 (friends): `accounts.players` gained a `discriminator` + a unique index on `(lower(display_name), discriminator)`, and `MeView` carries the caller's own `handle` (`Name#1234`). Discovery of another player is wire-only (`accountsapi::Directory`), never front-door. Still missing: avatar, language, and any player-editable profile field. |

**Known gaps carried out of seq #2a** (deliberate, not oversights): `accounts` still parses
its provider environment inside the module rather than in `cmd/*`, so it is the one module
that reads env outside a composition root; `player.promoted`'s `from_provider` is the
constant `"guest"` because guest is the only promotable origin this release ships; the
proof fleet leaves Google deliberately unconfigured so a *known but unconfigured* provider
(503) stays distinguishable from an unknown one (400); and `unlink` does not exist — a
foreign identity is a 409 with no merge, because merging accounts post-wallet means merging
balances and is its own feature.
| Ban / moderation / trust | ❌ | accounts + admin | — | P1#12. Ban state gates session verify; admin page via extension points. |

## Social

| Feature | Status | Module(s) | Landed | Notes |
|---|:--:|---|---|---|
| Friends (add/remove/block, states) | ✅ | friends, friendsapi, friendsevents, friendsrpc, friends-svc | `922cc63`..`0b4d85a`, 2026-09-05–2026-09-06 | **P0#4**, [plan](../plans/2026-09-05-1921-friends-module-plan.md). 15th fortress: schema `friends`, one `edges` table with a canonical ordered pair (`low_id < high_id` CHECK, symmetry/pair-uniqueness live in the schema) plus `requester_id` for the asymmetric transitions; `pending`/`accepted` states, no blocking in v1. Six `#[http]` ops (request by handle/accept/decline/remove/list/pending); another player's edge is `NotFound`, never `Forbidden`. `request` resolves a target via the new wire-only `accountsapi::Directory` against a minted handle (`Name#1234`); the handle-existence oracle is **not** closed (201 vs 404), only rate-limited at the gateway. Three durable topics (`friend.requested/accepted/removed`, 30-day retention, both parties' handles denormalized) consumed by `audit` (3 new raw sinks) and `notifications` (2 `AfterRegistration` subscriptions). Read-only admin page "Friends" under Player Support + a "View Friends" row-menu entry. Proven in both topologies: `[FR1]`-`[FR9]` in split-proof (`decline` deliberately unasserted). Known gaps: **no socket presence** (`online_until` is session-derived only — gateway has no DB, presence is per-process RAM, the offline transition runs in a `Drop` that cannot `await`, no fan-in primitive); no blocking in v1; the handle-existence oracle; `audit`/`notifications` both violate `adminapi`'s never-`Err` `admin_data` contract (pre-existing, recorded not fixed); `PLAYERS_ROW_MENU` ordering is unpinned across topologies. |
| Groups / guilds / clans | ✅ | groups, groupsapi, groupsevents, groupsrpc, groups-svc | `7c7ee12`..`0d8c3b0`, 2026-09-07–2026-09-10 | 16th fortress, [plan](../plans/2026-09-06-2230-groups-membership-plan.md). Schema `groups`: a `groups` row, a `memberships` row per `(group_id, player_id)` with three states (`member`/`invited`/`requested`) and two roles (`admin`/`member`); `MAX_MEMBERS` enforced under a per-group advisory lock, not a count-then-insert. Nine `#[http]` ops (`create`/`list_mine`/`members`/`pending`/`join`/`leave`/`invite`/`respond`/`decide`) plus a wire-only `role_of` predicate (`groupsapi::Membership`, internal-edge only — not reachable through the front door) that `chat` will consume. Another player's view of a group it cannot see is `NotFound`, never `Forbidden`. Four durable topics at 30-day retention (`group.created`, `group.member_joined`, `group.member_left`, `group.role_changed`), consumed by `audit` (four raw sinks) and pruned by `groups.prune-on-scheduler.v1` (`GROUPS_RETENTION_DAYS`, default 30) — the sweep emits `group.member_left` (`reason = "expired"`) for a swept row rather than staying silent. Admin page "Groups" under Player Support adds an operator promote (member → admin). Proven in both topologies: `[GR1]`-`[GR6]` in split-proof, re-run against the monolith. Known gaps: no push nudge on membership change; no group rename or `join_policy` change after create; `chat`'s dependency on `role_of` unexercised until `chat` lands; no `group.deleted` topic, so a teardown reads as N leaves in the ledger; `memberships_player_idx` does not serve `list_mine` (that read has no equality predicate on `state`, so the index prefix can't supply the ordering); `"accept"`/`"reject"` are not exported consts in `groupsapi`, unlike every other vocabulary word in that crate; and `join` resolves the caller's handle before opening its transaction, so a join naming a nonexistent group still costs a directory call and answers 503 rather than 404 when accounts is unreachable. |
| Presence / online status / follow | ⚠️ | friends | `922cc63`..`0b4d85a`, 2026-09-06 | Session-derived presence shipped as part of P0#4: `online_until` (RFC3339, empty = no live session) computed from `accounts.sessions`, deliberately a timestamp rather than a socket-backed bool — a quit player reads a future value for up to an hour (60-minute access tokens). **Socket presence did not ship**: gateway has no DB, presence is per-process RAM with no accessor, the offline transition runs in a `Drop` that cannot `await`, and there is no fan-in primitive across gateway instances. Still needs client push for socket presence/follow. |
| Realtime chat | ❌ | — | — | P2 — gated on the realtime decision (#14). |
| Parties | ❌ | — | — | P2 — gated on the realtime decision (#14). |

## Progression & competition

| Feature | Status | Module(s) | Landed | Notes |
|---|:--:|---|---|---|
| Leaderboards (basic ranking) | ⚠️ | leaderboard | — | Single cumulative wins board, `GET /leaderboard`. |
| Leaderboard reset / seasons / rotation / buckets | ❌ | leaderboard | — | Seq #5. Scheduler already exists to drive resets; archive prior period + `leaderboard.reset` event. |
| Tournaments (start/end, attempts, rewards) | ❌ | — | — | P1#6. Builds on seasons + scheduler + wallet. |
| Statistics (versioned per-player numeric) | ❌ | — | — | P1#11. |
| Achievements / quests / missions | ❌ | — | — | P1#11, reads stats. |
| Battle pass / reward calendar / daily rewards | ❌ | — | — | Needs wallet + stats. |
| MMR / rating projection | ✅ | rating | pre-existing | `rating.ratings`, ±15 from 1000, upserted in the delivery tx; wire-only `MmrReader`. |

## Economy & commerce

| Feature | Status | Module(s) | Landed | Notes |
|---|:--:|---|---|---|
| Virtual currency wallet + ledger | ✅ | wallet, walletapi, walletevents, walletrpc, wallet-svc | `9ccd243`..`5804939`, 2026-07-30 | **Seq #1**, [plan](../plans/2026-07-28-2125-wallet-module-plan.md) (11 steps). 12th fortress: operator-owned currency catalog, per-player balances and an append-only ledger in schema `wallet`, one movement authority serving both a pool-owned and a handed delivery transaction. `walletapi::Wallet` (wire-only credit/debit, required idempotency key) + `walletapi::Player` (`GET /wallet/me`, `/wallet/currencies`). Durable `wallet.changed` → audit's 7th sink. Optional config-driven starter grant on `player.registered`, off by compiled default. Admin page under Economy & Store, remotely editable. Proven in both topologies: `[WL1]`-`[WL7]` + `[WL6m]` in split-proof. |
| Item catalog + player inventory | ⚠️ | inventory | pre-existing | Per-character holdings, static catalog; no stacks/instances model. |
| Store / storefront (listings, pricing, discounts) | ❌ | — | — | Seq #7, depends on wallet. |
| IAP receipt validation (Apple/Google/Steam) | ❌ | — | — | Seq #7. Only the simulated `INVENTORY_DEV_GRANT` route today. Needs outbound HTTP + `purchase.validated` durable event. |
| Player-to-player trading | ❌ | — | — | Low priority (only PlayFab ships it). |
| Crafting / rewards / entitlements | ❌ | — | — | Depends on wallet + inventory instances. |

## Multiplayer

| Feature | Status | Module(s) | Landed | Notes |
|---|:--:|---|---|---|
| Matchmaking (tickets/pool/queue, skill) | ❌ | — | — | P1#9. We compute MMR but never match on it. Ticket model first, not rooms. |
| Realtime multiplayer (authoritative loop) | ❌ | — | — | **P2 — needs a design conversation before any plan.** Player-QUIC plane is request/response op-dispatch, not a tick loop. |
| Relayed / client-authoritative matches | ❌ | — | — | P2, with #14. |
| State synchronization / delta encoding | ❌ | — | — | P2, with #14. |
| Lobbies | ❌ | — | — | P2, with #14/#15. |
| Dedicated game-server orchestration / fleet | ⚠️ | weles | pre-existing | Dev orchestrator (M0 + pre-M1). Standing decision: game-server management is a domain module, not generic discovery ([[server-management-is-a-domain-module]]). |

## Data, LiveOps & ops

| Feature | Status | Module(s) | Landed | Notes |
|---|:--:|---|---|---|
| In-app notifications | ✅ | notifications, notificationsapi, notificationsrpc, notifications-svc | `ecbefae`..`b991f8a`, 2026-08-31 | **Seq #3a**, [plan](../plans/2026-08-31-0032-notifications-inbox-3a-plan.md). 13th fortress, per-player inbox (schema `notifications`) fanned in from two durable subscriptions (`wallet.changed` when credited, `player.promoted`), both `AfterRegistration`. Player-facing `list` (keyset cursor in the POST body)/`mark_read`/`delete`; another player's row is `NotFound`, never `Forbidden`. Scheduled pruning via `notifications.prune-on-scheduler.v1`. Proven in both topologies: `[NT1]`-`[NT6]` + `[NT1m]`/`[NT4m]` in split-proof. Known gap: `match.finished` produces no inbox row — its `winner`/`loser` are opaque contestant strings, not `player_id`s. |
| Player mail (1:1 inbox) | ✅ | notifications | `ecbefae`..`b991f8a`, 2026-08-31 | Seq #3a, same module and plan — operator 1:1 mail through the admin "Inbox" page (new "Player Support" section), sharing the `source_event_id` dedup column with the durable fan-in via a disjoint `admin-send-mail-` prefix; a resubmit with an edited body is 409, never a silent success. |
| Outbound email channel (verification, reset) | ✅ | mail, mailevents, mailrpc, mail-svc | `d97503f`..`88e26fd`, 2026-08-31–2026-09-05 | **Seq #3b**, [plan](../plans/2026-08-31-1650-mail-outbound-channel-3b-plan.md). 14th fortress — a separate module from `notifications` (different key: a literal address, not `player_id`; different consumer: `accounts`; different failure surface: outbound SMTP with operator secrets). Durable outbox (`mail.outbox`) drained by a retry worker against a provider registry (`log` dev sink, `smtp` via `lettre`); ingress is `mail.send_requested`, which `mail` both defines and consumes (a deliberate deviation — the topic is a command, not a fact about another domain). Enqueue is exactly-once per `idempotency_key`; delivery to the recipient is at-least-once. Scheduled pruning via `mail.prune-on-scheduler.v1` (`MAIL_RETENTION_DAYS`, default 30). Admin page "Mail" under Platform, remotely editable via `admin.adminSubmit`. Known gaps: TLS negotiation (upgrade, cert/trust check, `AUTH`) is unproven by any automated test; the command topic has no in-tree producer until seq #4; `mail` stores no address, so nothing resolves a `player_id` to a recipient; a remote admin submit produces no `admin.action` row (closure deferred to a `mail.operator_sent` topic); `cancel` cannot recall a message the relay already accepted. |
| Push notifications (FCM/APNs) | ❌ | — | — | Seq #3c. Later channel on the notifications module. |
| Generic storage objects / player cloud-save (KV+OCC) | ❌ | — | — | P1#7. Self-contained new fortress. |
| Server-side custom logic / RPC hooks | ✅ | (architecture) | pre-existing | Our module registry **is** this. Scripting runtimes explicitly rejected — see gap doc "Explicit non-recommendations". |
| Remote config / feature flags | ⚠️ | config | pre-existing | Live-reload delivery half exists (revision + NOTIFY + invalidation); **no targeting**. |
| A/B experiments / segmentation / audiences | ❌ | — | — | P2#16 — the targeting brain is the real work. |
| Live events / event calendar | ❌ | — | — | P2#16. |
| Analytics / telemetry event pipeline | ⚠️ | audit + asyncevents | pre-existing | Durable transport + audit sink exist; no ingestion/query story. P1#13. |
| Title news / announcements | ❌ | — | — | Fits the notifications module later. |
| Metrics endpoint + record layer | ✅ | metrics | pre-existing | `GET /metrics`, listed in every main. |
| Scheduler (data-driven, replica-safe) | ✅ | scheduler | pre-existing | Drives seq #5 resets and any future rotation. |
| Admin portal + cross-module extension points | ✅ | admin | pre-existing | Remote fan-out + remotely-editable forms ([[admin-extension-points-shipped]]). |
| API-key policy layer | ✅ | apikeys | pre-existing | role→policy, hashed secrets, admin-editable. |

---

## Explicitly not doing

| Item | Decision |
|---|---|
| Embedded scripting runtimes (Lua/JS/Go/CloudScript) | ⛔ The module registry is the extensibility seam; a scripting runtime undercuts Open/Closed-by-new-code. |
| Colyseus-style ephemeral in-memory rooms as the persistence model | ⛔ Anti-pattern against the durable-Postgres floor. If realtime lands, state still checkpoints durably. |
| Managed-cloud / DevOps product | ⛔ Business model, not a backend feature. |

---

## Change log

- **2026-09-10** — Groups (P1#8) **landed**, `7c7ee12`..`0d8c3b0`, 22 commits,
  [plan](../plans/2026-09-06-2230-groups-membership-plan.md) revision 2. 16th fortress:
  membership, two roles, three states, and a wire-only `role_of` capability
  `chat` will consume — no channels or messages, those are `chat`'s own plan. The
  rollout's review response closed five blocking holes before Step 2 landed
  (`invite` and `decide` were unreachable as first specified — see the plan's
  *Review response*), and Step 5's retention sweep was corrected after the fact
  (`7807980`) to emit a terminal `group.member_left` for a swept invite rather than
  going silent, reversing the plan's original non-goal. A separate defect surfaced
  by this rollout but not caused by it — `rpc-contract-model`'s served-surface gates
  keyed on `api/<domain>` rather than `modules/<domain>`, so a contract crate
  landing ahead of its module could turn five gates red — was closed in the same
  window (`a5c8398`, `6c09bea`). Known gaps recorded in the tracker row above.
- **2026-09-06** — Friends (P0#4) **landed**, `922cc63`..`0b4d85a`. 15th
  fortress: `friends` (schema `friends`, canonical-ordered-pair `edges` table,
  `pending`/`accepted` states), a new wire-only `accountsapi::Directory` and
  minted player handles (`Name#1234`, a `discriminator` + unique index added
  to `accounts.players`) as the addressing scheme. Six `#[http]` ops; three
  durable topics at 30-day retention consumed by `audit` and `notifications`.
  `cargo run -p verifyctl -- --fast` 15/15 blocking PASS,
  `--all --strict` 19 PASS + 1 SKIP (`csharp-client`, no `dotnet` on this
  platform), split-proof 172 assertions / 0 failed including `[FR1]`-`[FR9]`
  in both topologies. Known gaps carried forward: no socket presence (four
  code-verified obstacles — gateway has no DB, presence is per-process RAM,
  the offline transition runs in a `Drop` that cannot `await`, no fan-in
  primitive); no blocking in v1 (a future state on the same edge, not a new
  table); the handle-existence oracle is not closed by `request` (201 vs
  404), only rate-limited at the gateway; `audit` and `notifications` both
  violate `adminapi`'s never-`Err` `admin_data` contract (pre-existing,
  recorded not fixed); `PLAYERS_ROW_MENU` ordering is unpinned across
  topologies; `decline` has no split-proof assertion, as a stated decision.
- **2026-09-05** — Outbound email channel (seq #3b) **landed**, all 13 steps,
  `d97503f`..`88e26fd`. `mail` is a new, 14th fortress rather than a channel inside
  `notifications` — the tracker's own prior answer, corrected above: the recipient
  key (a literal address, not `player_id`), the consumer (`accounts`, not the
  inbox), and the failure surface (outbound SMTP with operator secrets) each mark a
  fortress-shaped seam, not a home for a new subscription on an existing one.
  `mail.send_requested` is a **consumer-defined command topic** — `mail` both
  defines and subscribes to it, the one deliberate deviation from "publisher owns
  the event, consumer owns the subscription", because the topic names an
  imperative ("send this") rather than a fact about another domain. Provisioning
  the 14th DB-backed process also raised the cluster's usable-session floor from
  97 to 147 (`REQUIRED_MAX_CONNECTIONS` 150 minus the 3-session superuser
  reservation), preflighted against the live cluster rather than assumed. Known
  gaps carried forward rather than worked around: TLS negotiation (the STARTTLS
  upgrade, the certificate/trust check, `AUTH`) is unproven by any automated
  test — a `cfg(test)` plaintext constructor pins the full SMTP dialogue instead,
  since `webpki-roots` rules out a self-signed loopback fixture standing in for
  one; the command topic has no in-tree producer until seq #4; `mail` stores no
  address; and a remote admin submit produces no `admin.action` row, closure
  deferred to a `mail.operator_sent` topic.
- **2026-09-05** — Push hub (row 8, not from the original gap matrix — appended
  during implementation) **landed**: a `GET /push` WebSocket hub on the gateway,
  SignalR-shaped (server-minted connection id, `Target::Player|Group|All`,
  ephemeral non-authorizing groups, front-local presence default off), backed by
  a transport-free model crate (`core/push`) and a backplane fan-out over the
  existing internal mTLS edge (`core/remote`'s `PushSender`/`Pool::deliver_all`,
  wire method `push.deliver`) so a producer in any process reaches sockets a
  different front owns. `notifications` is the first producer, nudging
  `notifications.new` on every inbox insert. See
  [docs/reference/push-hub.md](../reference/push-hub.md) for the wire contract
  and the carried gaps (RPC-only C# fixture, no per-message rate limiting, no
  `routecheck` coverage of the fixed route, and a nudge that can lose a race
  with its own commit).
- **2026-08-31** — Seq #3a (notifications in-app inbox) **landed**,
  `ecbefae`..`b991f8a`. `cargo test -p notifications` 35/35, conformance
  `OK: 14 modules × 4 conventions`, and split-proof 134/134 on the real 14-process
  fleet plus the monolith parity re-run (`[NT1]`-`[NT6]`, `[NT2b]`, `[NT2c]`,
  `[NT1m]`, `[NT4m]`). The gates lied again, in the familiar shape: a proof audit
  found `cmd/gateway-svc/tests/boots.rs`'s `PEER_SLOT` assertion iterated a
  hand-written literal and had gone vacuous for the 13th module — its `PEER_SLOT`
  entry is the module's ONLY reachability path in split, since it registers as a
  zero-factory `describe_peer`, so deleting that line would have left the test
  green while every `/notifications` op was unroutable through the front door
  (fixed by deriving the provider set from `opscatalog::OPERATIONS`). Separately,
  `deliver_or_skip`'s infrastructure-failure arm had zero coverage at Step 8 time, so
  swapping it to `Ok(())` — which would silently LOSE events on an infra error
  rather than back off and pause the subscription — passed all 33 tests at the
  time; closed before landing (Step 8 closing round, `b991f8a`).
- **2026-08-31** — Seq #3 **split into 3a/3b/3c** (in-app inbox / outbound email / push);
  the single row bundled three items with unrelated risk — the inbox is durable-plane
  consumer work (a 13th fortress fanning inbox rows in from existing events), the mail
  channel is outbound I/O to the world with operator secrets and a different error model,
  and push is a third-party transport. Three contract decisions were settled before
  writing 3a's plan: the keyset cursor for the inbox list is carried in the `POST` body,
  not a query parameter, because the `#[http]` grammar has no query-parameter source and
  the hard-cap list shape is on module-reference's do-NOT-copy list; the inbox fans in
  from operator 1:1 mail plus durable `wallet.changed` and `player.promoted`; and the
  player marks an item read or deletes it, while the scheduler prunes. One finding is
  carried forward rather than worked around: `match.finished`'s `winner`/`loser` fields
  are opaque contestant strings, not `player_id`s, so match results cannot feed the inbox
  without a name→player lookup this rollout does not add — recorded as a known gap.
  Plan: [2026-08-31-0032-notifications-inbox-3a-plan.md](../plans/2026-08-31-0032-notifications-inbox-3a-plan.md).

- **2026-08-30** — Federated providers (seq #2a) **landed**, all 16 steps,
  `cargo run -p verifyctl -- --fast` 15/15 blocking stages green and split-proof 124/124
  across both topologies. `login_epic` became
  `login_federated(provider, credential)` over a verifier registry (`epic`, `google`,
  `guest`); `create_guest` + `POST /accounts/link` + durable `player.promoted` complete the
  anonymous→real lifecycle; sessions split into 60-minute access tokens and rotating 30-day
  refresh families. Three things worth carrying forward. First, **open decision 3 was real**:
  guest registration plus an unconditional starter grant is a free-currency mint, closed by
  moving the grant onto `player.promoted` and pinning the guest-skip with an executed test —
  the guard clause it depends on was reachable from the wire and unpinned by all 124 accounts
  tests until then. Second, the gates lied again in the same way they did during #1: a proof
  audit deleted **both** input-cap guards from the production `link` handler and
  `conformancecheck` still printed `OK: 13 modules × 4 conventions`, because
  `InputPolicy::Validated { basis }` is prose nothing executes — now backed by CapCases that
  drive the real op. Third, `KNOWN_PROVIDERS` was narrowed to what the build can actually
  construct a verifier for: listing a *planned* provider handed anonymous callers a permanent,
  operator-unfixable 503.

- **2026-07-28** — Tracker opened. Sequence agreed: wallet → OIDC providers/guest →
  notifications → self-registration → seasons → Steam → store/IAP. Added the
  "outbound email channel" row (missing from the source analysis) as the hard
  prerequisite for production self-registration.
- **2026-07-28** — Wallet (seq #1) planned: `docs/plans/2026-07-28-2125-wallet-module-plan.md`,
  11 steps, revision 3. Scope grew by one deliberate item during planning — an **optional,
  config-driven starter grant** on `player.registered`, off by compiled default — because a
  registered player owning no balance row makes the economy inert the moment a store exists.
- **2026-07-30** — Wallet (seq #1) **landed**, all 11 steps, `cargo run -p verifyctl -- --all
  --strict` green (the one SKIP is `csharp-client`, not applicable on this platform).
  Two things worth carrying forward. First, the rollout's review passes turned up defects in
  the **gates**, not the module: the conformance input traversal was fail-open (silently
  dropping maps, enums, newtypes, aliases and the whole `admin` domain), which is how an
  uncapped operator string could have reached SQL with every gate green — now fail-closed,
  and closing it immediately exposed a real uncapped path in `modules/apikeys` that has since
  been capped at both the Rust and the column-CHECK level. Second, five separate hand-written
  service counts went red on the 13th process (`processctl`, `weles` ×2, `weles-master`,
  `devctl`); each is now either derived from the fleet or deleted as a restatement, so the
  14th module should not repeat it.
- **2026-07-30** — Seq #2 **split into #2a** (federated-provider seam + Google + guest/device)
  **and #2b** (Apple + link/unlink); the single row bundled four items with unrelated risk.
  Four open decisions recorded above as prerequisites to writing the #2a plan — the one that
  forces the split is decision 1: whether a provider stays a method on the wire
  (`login_epic`, and then three more) or becomes an argument. That choice touches the
  public-api baseline and contract-golden, so it belongs in the first rollout of the pair,
  not the second. Decision 3 is a *new* gap created by seq #1 landing: guest auth plus the
  wallet starter grant is a free-currency faucet that neither feature has alone.
