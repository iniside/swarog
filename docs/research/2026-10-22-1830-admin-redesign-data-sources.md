# Admin redesign — per-element data sources (research, 2026-10-22)

Companion to `2026-10-22-1707-admin-redesign-gap-analysis.md`. For every element of the
in-scope redesign screens (`UILayout/redesign/`, Lore screens excluded) it records where the
value comes from. Owner rule: port 1:1; anything the backend cannot supply is a deterministic
fake marked as fake in a code comment; a real field always wins over a fake.

Legend: **REAL** (exists today) · **DERIV** (computable from existing columns, new query) ·
**FAKE** (nothing stores it) · **NEW** (needs new backend behaviour) · **UI** (portal/client only).
Method: targeted reads of the mockup assets and module sources (rust-analyzer unavailable for
`.rs` in this environment); grep hits are lower bounds. Mockup "demo states" (wallet down,
stale, denied) are demonstrations of panel states, not data: the real state wins.

## Cross-cutting

- Handles are `display_name#discriminator` (`accounts.players`); mockup "user"/`marta.k`
  handles, the "staff" kind and operators-as-accounts have no backend equivalent (operators
  live in `admin.users`, decided to stay separate).
- No time-series anywhere: every sparkline, delta and 1h/24h/7d range is FAKE; some 24 h
  aggregates are DERIV.
- Only topic `admin.action` carries actor/action/target/detail (`api/admin/events/src/lib.rs:21`).
- Deterministic-fake precedent: `modules/characters/src/admin.rs:257` (FNV of uuid),
  `modules/inventory/src/admin.rs:203` (rarity).
- Mockup vocabulary vs real: `swg_live_` vs `ak_` keys; cron vs `interval_seconds`;
  `EMB/GLD/TOK` vs seeded `gold/gems`; dotted config keys vs `namespace`+`key`; mail
  `retrying/failed` vs `pending(attempts>0)/parked`; group role `officer` vs `admin|member`.
  Real values are rendered in the mockup's layout.

## Dashboard (`pages-core.js:45-69`, `registry.js:45-62`)

| Element | Source |
| --- | --- |
| Subtitle (env, version, process count) | FAKE |
| 1h/24h/7d control | FAKE (no metric store) |
| Tile active players 24 h | DERIV `count(DISTINCT player_id)` over `accounts.refresh_tokens.issued_at`; delta/spark FAKE |
| Tiles Lore seats / Lore store | FAKE (no lore module) |
| Tile cloud saves used | DERIV `sum(octet_length(value::text))` over `storage.objects`; "64 MB quota" FAKE; spark FAKE |
| Tile mail queue | REAL `outbox_stats().pending`; "failed" = parked REAL; "relay throttled" FAKE; spark FAKE |
| Tile paused subscriptions | REAL `asyncevents.subscriptions.state` (new reader) |
| Tile embers spent 24 h | DERIV `wallet.ledger` sum of negative delta in 24 h |
| Tile matches in progress | FAKE (`match.matches` holds finished matches only) |
| Needs attention: subscription paused | REAL (asyncevents reader) |
| Needs attention: mail throttling | REAL counts; "retrying" DERIV pending∧attempts>0 |
| Needs attention: wallet-svc restarts, lore locks, `storage.gc` failure | FAKE |
| Recent operator activity | REAL `audit.log`; actor/action/target only for `admin.action`, others FAKE or derived per topic |
| Service health list + history bars | FAKE until Weles exposes its API (decided: portal reads Weles; no portal probe) |
| Top-bar health pill, notification bell | FAKE / NEW (no operator feed) |

## Users & players list (`pages-core.js:72-112`)

| Element | Source |
| --- | --- |
| User (avatar + handle) | REAL `display_name`+`discriminator`; avatar deterministic |
| Kind | FAKE |
| Status active/suspended/banned | NEW (only Online/Offline is real, from sessions) |
| Identities | REAL `accounts.identities.provider` |
| Characters count | DERIV group-by `characters.characters.player_id` (batch) |
| Lore role | FAKE |
| Embers | DERIV `wallet.balances` batch read |
| Last seen | FAKE (no column) |
| Created | REAL `players.created_at` |
| Search | REAL exact handle / id; substring + email NEW |
| Filters provider / kind / status | provider DERIV; kind FAKE; status NEW |
| Total count | REAL `store.stats()`; paging NEW (list capped at 50) |
| Export CSV, Create account, bulk actions | NEW |
| Row: grant/revoke currency | REAL (wallet submit) |
| Row: send notification | REAL (notifications submit) |
| Row: suspend, delete account, grant item, download saves | NEW |
| Row: change Lore role, release locks | FAKE |

## User detail (`pages-core.js:114-171`, `registry.js:66-91`)

