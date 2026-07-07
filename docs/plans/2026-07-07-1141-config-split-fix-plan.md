# Fix: make the config module work in the microservices split

**Date:** 2026-07-07 11:41
**Branch:** master (per work-on-master preference)
**Status:** plan, pre-review

## The screwup this fixes

The config module (merged in `e212cd3`) was made **monolith-only** by design
decision #3 of the original plan: "config is hosted only in `cmd/server`; live reload
is a monolith-only demo." That guts the feature's entire point — "central config,
edit without redeploy" — in the **split** deployment (a first-class topology here:
`cmd/characters-svc` + `cmd/inventory-svc` + `cmd/gateway-svc`). In the split,
`inventory-svc` hosts the config *consumer* (inventory) but not config, so it
silently falls back to constants and cannot be configured at all without a rebuild.

Two compounding mistakes:
1. **Wrong dependency shape.** I used a SOFT `registry.TryRequire` + monolith-only
   hosting, chosen because it *passed the arch-lint / split-build gates*, not because
   it was correct. The soft-degrade conflated two different "no value" cases:
   *(a) this key has no override set* (legitimate → use the code default) and
   *(b) the config service isn't deployed* (a broken deployment → should fail loud).
2. **Overclaimed the write path.** The design said "a write (admin OR raw psql)
   fires `pg_notify`," but the implementation only fires `pg_notify` from
   `service.Set` (app code). A raw `psql UPDATE config.settings` does NOT notify — so
   "edit via psql without redeploy" never worked either.

## The fix, in one line

**Config is a foundation, not a monolith module.** Host it in every binary that
hosts a config consumer, hard-require it (fail loud if absent), and drive
`config.changed` from a **DB trigger** so *any* writer on the shared Postgres — any
service's `Set`, or raw psql — propagates to every listener. This is exactly what the
shared-DB `LISTEN/NOTIFY` design was for: Postgres delivers a `NOTIFY` to every
session that has `LISTEN`ed on the channel in that database, across processes, for
free.

## Context — why this shape (and why not the alternatives)

Research: 3 narrow subagents (split lifecycle wiring / arch-lint deltas / hard-require
change surface), each with nav guidance, synthesized here. All three **confirmed**
(not refuted) the facts below.

| Alternative | Why not |
|---|---|
| **Keep soft `TryRequire`, host config in inventory-svc** | Makes the feature work, but keeps the silent-degrade footgun: a future binary that hosts inventory but forgets config boots fine and ignores config. User chose hard-require (fail-loud). |
| **config as a remote service over the QUIC edge** (like accounts/characters) | The edge is sync request/response, not pub/sub — `config.changed` push would need the outbox→HTTP-sink fanout, a lot of machinery. Pointless when the DB is already shared: `LISTEN/NOTIFY` IS the cross-process pub/sub, free. |
| **Host config only in cmd/server (status quo)** | The bug. Feature dead in split. |
| **App-code `pg_notify` from `Set` only (status quo)** | In-process reload works, but raw psql / any-external write does not propagate. A DB trigger makes NOTIFY fire on *any* write, delivering the real "edit anywhere" promise and enabling an honest cross-process proof. |

**Confirmed facts (file:line):**
- `lifecycle/app.go` `Build`/`Migrate`/`Start`/`Stop` (44-93) are generic type-assertion
  loops over the module list — dropping `&config.Module{}` into a binary's `mods`
  slice runs its Register/Init/Migrate/Start with no other lifecycle change.
- `internal/app/app.go:44` reads `DATABASE_URL` (same default DSN literal as
  `modules/config/config.go:24`); `run.ps1` `$envB:204` already passes `DATABASE_URL`
  to inventory-svc, so config's listener DSN resolves there.
- `internal/app/app.go:193-206` `validateRequires` returns an error (before Build) if
  any module's `Requires()` names something not hosted → the fail-loud guarantee.
- `.go-arch-lint.yml`: only change needed is `- config` under `cmdInventorySvc.mayDependOn`
  (161-168); config's own `deps` `[lifecycle, contracts]` (109) are all satisfiable in
  inventory-svc, no new forbidden edge, no isolation leak (config's impl can't import
  accounts/characters).
- Hard-require surface in `modules/inventory/inventory.go`: `Requires()` (82),
  `TryRequire` call (148-154), `loadStarterLocked` nil-branch (190-199), comments
  (26-27, 61-63, 143-147). One breaking test: `TestInventoryReactsToCharacterLifecycle`
  (inventory_test.go:91) builds `&Module{}` without cfg → inject the existing
  `fakeConfig{}` helper. `TryRequire` stays (exported, tested, lint-safe).

