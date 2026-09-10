# Feature-gap analysis: our backend vs. Nakama / PlayFab / Pragma / Beamable / Colyseus

*Analysis date: 2026-07-22. Purpose: identify which **functionalities** commercial game
backends ship out-of-the-box that we lack, so we can decide what to copy to look
"out-of-box complete". Architecture is explicitly NOT up for copying — our modular
monolith + proven split is the differentiator; this is a pure feature inventory.*

*Sources: official docs of Nakama+Satori (heroiclabs.com), PlayFab (learn.microsoft.com),
Pragma (pragma.gg), Beamable (docs.beamable.com), Colyseus (docs.colyseus.io), cross-read
against our own `modules/*`, `api/*`, `cmd/*`. This is a decision aid, **not** an
implementation plan — no sequencing, no per-step design.*

---

## TL;DR

We are, functionally, a **competitive-match backend**: accounts (Epic + Google OIDC +
guest + dev), characters, lite inventory, match-report → rating/MMR →
single wins-leaderboard, plus a
strong **platform layer** (durable event bus, split topology, api-key policy, admin
portal with remote fan-out, config live-reload, scheduler, metrics, TLS/ACME).

Against the four full-stack platforms (Nakama, PlayFab, Pragma, Beamable) we are missing
most of the **player-facing "game services" catalog** that all four treat as table
stakes. The good news: our three seams (module registry, event bus, service registry)
plus **scheduler + config-live-reload + admin extension points already built** make a
large fraction of these *cheap* — they're new fortress modules that slot into existing
plumbing, not architectural surgery.

The gaps fall in three buckets:
- **P0 — table-stakes, cheap, plumbing already exists** (friends, notifications/mail,
  wallet/currency, leaderboard seasons/reset, more auth providers).
- **P1 — high value, medium lift** (tournaments, guilds, generic storage/cloud-save,
  matchmaking, store+IAP receipt validation, achievements/quests, moderation/ban,
  telemetry pipeline).
- **P2 — big lifts or architectural questions** (realtime multiplayer / relay / rooms,
  parties+lobbies, chat, LiveOps A/B+segmentation+feature-flags, game-server fleet).

Colyseus is the odd one out — it's *only* realtime rooms + state-sync and deliberately
has **no** economy/leaderboard/social/persistence. It's relevant only to the P2 realtime
question, and confirms realtime is a self-contained sub-system everyone treats separately.

---

## What we ALREADY have (and which of it competitors sell as headline features)

Worth stating up front so "completeness" isn't read as all-gaps — several things we have
are *marketed features* elsewhere:

| Our capability | Competitor equivalent they market |
|---|---|
| Durable event bus (XID log, pull subs, exactly-once effects) | PlayFab PlayStream, Pragma Telemetry pipeline (we have the transport, not the analytics UI) |
| Split topology (every module boots as its own svc) | Pragma "nodes/services", Beamable microservices — but ours is automatic, not hand-built |
| API-key policy layer (role→policy, hashed, admin-editable) | PlayFab title/secret keys, Supabase-style anon/service keys |
| Admin portal + cross-module extension points + remote fan-out | Nakama Console, PlayFab Game Manager, Pragma/Beamable Portal (ours is thinner on domain pages) |
| Config live-reload (DB knobs, monotonic revision, NOTIFY) | Nakama/Satori feature-flags & remote config (we have the delivery half, not targeting) |
| Scheduler (data-driven, replica-safe exactly-once) | Nakama leaderboard/tournament reset schedules, PlayFab Scheduled Tasks |
| Metrics endpoint + record layer | Nakama Prometheus metrics, Pragma monitoring |
| accounts: federated providers (Epic/Google OIDC, guest device), Epic web OAuth, account linking, 60-min access tokens + rotating 30-day refresh families | one of many providers each platform ships |

**Key leverage insight:** scheduler, event bus, config-live-reload, and admin extension
points are exactly the primitives the missing features need. That's why the P0/P1 lists
below are mostly "new module wired to existing plumbing", not new plumbing.

---

## The gap matrix

Legend: ✅ has it · ⚠️ partial · ❌ absent · — out of scope for that product.
"Us" = this repo today.