| Element | Source |
| --- | --- |
| Header title, handle, id, joined | REAL |
| Status / kind badges | NEW / FAKE |
| Fact: N characters | REAL |
| Fact: Lore role | FAKE |
| Header actions: Notify REAL; Suspend NEW; Change Lore role FAKE |
| Tab counts | DERIV per module (characters, inventory, friends, groups, inbox) |
| Profile: id, handle, display name, created | REAL; kind FAKE; status NEW; country FAKE; last seen FAKE; Edit NEW |
| Personal data | FAKE (rendered as the mockup's denied state) |
| Linked identities: provider, subject, linked | REAL (`created_at` not yet selected); last used FAKE; link/unlink NEW; subject needs a masking rule |
| Active sessions: started, expires, count | REAL; client, IP FAKE; revoke NEW |
| Characters: name, class, id | REAL; level, region, last played FAKE |
| Wallet: balances, ledger, grant/revoke | REAL |
| Inventory: item, type, qty, owner | REAL (character names need a join); source, acquired FAKE; grant item NEW |
| Friends: user, state, since, counts | REAL |
| Groups: name, role, members, joined | REAL (role `admin|member`); per-player view DERIV (`page_mine`) |
| Cloud saves: slot, size, updated, version | REAL; checksum FAKE; quota FAKE; download NEW; delete disabled (matches reality) |
| Inbox: title, text, time, unread, kind | REAL; template/broadcast semantics FAKE; send REAL |

## Global search (`registry.js:37-42`)

Users: exact handle/id REAL, prefix search NEW. API keys: REAL (`name`, `prefix`, `revoked_at`).
Lore repositories: FAKE. Wallet transactions: no lookup-by-id read (NEW).

## API keys (`pages-platform.js:31-44`)

| Element | Source |
| --- | --- |
| Keys: name, prefix, role, created, status | REAL (`apikeys.keys`) |
| Keys: created-by, last used | FAKE |
| Filters / search | DERIV params |
| Row: change role, revoke (CAS) | REAL |
| Row: rotate secret | NEW |
| Revoke impact last-used / requests 24 h; reason | FAKE / NEW |
| Roles: name, policy as tags | REAL (policy = `full` or wire-method list) |
| Roles: key count | DERIV |
| Roles: create, set policy, delete | REAL |
| Create modal: name, role | REAL; expires NEW |
| Secret reveal | REAL (`SubmitOutcome.reveal`); expiry text NEW; header name to verify (`X-Api-Key`?) |

apikeys emits no audit events today.

## Mail outbox (`pages-platform.js:53-71`)

| Element | Source |
| --- | --- |
| Counts pending, parked, sent 24 h | REAL `OutboxStats` |
| Retrying / needs attention | DERIV (pending∧attempts>0; + parked) |
| Failed | maps to parked (no `failed` state) |
| Relay host, throttling banner | FAKE |
| Pause sending | NEW |
| Rows: id, recipient, kind, state, attempts, next attempt, last error | REAL (recipient is an email; avatar/handle FAKE) |
| Search, template filter, bulk select, paging | NEW (50-row cap) |
| Cancel | REAL for pending only; parked NEW |
| Resume parked, requeue all parked | REAL |
| Retry-now pending, park | NEW |
| Test send | REAL |

## Scheduler (`pages-platform.js:74-85`)

| Element | Source |
| --- | --- |
| Id, last fired | REAL (`scheduler.schedules`) |
| When | REAL as interval seconds; cron/one-shot NEW |
| Next fire | DERIV `last_fired + interval` |
| Target | FAKE (scheduler emits `scheduler.fired{name}`) |
| Last status, enabled toggle | NEW |
| Create, edit, delete, fire now | NEW (page is read-only) |
| Recent fires timeline | FAKE (or DERIV from `scheduler.fired` events) |

## Config & flags (`pages-platform.js:88-109`)

| Element | Source |
| --- | --- |
| Key, value, updated | REAL (`namespace`+`key`, `value`, `updated_at` not yet selected) |
| Type | FAKE (infer-by-parse is DERIV) |
| Per-key rev | NEW (only global revision) |
| Updated by, change note | FAKE / NEW |
| Edit with CAS | REAL (global-revision CAS), monolith only: **no `AdminSubmit` in the split** |
| Add key | REAL |
| Revisions timeline | DERIV from `audit.log` topic `config.changed`; "by" FAKE; restore NEW |

## Audit log (`pages-platform.js:112-126`)

| Element | Source |
| --- | --- |
| Time, topic | REAL (`audit.log`, latest 100) |
| Actor, action, target, detail | REAL for `admin.action`; per-topic derivation otherwise; FAKE for actors that do not exist |
| Filters topic/actor/time, search | DERIV params |
| Count, paging, export | NEW |

Topics shown in the mockup (`apikeys`, `scheduler`, `events`, `lore.*`) are not audited today.

## Economy (`pages-platform.js:164-185`)

| Element | Source |
| --- | --- |
| Currencies: code, name, kind | REAL |
| Supply, holders | DERIV aggregates |
| Cap | FAKE |
| Grant/revoke: amount, currency, reason, idempotency key | REAL |
| Player field | REAL as uuid; handle picker NEW |
| Ledger (all players) | NEW query (today per player only; needs an index on `seq`/`at`) |
| Ledger columns time, currency, delta, balance, reason, id | REAL; player handle FAKE/NEW |
| Totals, paging | NEW |

## Event plane (`pages-platform.js:130-157`)

| Element | Source |
| --- | --- |
| Subscription id, topic, contract version, state, last error, consecutive failures | REAL `asyncevents.subscriptions` |
| Lag (events behind), oldest age | DERIV (`tools/eventctl/src/lib.rs:79-122`) |
| Checkpoint / head as integers | FAKE format: real position is (generation, xid, tie) |
| Head rate events/min | FAKE |
| Consumer module, handler name | FAKE (not stored) |
| Poison event id + payload | DERIV (first event past cursor, skip predicate `lib.rs:265-275`) |
| Dead-lettered list | NEW (not stored) |
| Pause, resume, retry, skip | REAL in `tools/eventctl` (SQL in the tool, not in `core/asyncevents`) |
| Rewind | NEW |

Defect found: `eventctl resume` does not reset `consecutive_failures`/`last_error`, so a resumed
subscription re-pauses on its next failure (pause threshold 20, `core/asyncevents/src/worker.rs:88`).