**Scope decision — config in inventory-svc only, NOT characters-svc.** characters-svc
hosts accounts+characters, neither of which consumes config today (YAGNI). Rule for
the future: *any binary hosting a config consumer must host config* — enforced
automatically by hard-require + `validateRequires`. If accounts/characters later read
config, characters-svc gets the same one-line treatment then.

**Side effect (desirable):** hosting config in inventory-svc makes config's admin item
("Game Config & Flags") appear at inventory-svc's `/admin` (:8081) — that's the split's
config editor. Good, not a bug.

---

## Steps (ordered so every commit keeps both topologies green)

### Step 1 — host config in `cmd/inventory-svc`  `[sonnet]`
- **(a) What:** `cmd/inventory-svc/main.go` — add import `"gamebackend/modules/config"`
  and `&config.Module{},` as the first entry of the `mods` slice (60-65).
  `.go-arch-lint.yml` — add `- config` to `cmdInventorySvc.mayDependOn` (161-168).
- **(b) Why first:** it must land BEFORE the hard-require (Step 2), otherwise the
  moment inventory `Requires("config")` the inventory-svc binary fails `validateRequires`
  at boot. With config present first, the split works under BOTH the current soft
  require and the coming hard require — no red commit in between.
- **(c) How:** config is a foundation (`Requires() == nil`), so slice position only
  needs to precede consumers by convention; `validateRequires`/two-phase Register make
  order non-load-bearing. No `run.ps1` change (`$envB` already passes `DATABASE_URL`).
- **Verify:** `go build ./...`, `go-arch-lint check` green; `run.ps1 -Mode microservices`
  still boots all three (inventory-svc now logs `module ready module=config`).
- **Commit:** `fix(inventory-svc): host config module in the split binary`

### Step 2 — hard-require inventory → config  `[sonnet]`
- **(a) What:** `modules/inventory/inventory.go` + one line of `inventory_test.go`
  + a new `internal/app/app_test.go` (or extend an existing one) asserting fail-loud.
- **(b) Why now:** config is now hosted wherever inventory runs (Step 1), so the
  dependency can be made explicit and fail-loud instead of silently degrading.
- **(c) How — exact edits (from research):**
  - `Requires()` (82): append `"config"` → `[]string{"accounts","characters","config"}`.
  - Replace the soft block (148-154) `if cfg, ok := TryRequire[...]; ok { m.cfg = cfg; bus.On(...) }`
    with unconditional `m.cfg = registry.Require[configReader](ctx.Registry, "config")`
    then `bus.On(ctx.Bus, configevents.ChangedEvent, m.onConfigChanged)`.
  - `loadStarterLocked` (190-199): drop the `if m.cfg != nil / else consts` branch —
    always `m.cfg.GetString("inventory","starter_item", starterItem)` /
    `GetInt("inventory","starter_qty", starterQty)`. The `starterItem`/`starterQty`
    consts remain as the **default-value args for absent keys** (case (a)), NOT a
    no-config fallback (case (b) is now impossible — fail-loud).
  - Update stale comments (26-27, 61-63, 143-147) to say config is mandatory; the
    consts are per-key defaults.
  - `inventory_test.go:91`: `m := &Module{store: s, log: ..., cfg: &fakeConfig{}}` — an
    empty `fakeConfig{}` returns the passed defaults, so the `qtyOf==1` assertion (102)
    holds unchanged. (The other two tests already pass cfg or no Module.)
  - `registry.TryRequire`: leave it (exported, tested by `TestTryRequire`, lint-safe).
  - **Fail-loud as a COMMITTED test (not a manual check):** `validateRequires`
    (`internal/app/app.go:193-206`) is pure — add a unit test that builds a module set
    where one module's `Requires()` names an absent provider and asserts the returned
    error mentions the missing dep. This locks the fail-loud guarantee that justifies
    the hard-require redesign, instead of eyeballing it once.
- **Verify:** `go test ./modules/inventory/... ./registry/... ./internal/app/...` green.
- **Commit:** `fix(inventory): hard-require config (fail-loud), drop silent no-config fallback`

### Step 3 — NOTIFY from a DB trigger (edit-anywhere, cross-process)  `[opus]`
- **(a) What:** `modules/config/store.go` (schema DDL) + `modules/config/service.go`
  (`Set`) + `modules/config/config_test.go`.