### Identity & accounts
| Feature | Nakama | PlayFab | Pragma | Beamable | Us |
|---|:--:|:--:|:--:|:--:|:--:|
| Multi-provider auth (Steam/Apple/Google/Facebook/console) | ✅ | ✅ | ✅ | ✅ | ⚠️ Epic + Google OIDC only |
| Anonymous / guest / device auth | ✅ | ✅ | ⚠️ | ✅ | ✅ `create_guest` + device ticket |
| Account linking / unlinking (cross-platform one account) | ✅ | ✅ | ✅ | ✅ | ⚠️ `POST /accounts/link` ships; no unlink op |
| Session + refresh-token model | ✅ | ✅ | ✅ | ✅ | ✅ 60-min access + rotating 30-day refresh families, reuse detection |
| Account self-delete + GDPR export | ✅ | ✅ | ✅ | — | ❌ (only server-side prune) |
| User metadata / profile fields (display name, avatar, lang) | ✅ | ✅ | ✅ | ✅ | ❌ (only player_id + identities) |
| Ban / moderation / trust | ✅ | ✅ | ✅ | — | ❌ |

### Social
| Feature | Nakama | PlayFab | Pragma | Beamable | Us |
|---|:--:|:--:|:--:|:--:|:--:|
| Friends (add/remove/block, states, social-graph import) | ✅ | ✅ | ✅ | ✅ | ❌ |
| Groups / guilds / clans (roles, join requests) | ✅ | ✅ | — | ✅ | ❌ |
| Presence / online status / follow | ✅ | ⚠️ | ✅ | — | ❌ |
| Realtime chat (room/group/DM, history) | ✅ | — | — | ✅ | ❌ |
| Parties (invite, matchmake as group) | ✅ | — | ✅ | ✅ | ❌ |

### Progression & competition
| Feature | Nakama | PlayFab | Pragma | Beamable | Us |
|---|:--:|:--:|:--:|:--:|:--:|
| Leaderboards (basic ranking) | ✅ | ✅ | — | ✅ | ⚠️ single cumulative wins board |
| Leaderboard reset / seasons / rotation / buckets | ✅ | ✅ | — | ✅ | ❌ |
| Tournaments (start/end, attempts, rewards) | ✅ | ✅ | ⚠️ | ✅ | ❌ |
| Statistics (versioned per-player numeric stats) | — | ✅ | ✅ | ✅ | ❌ |
| Achievements / quests / missions | — | ⚠️ | ✅ | ✅ | ❌ |
| Battle pass / reward calendar / daily rewards | — | — | ✅ | ✅ | ❌ |

### Economy & commerce
| Feature | Nakama | PlayFab | Pragma | Beamable | Us |
|---|:--:|:--:|:--:|:--:|:--:|
| Virtual currency / wallet + ledger | ✅ | ✅ | ✅ | ✅ | ❌ (static item table only) |
| Item catalog + player inventory (stacks, instances) | ⚠️ | ✅ | ✅ | ✅ | ⚠️ per-char holdings, static catalog |
| Store / storefront (listings, pricing, discounts) | — | ✅ | ✅ | ✅ | ❌ |
| IAP receipt validation (Apple/Google/Steam) | ✅ | ✅ | ✅ | ✅ | ❌ (simulated dev grant only) |
| Player-to-player trading | — | ✅ | — | — | ❌ |
| Crafting / rewards / entitlements | — | ⚠️ | ✅ | ✅ | ❌ |

### Multiplayer
| Feature | Nakama | PlayFab | Pragma | Beamable | Colyseus | Us |
|---|:--:|:--:|:--:|:--:|:--:|:--:|
| Matchmaking (tickets/pool/queue, skill) | ✅ | ✅ | ✅ | ✅ | ⚠️ room-filter | ❌ (only MMR projection) |
| Realtime multiplayer (authoritative loop) | ✅ | — | — | ⚠️ relay | ✅ | ❌ |
| Relayed / client-authoritative matches | ✅ | — | — | ✅ | ✅ | ❌ |
| State synchronization / delta encoding | — | — | — | — | ✅ | ❌ |
| Lobbies | — | — | ⚠️ | ✅ | ⚠️ | ❌ |
| Dedicated game-server orchestration / fleet | — | ✅ | ✅ | — | — | ⚠️ Weles (dev orchestrator) |

### Data, LiveOps & ops
| Feature | Nakama+Satori | PlayFab | Pragma | Beamable | Us |
|---|:--:|:--:|:--:|:--:|:--:|
| Generic storage objects / player cloud-save (KV+OCC) | ✅ | ✅ | ✅ | ✅ | ❌ (per-module schemas only) |
| In-app notifications | ✅ | ✅ | ✅ | ✅ | ❌ |
| Player mail (1:1 inbox) | — | — | — | ✅ | ❌ |
| Push notifications (FCM/APNs) | ⚠️ Satori | ✅ | — | ⚠️ | ❌ |
| Server-side custom logic / RPC hooks | ✅ Go/JS/Lua | ✅ CloudScript | ✅ services | ✅ C# svc | ✅ **our modules ARE this** |
| Remote config / feature flags | ✅ Satori | ✅ | ✅ | ⚠️ | ⚠️ config live-reload (no targeting) |
| A/B experiments / segmentation / audiences | ✅ Satori | ✅ | — | ✅ | ❌ |
| Live events / event calendar | ✅ Satori | — | ✅ | ✅ | ❌ |
| Analytics / telemetry event pipeline | ✅ Satori | ✅ PlayStream | ✅ | ✅ | ⚠️ event bus + audit, no analytics |
| Title news / announcements / content CDN | — | ✅ | — | ✅ | ❌ |

