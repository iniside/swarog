# Config module — central DB-backed configuration with live reload

**Date:** 2026-07-07 09:28
**Branch:** feat/verify-suite (or a fresh `feat/config-module`)
**Status:** plan, pre-review

## Goal

A central `config` module: DB-backed, namespaced `key=value` settings that any
module can read at startup (with a code-default fallback) and that can be edited
live in `/admin` without redeploy. Editing propagates to every reader via Postgres
`LISTEN/NOTIFY` → in-memory cache refresh → `config.changed` bus event.

Two reader models, both supported:
- **Pull** (read-at-use): consumer calls `cfg.GetString(ns, key, default)` at the
  point of use; the value is served from an in-memory cache that the listener keeps
  fresh. Covers limits, flags, thresholds — the majority.
- **Push** (materialized resource): consumer subscribes to `config.changed` to
  rebuild something it built once (a pool, a compiled rule). Wired via `bus.On`.

## Context — why a new module, and why not extend an existing seam

Research method: 6 parallel Sonnet subagents (API surface / bus / lifecycle / admin
contribution / current env reads / module skeleton), each with nav guidance, then
synthesis in main model. Key correction surfaced: **CLAUDE.md's `core.*` naming is
stale** — the code is split into `bus/`, `registry/`, `contrib/`, `lifecycle/`, and
the manifest method is `Requires()`, not `DependsOn()`.

Overlapping existing systems considered:

| Candidate | What it does | Why not extend it |
|---|---|---|
| **env vars (`os.Getenv`)** | Every module reads config today via scattered `envOr`/`envBool` helpers (duplicated per file — `accounts.go:282`, `inventory.go:409`, `admin.go`, `internal/app/app.go`). Read **once** at `Init`, no reload. | This IS the thing we're augmenting. Config is an *override layer over* these defaults — the fallback arg to `GetString` mirrors the existing `envOr(key, def)` idiom exactly, so absence-in-DB degrades identically to unset-in-env. We don't remove env; secrets stay there. |
| **`registry.Provide/Require`** | Sync "ask now" service seam. | We USE it: config `Provide`s the `"config"` service; readers `Require` it against their own 1-method interface. Not a replacement — the delivery mechanism for pull. |
| **`bus` (`Define/Emit/On`)** | Async fire-and-forget fanout. | We USE it: `config.changed` is the push/invalidation signal. Not a config store itself (async, can't answer "what's the value now"). |
| **`contrib` slot + `adminapi`** | Multi-value slot; modules contribute admin `Item`s. | We USE it: config contributes its editor page. But `adminapi` is 100% read-only today (`Render → Content{KPIs, Table}`, zero forms in the whole repo) — so it needs an **additive** extension for the write path (below). |
| **`admin` module** | Serves `/admin`, renders contributed items, HTTP Basic auth via `ADMIN_USER/PASS`. | Reused as the write host: it already owns auth + the shell. We extend its contract additively rather than duplicating auth in config. The template even has a dead "Game Config & Flags — COMING SOON" nav entry (`admin.html.tmpl:57`) — literally this slot. |

Conclusion: config is a genuinely new *foundation* capability (a store + a live
delivery mechanism), not a near-twin of an existing module. It leans on all three
seams rather than reinventing them, and requires exactly one additive contract
change (`adminapi.Form`).

## Design decisions (locked)

1. **Editing UI:** extend `adminapi` additively with a `Form` widget; the **admin**
   module owns the `POST` route + auth + rendering; config supplies a `Submit`
   closure (mirrors the existing `Render` closure pattern). Admin never imports
   config. *(User-approved.)*
2. **Reference consumer = the PUSH/materialized path.** `inventory` materializes a
   starter spec `{item, qty}` and rebuilds it ONLY on `config.changed` (push), so the
   subscription is load-bearing (not a redundant log over a pull that already
   refreshed). The PULL/getter semantics (`GetString/GetBool/GetInt` + fallback) are
   demonstrated + tested directly in `config_test.go`. Together they cover both
   models without a fig-leaf subscriber. *(Reference-consumer choice user-approved;
   split into push-here / pull-in-config-test after review.)*
3. **inventory depends on config SOFTLY, not via `Requires()`.** It uses a new
   `registry.TryRequire` (comma-ok) and holds a possibly-nil `cfg`. This (a) matches
   the user's original intent ("try to reach config; if present use it, else
   fallback"), and (b) keeps the split builds working: `cmd/inventory-svc`
   (`.go-arch-lint.yml:157-164`) does NOT host config, so a hard `Requires("config")`
   would fail `validateRequires` (`internal/app/app.go:193-206`) AND panic the eager
   `Require` (`registry.go:37`). With a soft require, inventory-svc runs and inventory
   falls back to constants. **config is hosted only in `cmd/server`**; live reload of
   the starter item is a monolith-only demo (documented, not a regression).
4. **No cross-module seeding.** `config.settings` starts **empty**. Code defaults
   (the fallback arg / the constant) reproduce today's behavior with an empty table.
   A key first appears when an operator adds it in `/admin`. config's `Migrate`
   creates only its own schema — it must NOT know inventory's keys (isolation).
5. **Secrets stay in env.** `EPIC_CLIENT_SECRET`, `ADMIN_PASS`, `DATABASE_URL` never
   go in `config.settings`.
6. **Bootstrap tier in env.** The listener's own DSN is read from `DATABASE_URL`
   (same default as `internal/app/app.go:44`) — config can't store the DSN it needs
   to reach its own store.
7. **Namespace/key are validated identifiers `^[a-z0-9_]+$`** (rejected on `Set`).
   This makes a plain `:` separator safe everywhere — the `pg_notify` payload
   (`namespace:key`) and the HTML form field `Name`. **Never `\x00`**: Postgres
   `text`/`pg_notify` reject a NUL byte (write would error), and a NUL in an HTML
   attribute is fragile.
8. **`LISTEN/NOTIFY` is the single write→refresh path.** A write (admin or raw
   `psql`) does `pg_notify('config_changed', 'namespace:key')` **inside the same tx
   as the upsert** (so NOTIFY fires iff the write commits); the listener (even in the
   same process) is the ONE place that refreshes the cache and emits `config.changed`.
   Local and external edits are handled identically. Eventually consistent (bus is
   fire-and-forget, constraint #7). **On every (re)connect the listener reloads the
   FULL cache** (`loadAll`), because PG does not queue notifications for a
   disconnected session — a reconnect without a full reload leaves keys changed
   during the gap silently stale forever.

## New/changed files

```
modules/config/config.go              # Module: Name/Requires/Register/Migrate/Init/Start/Stop
modules/config/service.go             # *service: RWMutex cache, typed getters, Set (upsert + pg_notify)
modules/config/store.go               # schema DDL, loadAll, upsert, getOne
modules/config/listen.go              # dedicated pgx LISTEN loop (outbox.Relay cancel+done shape)
modules/config/admin.go               # adminapi.Item Render (KPIs + Table + editable Form + applyEdit Submit)
modules/config/config_test.go         # cache/getters/fallback (pure) + DB Set→load→notify→refresh (local PG)
modules/config/configevents/configevents.go  # Changed{Namespace,Key,Value} + Define("config.changed")
modules/admin/adminapi/adminapi.go    # ADD Form/Field types + Content.Form (additive)
modules/admin/admin.go                # ADD POST /admin/{slug} handler + pageView.Form
modules/admin/admin.html.tmpl         # ADD form render block
registry/registry.go                  # ADD TryRequire[T] (comma-ok, soft dependency)
registry/registry_test.go             # ADD TryRequire present/absent/wrong-type cases
modules/inventory/inventory.go        # ADD configReader iface, TryRequire, materialized starter spec, bus.On
modules/inventory/inventory_test.go   # ADD live-reload-via-event test + cleanup
.go-arch-lint.yml                     # ADD config component + contracts entry + deps + cmdServer dep
cmd/server/main.go                    # ADD import + &config.Module{} (first in slice)
```

## Schema

```sql
CREATE SCHEMA IF NOT EXISTS config;
CREATE TABLE IF NOT EXISTS config.settings (
    namespace  text NOT NULL,
    key        text NOT NULL,
    value      text NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (namespace, key)
);
```

## Service contract (provided under name `"config"`)

```go
// modules/config/service.go — the capability other modules Require against their
// OWN local interface (they need only the getter subset).
type service struct {
    db    *sql.DB
    log   *slog.Logger
    mu    sync.RWMutex
    cache map[cacheKey]string  // cacheKey{namespace, key}
}
func (s *service) GetString(namespace, key, def string) string
func (s *service) GetBool(namespace, key string, def bool) bool   // "1"/"true"/"on", mirrors envBool
func (s *service) GetInt(namespace, key string, def int) int      // strconv.Atoi, def on error
func (s *service) Get(namespace, key string) (string, bool)       // raw
func (s *service) Set(ctx context.Context, namespace, key, value string) error  // validate + tx{upsert + pg_notify}
func (s *service) all() []setting                                 // admin render (sorted)
```

`Set` validates `namespace`/`key` against `^[a-z0-9_]+$` (reject otherwise), then in
**one** `BeginTx`/`Commit`: upsert the row AND `SELECT pg_notify('config_changed',
$1)` with payload `namespace:":"+key`. Two `Exec`s on the pool would use two
connections and could commit the write without notifying — the tx makes them atomic.
`Set` does NOT touch the cache; the listener is the single refresh path (decision #8).

Reader-side (inventory) declares only what it uses:
```go
type configReader interface {
    GetString(namespace, key, def string) string
    GetInt(namespace, key string, def int) int
}
```

New foundation helper (`registry/registry.go`) — the soft-require the reference
consumer needs so it degrades when config isn't hosted:
```go
// TryRequire is the comma-ok Require: (svc, true) if present AND assignable to T,
// else (zero, false). No panic — for an OPTIONAL dependency.
func TryRequire[T any](r *Registry, name string) (T, bool)
```

## `adminapi` additive extension

```go
type Content struct {
    KPIs  []KPI
    Table *Table
    Form  *Form   // NEW — nil = today's read-only behavior (backward compatible)
}
type Form struct {
    Action string                                                     // page slug it posts back to; admin fills this
    Fields []Field
    Submit func(ctx context.Context, values map[string]string) error `json:"-"` // local-only; nil across the remote wire
}
type Field struct {
    Name  string   // form input name + Submit map key
    Label string
    Value string   // current value, pre-filled
}
```
`Submit` is `json:"-"` so the remote `ItemData` marshal (the `writeJSON(ItemData{…})`
path, `inventory/admin.go:28`) never fails on a func field; remote forms render
read-only (no Submit) in v1. **apidiff invariant:** adding `Content.Form *Form` is
additive-safe *because `Content` is already non-comparable* (`KPIs []KPI`,
`adminapi.go:44`) — adding a slice/func-bearing field doesn't flip comparability, so
apidiff sees no incompatibility. If `Content` were ever all-comparable this would
flag; keep it non-comparable.

---

## Steps (ordered)

### Step 1 — `adminapi` contract extension  `[opus]`
- **(a) What:** `modules/admin/adminapi/adminapi.go` — add `Form`, `Field` structs
  and the `Form *Form` field on `Content` (with `Submit ... json:"-"`).
- **(b) Why now:** it's the shared contract both the admin render/write path (Step 2)
  and config's editor (Step 3) compile against. Additive → compiles standalone.
- **(c) How:** zero-value `Form == nil` must mean "exactly today's behavior" so all
  existing contributors (accounts/characters/inventory) are untouched. Doc-comment
  the local-only nature of `Submit`. Do NOT change `Item`/`KPI`/`Table`/`Cell`.
- **Commit:** `feat(admin): additive Form/Field widget in adminapi contract`

### Step 2 — admin renders + applies the Form  `[opus]`
- **(a) What:** `modules/admin/admin.go` (`pageView` gains `Form *adminapi.Form`;
  `handleItem` passes `content.Form` through; new `POST /admin/{slug}` →
  `handleItemPost`) + `modules/admin/admin.html.tmpl` (a `{{with .Page.Form}}` block
  rendering `<form method="post" action="/admin/{{slug}}">` with a text input per
  `Field`, pre-filled `Value`, plus a submit button).
- **(b) Why now:** config's editor (Step 3) needs a working write host before it can
  expose editing; admin owns auth (`gate`) + shell, so the POST lives here, not in
  config.
- **(c) How:** register `ctx.Mux.HandleFunc("POST /admin/{slug}", m.gate(m.handleItemPost))`
  beside the two GETs in `Init` (admin.go:67-68). `handleItemPost`: resolve the item
  via the same `items(r.Context())` + slug match as `handleItem`; if it's a LOCAL
  item, call `cur.render(ctx)` to obtain `Content.Form` (Render is idempotent/read-
  only — safe to call for the closure); if `Form == nil` → 404/redirect; else
  `r.ParseForm()`, build `map[string]string` from `Form.Fields[].Name`, call
  `Form.Submit(ctx, values)`; on success `http.Redirect(303 /admin/{slug})` (re-GET
  re-renders fresh values), on error re-render the page with an error card. Gate
  already enforces auth. Remote items have no Submit → treat as read-only (405/redirect).
- **Commit:** `feat(admin): render + POST editable Form widgets on /admin/{slug}`

### Step 3 — `registry.TryRequire` (soft dependency)  `[sonnet]`
- **(a) What:** `registry/registry.go` — add `TryRequire[T](r, name) (T, bool)`;
  `registry/registry_test.go` — present / absent / wrong-type cases.
- **(b) Why now:** the reference consumer (Step 5) needs it to depend on config
  optionally without breaking the split builds (decision #3); it's a leaf with no
  arch-lint delta, so land it before the consumers use it.
- **(c) How:** comma-ok on the map, then a comma-ok type assertion; return the zero
  `T` + `false` on miss or wrong type. Mirror `Require`'s two failure conditions
  (`registry.go:37,41`) but return instead of panic. Pure, no I/O.
- **Commit:** `feat(registry): TryRequire — comma-ok optional service lookup`

### Step 4 — the `config` module  `[opus]`
- **(a) What:** new `modules/config/` (`config.go`, `service.go`, `store.go`,
  `listen.go`, `admin.go`, `config_test.go`) + `modules/config/configevents/configevents.go`.
- **(b) Why now:** depends on Steps 1–2 (the Form contract + admin write path) to
  expose its editor; prerequisite for the inventory reference (Step 5) + registration
  (Step 6).
- **(c) How — the non-mechanical parts:**
  - **Lifecycle placement (constraint #8 — Init does no I/O):**
    - `Register(ctx)`: build `&service{cache: map[...]{}}`, `registry.Provide(ctx.Registry, "config", m.svc)`.
    - `Migrate(ctx, db)`: exec the schema DDL (mirrors `leaderboard.go:33`).
    - `Init(ctx)`: store `db/log/bus`; read listener DSN via
      `envOr("DATABASE_URL", "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable")`
      (exact default from `internal/app/app.go:44`); `ctx.Contribute(adminapi.Slot, adminapi.Item{ID:"config", Section:"Platform", Label:"Game Config & Flags", Render: m.adminRender})`.
      **No DB I/O here.**
    - `Start(ctx)`: launch the listen loop using the `outbox.Relay` shape —
      `runCtx,cancel := context.WithCancel(context.Background())`, store `cancel` +
      `done chan struct{}`, `go m.listen(runCtx)`. The initial `loadAll` is done by
      the listen loop on first connect (below), NOT separately here, so there is one
      cache-population path shared by boot and reconnect.
    - `Stop(ctx)`: `cancel()`, wait on `done` or `ctx.Done()`. **Does NOT close the
      pgx conn** — the loop owns it (`defer conn.Close()` inside `listen`), because
      the conn is re-created on every reconnect and `Stop` has no stable handle.
  - **`service.Set`:** validate ids (decision #7), then `BeginTx` → upsert
    `INSERT ... ON CONFLICT (namespace,key) DO UPDATE SET value=$3, updated_at=now()`
    → `SELECT pg_notify('config_changed', $1)` with `namespace+":"+key` → `Commit`.
    Atomic; does NOT touch the cache (listener refreshes — decision #8).
  - **`listen.go`:** a loop that owns its `*pgx.Conn` (`github.com/jackc/pgx/v5` — a
    DIRECT require, `go.mod:8`; raw pgx needed because `database/sql` can't
    `WaitForNotification`). Structure: `for runCtx not cancelled { conn = pgx.Connect;
    conn.Exec("LISTEN config_changed"); m.svc.loadAll(runCtx)  // full reload on every
    (re)connect — decision #8; inner: for { n,err := conn.WaitForNotification(runCtx);
    if runCtx cancelled → return; if err → conn.Close(), break to reconnect w/ backoff;
    split n.Payload on first ':' → ns,key; getOne from DB; cache Lock+set;
    bus.Emit(ChangedEvent) } }`. `defer conn.Close()` guards the current conn on any
    exit. Reconnect uses a bounded backoff ticker; a permanent DB outage degrades to
    "stale cache + retrying," never a silent dead goroutine.
  - **`admin.go` (`adminRender`):** `Content{ KPIs:[{"Settings",len},{"Namespaces",n}],
    Table: (namespace,key,value,updated_at) from `svc.all()`, Form: &Form{ Fields: one
    per setting (Name = `ns+":"+key`, Value = current) + add-new triple
    (`_new_namespace`,`_new_key`,`_new_value`), Submit: m.applyEdit } }`.
    **`applyEdit(ctx, values)` diffs against the current cache and calls `svc.Set`
    ONLY for keys whose value actually changed** (decision #8 makes each `Set` a
    NOTIFY + `config.changed`; rewriting all N rows on every save would emit a storm
    of false "changed" events). Then, if the add-new triple is fully filled, `Set` it.
    Config owns add-new semantics; the `adminapi.Form` contract stays generic name/value.
  - **`configevents`:** `type Changed struct{ Namespace, Key, Value string }`;
    `var ChangedEvent = bus.Define[Changed]("config.changed")`.
  - **Tests (`config_test.go`), all using a UNIQUE per-test namespace (UUID, like
    `inventory_test.go:39-46`) to avoid polluting the shared `config.settings`:**
    (1) pure — inject cache, assert `GetString/GetBool/GetInt` hit + fallback-on-miss
    (this is the PULL demonstration, decision #2);
    (2) DB-backed (local Postgres, per repo memory) — `Set` → fresh `loadAll` → `Get`;
    (3) real notify path — `Set`, then **poll `Get` with a deadline** (e.g. 2s, 20ms
    tick) asserting the listener refreshed the cache; NOT "call the refresh directly."
    Each test `t.Cleanup` deletes its namespace's rows.
- **Commit:** `feat(config): DB-backed live config module (service, LISTEN/NOTIFY, admin editor)`

### Step 5 — inventory reference consumer (push / materialized)  `[opus]`
- **(a) What:** `modules/inventory/inventory.go` — add `configReader` local iface; a
  materialized `starter` spec `{item string; qty int}` behind a `sync.RWMutex` + a
  `loaded bool`; `registry.TryRequire[configReader]` in `Init` (holds nil if config
  absent); `bus.On(ctx.Bus, configevents.ChangedEvent, m.onConfigChanged)` only when
  `cfg != nil`. `modules/inventory/inventory_test.go` — add the live-reload test.
- **(b) Why now:** needs the config service + events (Step 4) and `TryRequire`
  (Step 3). Its `bus.On` gives `topiccheck` a real cross-module subscriber for
  `config.changed` (confirmed the analyzer matches cross-module `On` by object
  identity, `tools/topiccheck/main.go:180-193`).
- **(c) How — why this is a REAL push demo, not a fig leaf:**
  - `starterSpec()` helper: lazy-load under the mutex on first use (order-independent
    — no reliance on config.Start running before inventory.Start). If `cfg == nil`,
    load constants (`starterItem="starter_sword"`, qty 1, inventory.go:24). Grant
    reads the MATERIALIZED spec; it does NOT re-pull per grant.
  - `onConfigChanged(e)`: if `e.Namespace=="inventory"` and key ∈ {`starter_item`,
    `starter_qty`}, rebuild the spec under `Lock` from `cfg` and log. **This is the
    only refresh path for the spec** → the subscription is load-bearing; without the
    event, edits would never reach a running inventory. This is the genuine push
    demonstration (pull is covered by `config_test`, decision #2).
  - **Existing `TestInventoryReactsToCharacterLifecycle` stays green:** it builds
    `&Module{store, log}` with `cfg == nil` and never calls `Init` (`inventory_test.go:87`),
    so `starterSpec()` loads constants → grants `starter_sword`×1 → `inventory_test.go:98`
    (`qtyOf(list, starterItem)==1`) passes. (The prior plan's claim was wrong because
    it assumed a per-grant pull that would nil-panic here; the materialized+nil-guard
    design is what actually keeps it green.)
  - **New test** (`t.Cleanup` deletes namespace `inventory` rows): wire a real config
    service, `Init` inventory, `Set("inventory","starter_item","health_potion")`, then
    poll-with-deadline creating a character until the grant is `health_potion` — this
    exercises the FULL chain Set→pg_notify→listener→`config.changed`→`onConfigChanged`
    →spec-rebuild→grant, i.e. the real push path (addresses the "is the listener path
    tested" gap).
- **Commit:** `feat(inventory): materialize starter item from config, live-reload on config.changed`

### Step 6 — wiring & registration  `[sonnet]`
- **(a) What:** `.go-arch-lint.yml` + `cmd/server/main.go`.
- **(b) Why now:** only valid once the packages exist; makes the module live and
  keeps `go-arch-lint` green.
- **(c) How — exact deltas:**
  - `.go-arch-lint.yml`: add component `config: { in: modules/config }` (alphabetical,
    ~line 44); add `modules/config/configevents` to `contracts.in` (~line 39); add
    `config: { mayDependOn: [ lifecycle, contracts ] }` to `deps` (~line 108); add
    `- config` under `cmdServer.mayDependOn` (~line 141). (pgx/v5 is vendor →
    `depOnAnyVendor: true` covers it; no `edge`/`outbox` needed.)
  - `cmd/server/main.go`: add `"gamebackend/modules/config"` import (alphabetical);
    add `&config.Module{},` as the FIRST entry in the `mods` slice (foundation
    convention). Ordering note: `Register` phase already guarantees the service exists
    for any `TryRequire` regardless of slice position, and the real correctness
    guarantee for reads is that `app.Run` completes ALL `Start`s before HTTP serves
    (`internal/app/app.go:133-153`) — not Start-order among modules. Config-first is
    defensive tidiness, not load-bearing. (config is NOT added to `cmd/inventory-svc`
    — inventory soft-degrades there, decision #3.)
- **Commit:** `chore(config): register module + arch-lint boundaries`

### Step 7 — verify  `[inline]`
- **(a) What:** run `./verify.ps1 --all`.
- **(b) Why last:** the gate. Confirms build/vet/golangci-lint/go-arch-lint/`go test
  ./...`/govulncheck (blocking) + apidiff (additive `Form` passes), topiccheck
  (inventory subscribes → `config.changed` satisfied), and the new tests.
- **(c) How:** if apidiff flags `Form` as non-additive, re-check the field is added
  (not a reshape) and `Content` stayed non-comparable (apidiff invariant above). If
  topiccheck still flags, confirm `bus.On(configevents.ChangedEvent…)` compiled into
  inventory. Drive one manual smoke: start server, add
  `inventory:starter_item=health_potion` in `/admin`, create a character, confirm the
  grant — the live-reload payoff, observed end-to-end (per the `verify` skill).

---

## Risks / watch-items
- **pgx raw conn owned by the loop:** the loop opens/owns its `*pgx.Conn` and
  `loadAll`s on every (re)connect (decision #8); `Stop` only cancels + waits, never
  closes the conn (no stable handle across reconnects). A permanent DB outage → stale
  cache + bounded-backoff retry, never a silent dead goroutine.
- **Admin POST re-renders Content** to obtain the `Submit` closure — fine because
  `Render` is read-only/idempotent; note it if `Render` ever gains side effects.
- **applyEdit diffs before writing** — one `Set` (one NOTIFY, one `config.changed`)
  per genuinely-changed key, not per rendered field.
- **Eventual consistency:** a write is visible to readers only after the NOTIFY round-
  trip refreshes the cache — acceptable for config, not a transaction.
- **apidiff invariant:** `Content.Form` is additive-safe only while `Content` stays
  non-comparable (already true). Don't make `Content` all-comparable.
- **Split-topology:** live reload is a `cmd/server` (monolith) feature; `cmd/inventory-svc`
  runs without config and inventory falls back to constants (by design, decision #3).

## Dispatch summary
Step 3 `[sonnet]` (small pure `TryRequire`), Steps 1–2 + 4–5 `[opus]` (the contract
change + the correctness-critical module — cache concurrency, background loop, pgx,
LISTEN/NOTIFY — and the materialized-push consumer), Step 6 `[sonnet]` (wiring against
existing patterns), Step 7 `[inline]` (verification/observation). Every code-writing
subagent gets an explicit `model:` + the nav guidance + its lane's `Co-Authored-By`
trailer (Opus 4.8 / Sonnet 4.6).

## Execution order (dependencies)
1 (adminapi contract) → 2 (admin render/POST) → 4 (config module) needs 1+2;
3 (`TryRequire`) is independent, land any time before 5; 5 (inventory) needs 3+4;
6 (registration) needs 4; 7 (verify) last. A valid linear order: **1 → 2 → 3 → 4 →
5 → 6 → 7** (as numbered).