- **(b) Why now:** delivers the real "edit anywhere without redeploy" promise (raw psql
  + any service's write, not just in-process `Set`), and enables an HONEST cross-process
  proof in Step 4. Independent of Steps 1-2 but lands before verification.
- **(c) How — the non-mechanical parts:**
  - **Schema DDL** gains an `AFTER INSERT OR UPDATE` trigger on `config.settings`
    (NOT DELETE — see below) calling a `config.notify_changed()` plpgsql function:
    `PERFORM pg_notify('config_changed', NEW.namespace || ':' || NEW.key); RETURN NULL;`.
    Use `CREATE OR REPLACE FUNCTION` + `CREATE OR REPLACE TRIGGER` (Postgres 14+; this
    repo runs PG18) so re-running `Migrate` is idempotent WITHOUT a drop/create window.
    `RETURN NULL` is correct for an `AFTER` trigger. Payload matches the listener's
    `split on first ':'` (ids are `^[a-z0-9_]+$`, so `:` is unambiguous).
  - **DELETE is deliberately NOT triggered.** The listener has no delete path —
    `listen.go` does `getOne`, and on `!found` it `continue`s (never removes from cache
    or emits), and `service.setCacheOne` can only set. The admin editor only
    adds/updates. So live-deleting a key is out of scope; the trigger stays
    INSERT/UPDATE. (Handling deletes = a separate follow-up: cache-delete +
    `config.changed` with a tombstone. Noted, not built.)
  - **`service.Set`** drops its explicit `SELECT pg_notify(...)`; the trigger fires
    NOTIFY on the same statement, delivered on commit. **Decision (not "optional"):**
    `Set` becomes a single autocommit `INSERT … ON CONFLICT … DO UPDATE` — the explicit
    `BeginTx`/`Commit` (service.go:86-98) is removed, since one statement + a trigger
    NOTIFY is already atomic. No regression: the monolith reload path (already verified
    live) still fires, just sourced from the trigger not from `Set`.
  - **Push self-heal on reconnect (fixes the materialized-consumer staleness).**
    Today `listenOnce` reloads the pull cache on every (re)connect but emits nothing,
    so a materialized consumer (inventory's starter spec) stays stale for any key that
    changed during a disconnect. Change `replaceCache` to return the set of
    `(namespace,key,value)` whose value differs from the prior snapshot; after a
    **re**connect reload (NOT the initial boot load — that would spam every key on
    startup), the listener emits `config.changed` for each differing key so push
    consumers rebuild. Initial-vs-reconnect tracked by a bool on the listener.
  - **Tests:** keep the Set→poll notify test (now trigger-sourced). ADD (1) a
    **raw-write** test — `db.ExecContext("UPDATE config.settings SET value=… WHERE …")`
    bypassing `Set`, then poll `Get` — proving the trigger propagates writes that never
    touched app code (the psql path); (2) a **reconnect self-heal** test — seed a key,
    load cache, change the value directly, then invoke the reconnect reload path and
    assert a `config.changed` was emitted (subscribe a test handler) AND the cache
    updated. Unique per-test namespace + `t.Cleanup`.
- **Verify:** `go test ./modules/config/...` green (incl. both new tests).
- **Commit:** `feat(config): NOTIFY config.changed from a DB trigger (any writer, incl. psql) + reconnect self-heal`

### Step 4 — verify in the split, as a COMMITTED artifact  `[inline]`
- **(a) What:** `./verify.ps1 --all`, PLUS a new committed smoke script
  `scripts/smoke-split-config.sh` (bash; the repo's tooling is cross-shell) that boots
  the split, drives the flow, asserts, and **exits non-zero on failure** — so the split
  proof is repeatable and reviewable, not a one-time eyeball. Capture its run output to
  `docs/2026-07-07-HHMM-config-split-verified.md` and commit that too.
- **(b) Why last / why committed:** the first time, I "verified" by running the monolith
  (never at risk) and passed it off as split coverage. A manual, uncommitted,
  eyeball-graded drive is the same failure mode. A committed script that fails loudly is
  the antidote.
- **(c) The smoke script's asserted sequence (each an explicit pass/fail):**
  1. `run.ps1 -Mode microservices` boots all three; grep inventory-svc log for
     `module ready module=config` — **asserts fail-loud is satisfied** (config present)
     and, by booting at all, that hard-require didn't break the binary.
  2. **In-process reload in the split:** POST `inventory:starter_item=health_potion` to
     inventory-svc's own editor (`:8081/admin`). Register on characters-svc (`:8080`),
     then **loop: create a FRESH character and check its grant, retrying with a
     deadline** until the grant is `health_potion`. Creating one character then polling
     the same one is WRONG — the starter is granted once at `character.created`; a
     character created before propagation is frozen to the old item. Retry-fresh-create
     mirrors `TestInventoryStarterLiveReloadFromConfig` (inventory_test.go:181-203).
     Fail if it never flips within the deadline.
  3. **Any-writer reload (needs Step 3's trigger):** `psql UPDATE config.settings SET
     value='coin' WHERE namespace='inventory' AND key='starter_item'` — a write that
     never touched app code — then retry-fresh-create until the grant flips to `coin`.
     Proves a write from a **separate connection** propagates via the trigger's NOTIFY
     to inventory-svc's listener. (Honest scope: this shows cross-connection propagation
     to the split's ONE config listener; it is NOT a multi-listener broadcast demo —
     see the separate broadcast check below.)
  4. Teardown (`run.ps1 -Teardown`), `DELETE FROM config.settings WHERE namespace='inventory'`.
- **(d) Separate, honest broadcast proof (manual, documented in the captured md):** in a
  `psql` session run `LISTEN config_changed;` then, from another connection, an app
  `Set` (or a raw UPDATE) — the psql session receives an async `Asynchronous notification
  "config_changed"` payload. This directly demonstrates Postgres delivering the NOTIFY to
  a DIFFERENT session/connection than the writer — the cross-process broadcast property
  the whole design rests on, shown rather than asserted.
- **Commit:** `test(config): committed split smoke + captured verification evidence`
  (the script + the evidence md). Report exactly what was observed, incl. anything that
  did NOT behave as expected.

---

## Risks / watch-items
- **Two DB connections per config-hosting process** (ctx.DB pool + the listener's raw
  pgx conn) — unchanged from today, just now also in inventory-svc. Fine.
- **Trigger vs app-notify double-fire:** Step 3 REMOVES the explicit `pg_notify` from
  `Set` when adding the trigger — do not leave both, or every edit notifies twice.
- **Idempotent Migrate — corrected rationale:** in every supported topology exactly
  ONE process migrates the `config` schema (monolith = single process; split = only
  inventory-svc hosts config, per the scope decision). So there is no concurrent
  same-schema migration to defend against — `run.ps1` health-gates each boot serially.
  Idempotency is still wanted for re-runs across restarts: use `CREATE OR REPLACE
  FUNCTION` + `CREATE OR REPLACE TRIGGER` (PG14+), which is also race-safe IF a future
  second config host is ever added (the anticipated characters-svc step) — unlike
  `DROP TRIGGER IF EXISTS; CREATE TRIGGER`, which has a window.
- **Shutdown emit-after-drain (pre-existing, benign):** `app.Run` calls `ctx.Bus.Close()`
  before `appl.Stop()` (app.go:182-183); the listener isn't a bus subscriber and can
  still `bus.Emit` in that window — `mailbox.push` silently drops on a closed bus
  (bus.go:146-148), no panic. Not introduced here (config already runs in the monolith);
  flagged for honesty, not fixed in this plan.
- **Admin editor now in the split at :8081** — intended; the split's config editor.
  characters-svc has no admin, so no config editor there (by design).
- **Broadcast scope honesty:** the split runs ONE config listener (inventory-svc), so
  the split smoke proves cross-connection propagation to that listener, not many-listener
  fan-out. The many-listener broadcast is shown separately via a `psql LISTEN` session
  (Step 4d), not implied by the split smoke.
- **No cross-service write coordination** — last-write-wins on `config.settings`, same
  as any shared table. Acceptable for config.

## Dispatch summary
Step 1 `[sonnet]` (host config in inventory-svc + arch-lint line), Step 2 `[sonnet]`
(hard-require edits + one-line test fix + a `validateRequires` unit test), Step 3
`[opus]` (trigger/plpgsql + `Set` write-path change + reconnect self-heal + two new
tests — correctness-critical), Step 4 `[inline]` (the split verification I owe, now a
COMMITTED smoke script + captured evidence, not an eyeball). Every code-writing subagent
gets explicit `model:` + nav guidance + its lane's `Co-Authored-By` trailer.

## Review resolution (grumpy reviewer, think-hard)
All 9 findings addressed in-plan: #1 trigger INSERT/UPDATE only (listener has no delete
path); #2 reconnect self-heal emits `config.changed` for changed keys (Step 3); #3 split
proof is now a committed script + captured evidence (Step 4); #4 retry-fresh-create to
avoid the one-shot-grant propagation race (Step 4c-2); #5 `validateRequires` fail-loud is
a committed unit test (Step 2); #6 corrected Migrate rationale (single migrator) +
`CREATE OR REPLACE TRIGGER`; #7 honest broadcast scope + separate `psql LISTEN` proof
(Step 4d); #8 tx decision made (single autocommit upsert, drop the tx); #9 shutdown
emit-after-drain acknowledged as benign/pre-existing.