---

## Prioritized "copy these" list

Each item: **what it is · who has it · why it matters · architectural fit (which seam)
· rough size**. Fit notes are one-liners, not designs.

### P0 — table stakes, cheap, plumbing already exists

1. **Leaderboard seasons / reset / rotation.**
   Nakama, PlayFab, Beamable all have it; we have a single never-resetting board.
   *Why:* the single most glaring "toy vs. real" tell in our current surface.
   *Fit:* extend the existing `leaderboard` module; **scheduler already exists** to drive
   cron resets + `emit_tx` a `leaderboard.reset` event; archive prior period to a history
   table. No new plumbing.
   *Size:* small.

2. **Notifications + player mail (in-app, durable).**
   Nakama/PlayFab/Beamable core feature.
   *Why:* prerequisite for rewards, social invites, LiveOps, moderation messages —
   unlocks half the other features.
   *Fit:* new `notifications` fortress; **durable event bus is the perfect fan-in** —
   other modules `emit_tx` and the module persists per-player inbox rows; player-facing
   `GET /notifications` + delete. Push (FCM/APNs) is a later add-on channel.
   *Size:* small–medium.

3. **Virtual currency wallet + ledger.**
   All four platforms.
   *Why:* foundation for store, rewards, battle pass, IAP — economy has no floor without
   it. Our "coin" item is a fake stand-in.
   *Fit:* new `wallet` fortress; per-player balances + auditable ledger in its own schema,
   mutated only in a delivery/registry tx; `WalletWriter` sync capability for other
   modules; `wallet.changed` durable event. Textbook fit for our transactional model.
   *Size:* small–medium.

4. **Friends (add/remove/block, states, list).**
   All four.
   *Why:* the anchor of "social" — nearly every other social feature hangs off it.
   *Fit:* new `friends` fortress; own schema (relationship rows), sync `Friends`
   capability, `friend.added`/`friend.removed` durable events; notifications module
   consumes for invite alerts. No realtime needed for the async part.
   *Size:* medium.

5. **More auth providers + guest/anonymous.**
   All four ship many; we ship Epic + Google (OIDC) + guest + dev — Google/guest landed
   seq #2a, over one `login_federated(provider, credential)` op and a verifier registry.
   *Why:* Steam/Apple are still missing; unlink (only link shipped) is the remaining gap.
   *Fit:* extend `accounts` — Apple (OIDC + signed-JWT client secret) is incremental over
   the existing `OidcVerifier`; Steam needs its own ticket verifier. Add the unlink op
   while here.
   *Size:* small (Apple) to medium (Steam).

### P1 — high value, medium lift

6. **Tournaments** (start/end/duration, join, max attempts, rewards).
   Nakama/PlayFab/Beamable. *Fit:* builds directly on P0#1 (seasons) + scheduler +
   P0#3 (wallet for rewards). *Size:* medium.

7. **Generic storage objects / player cloud-save** (collection/key, per-user + public,
   read/write permission levels, OCC via version).
   Nakama + PlayFab core. *Why:* the universal "let the game store arbitrary JSON" escape
   hatch — studios expect it. *Fit:* new `storage` fortress, its own schema, a permission
   + version(OCC) model; player-facing CRUD ops. Self-contained. *Size:* medium.

8. **Guilds / groups** (roles: owner/admin/member, join requests, group metadata).
   Nakama/PlayFab/Beamable. *Fit:* new `groups` fortress + durable membership events;
   depends on friends/notifications for invites. *Size:* medium–large.

9. **Matchmaking (ticket/pool/queue with skill).**
   All four. *Why:* we already compute MMR (`rating`) but never match on it — closing this
   loop is a natural story. *Fit:* new `matchmaking` fortress; a pool + a periodic matcher
   (scheduler-like tick) that reads `rating` MMR and `emit_tx`s a `match.proposed` event.
   Non-realtime ticket model first (à la PlayFab/Nakama), not room-based. *Size:* medium.

10. **Store + IAP receipt validation** (Apple/Google/Steam).
    All four. *Why:* the monetization floor; without receipt validation the economy is
    demo-only. *Fit:* extend inventory/wallet with a `catalog`+`store`; a `commerce` module
    doing server-side receipt verification (outbound HTTP to Apple/Google), binding the
    purchase to the player, `purchase.validated` durable event. *Size:* medium–large.

