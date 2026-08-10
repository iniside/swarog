# Feature tracker — closing the gaps from the BaaS analysis

**Last update: 2026-07-30-2215**

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
| 2a | Federated-provider seam + Google OIDC + guest/device | ❌ | — |
| 2b | Apple OIDC + identity link/unlink | ❌ | — |
| 3 | Notifications + player mail (in-app, durable) | ❌ | — |
| 4 | Self-registration promoted to production (email verify, password reset) | ❌ | — |
| 5 | Leaderboard seasons / reset / rotation | ❌ | — |
| 6 | Steam auth (ticket verifier) | ❌ | — |
| 7 | Store + IAP receipt validation | ❌ | — |

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

### Open decisions — settle these BEFORE the #2a plan is written

These are the questions a plan for #2 cannot leave to implementation. Recorded here so
the sequence row stays a one-liner.

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
2. **link/unlink policy.** Three rules the ops cannot infer: (a) unlinking the *last*
   identity — refused, or does it orphan the player; (b) linking an identity already
   bound to another player — `Conflict` (409), or an account merge (merge is its own
   feature, and post-wallet it means merging *balances* — never the default); (c) whether
   linking/unlinking emits a durable `player.identity-linked` event (audit has no sink for
   one today, and its 7 ledger topics are enumerated in `modules/audit`).
3. **Guest/device × the wallet starter grant.** Wallet (seq #1) grants currency on
   `player.registered`. Anonymous accounts-on-demand make every dial a free grant, so #2a
   must state the mitigation explicitly (throttled guest registration, or the grant firing
   only on promotion to a real provider). Neither feature has this hole alone; the
   combination creates it.
4. **Session/refresh model stops being deferrable.** The "Session + refresh-token model"
   row below is ⚠️ on its own merits, but guest/device makes the opaque 30-day session the
   *only* device identity — losing it loses the account. Either pin the decision to #2a or
   defer it deliberately with a reason; do not let it slide by omission.

---

## Identity & accounts

| Feature | Status | Module(s) | Landed | Notes |
|---|:--:|---|---|---|
| Multi-provider auth (Steam/Apple/Google/Facebook/console) | ⚠️ | accounts | — | Epic OIDC + Epic web OAuth + dev password only. Seq #2a (seam + Google), #2b (Apple), #6 (Steam). The wire shape is per-provider today (`login_epic`) — open decision 1. |
| Anonymous / guest / device auth | ❌ | accounts | — | Seq #2a. New provider row, no verifier needed — but see open decision 3 (interaction with the wallet starter grant). |
| Account linking / unlinking | ⚠️ | accounts | — | `(provider, subject) → player_id` model already supports it and `Store::link_identity` exists; the link/unlink **ops** and the policy are missing. Seq #2b, open decision 2. |
| Session + refresh-token model | ⚠️ | accounts | — | Opaque 30-day DB sessions, no refresh token. Guest/device (#2a) makes this the sole device identity — open decision 4. |
| Self-registration (production-grade) | ⚠️ | accounts | — | `POST /accounts/register` + `/accounts/login` exist (`api/accounts/api/src/lib.rs:79,85`, argon2id) but gated behind `ACCOUNTS_DEV_AUTH` (default OFF). Missing: email verification, password reset, per-IP/per-account throttling, password policy, email-as-identity uniqueness, **and an outbound mail channel**. Seq #4, depends on #3. |
| Account self-delete + GDPR export | ❌ | accounts | — | Only server-side prune today. |
| User metadata / profile (display name, avatar, lang) | ❌ | — | — | Only `player_id` + identities. |
| Ban / moderation / trust | ❌ | accounts + admin | — | P1#12. Ban state gates session verify; admin page via extension points. |

## Social

| Feature | Status | Module(s) | Landed | Notes |
|---|:--:|---|---|---|
| Friends (add/remove/block, states) | ❌ | — | — | P0#4. New fortress, sync `Friends` capability + durable events. |
| Groups / guilds / clans | ❌ | — | — | P1#8. Depends on friends + notifications. |
| Presence / online status / follow | ❌ | — | — | P2 — needs client push. |
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
| In-app notifications | ❌ | — | — | **Seq #3.** New fortress, per-player inbox rows fanned in from durable events; player-facing list + delete. |
| Player mail (1:1 inbox) | ❌ | — | — | Seq #3, same module. |
| Outbound email channel (verification, reset) | ❌ | — | — | **Not in the source gap doc — added 2026-07-28.** Hard prerequisite for seq #4; owned by the notifications module as an outbound channel. |
| Push notifications (FCM/APNs) | ❌ | — | — | Later channel on the notifications module. |
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