11. **Statistics + achievements / quests.**
    PlayFab/Pragma/Beamable. *Fit:* a `stats` fortress (versioned numeric per-player) that
    achievements/quests read; durable events drive unlock notifications. *Size:* medium.

12. **Player ban / moderation / trust.**
    Nakama/PlayFab/Pragma. *Fit:* extend `accounts` (ban state gating session verify) +
    an admin page (we have extension points); `admin.action` audit already exists.
    *Size:* small–medium.

13. **Telemetry / analytics event pipeline.**
    All four. *Why:* we have the durable-event transport and `audit` sink but no analytics
    ingestion/query story. *Fit:* a `telemetry` sink module consuming a raw event topic
    into a big-data-friendly table; the bus already does the hard delivery part. *Size:*
    medium (ingestion) — the reporting UI is the expensive part, can defer.

### P2 — big lifts / architectural questions (decide separately)

14. **Realtime multiplayer** (authoritative match loop / relay / rooms + state-sync).
    Nakama, Colyseus, Beamable. *Why it's P2:* our player-QUIC plane is **request/response
    op-dispatch**, not a tick-based session/relay. This is a genuine new sub-system (a
    stateful match plane with a loop, presence, and push), not a module wired to existing
    plumbing. Colyseus exists purely for this and treats it as its whole product —
    evidence it deserves its own decision. **Needs a design conversation before any plan.**

15. **Parties + lobbies + presence + chat.**
    Nakama/Beamable/Pragma. *Why it's P2:* all four need realtime **push to the client**
    (presence changes, chat messages, party updates). Our invalidation plane is
    server-to-server LISTEN/NOTIFY, not client fan-out. Gated on the #14 realtime decision.

16. **LiveOps suite: A/B experiments, segmentation/audiences, feature-flag targeting,
    live-event calendar.**
    Satori/PlayFab/Beamable. *Why it's P2:* large product surface (targeting engine +
    experiment allocation + analytics scorecards). We have the *delivery* half
    (config live-reload = remote config without targeting); the targeting/experiment brain
    is the real work. Consider a thin feature-flag-targeting slice first if prioritized.

17. **Dedicated game-server orchestration as a product feature** (fleet, allocation,
    standby scaling).
    PlayFab/Pragma. *Why it's P2:* we have **Weles** (dev orchestrator) and a standing
    decision that "game-server management is a domain module, not generic discovery". This
    is a roadmap item with an owner already conceptualized — not a copy-Nakama exercise.

---

## Explicit non-recommendations (don't copy)

- **Multi-language embedded scripting runtimes (Go/Lua/JS/CloudScript).** Nakama/PlayFab
  offer these because their core is closed and users can't add compiled modules. **Our
  module registry IS the extensibility seam** — adding a scripting runtime would undercut
  the whole Open/Closed-by-new-code premise. This is a case where our architecture is
  strictly better; skip it.
- **Colyseus-style ephemeral in-memory rooms as the persistence model.** Colyseus
  explicitly persists nothing; that's an anti-pattern for us given our durable-Postgres
  floor. If we do realtime (#14), state still checkpoints durably.
- **A separate managed-cloud/DevOps product (Colyseus Cloud, Beamable Cloud, Pragma
  Managed Services).** Out of scope — that's a business model, not a backend feature.

---

## Suggested reading order for a decision

If the goal is "look out-of-box complete with least effort", the highest
completeness-per-unit-effort ordering is roughly the P0 list top-to-bottom
(seasons → notifications → wallet → friends → auth providers), because each is a module
that plugs into scheduler / event-bus / registry that **already exist**, and together they
flip the surface from "match-report demo" to "recognizable game platform". P1 then adds
the monetization and depth story. P2 needs its own architecture conversation before any
plan — do not fold it into the same decision.

---

## Errata (post-freeze)

This analysis is frozen at its 2026-07-22 date (`docs/roadmap/feature-tracker.md` is the
doc that moves); the row and recommendation below are recorded here rather than edited in
place.

- **Line 87** — "Groups / guilds / clans (roles, join requests) | … | ❌" is superseded:
  `groups` landed (`7c7ee12`..`0d8c3b0`, 2026-09-10), the 16th fortress. See
  `docs/roadmap/feature-tracker.md`'s Social table for current status and gaps.
- **Line 200-201** — "new `groups` fortress … depends on friends/notifications for
  invites" is superseded on the dependency clause: the landed module depends on
  `accounts` (to resolve an invite target's handle), not `notifications` — no
  notification is sent for a group invite, membership change, or expiry in this
  rollout.
