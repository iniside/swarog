# Architecture-enforcement checkers — plan

**Date:** 2026-07-09 08:39
**Goal:** Close the gaps where CLAUDE.md declares a *hard constraint* but nothing
catches a violation statically — today those failures surface only as a loud panic at
process startup (or not at all). Add/extend mechanical checkers so each of the three
seams + the persistence/test-layout rules has a static net, wired into `verify`.

## Context — the overlapping existing systems (Research-before-planning)

The repo already enforces a lot mechanically. This plan does **not** re-invent any of
it; every item below is either a NEW checker for an un-netted rule, or an ADDITIVE
extension to an existing checker. Overlap map (why-not-extend-X is answered per item):

| Existing enforcer | What it already catches | Gap this plan fills |
|---|---|---|
| `archcheck` (`tools/archcheck`) | module→module, module→foreign-`rpc`, single front-door on `gateway` crate, `Option<edge::Server>` tripwire | (a) `<name>api` transport-freedom, (b) `cmd/*-svc` must list `metrics`, (c) cross-schema FK, (d) inline tests — all **extend this tool** (it already reads `cargo metadata` + walks `modules/`) |
| `topiccheck` (`tools/topiccheck`) | defined-but-unsubscribed topics, via a real register→init runtime harness | cross-module event subscribed with in-process `on()` instead of durable `on_tx()` — **extends this tool** (its harness already separates durable vs in-process subs) |
| `app::validate_requires` (`core/app/src/lib.rs:149`) | declared `requires()` name has NO present provider (under-provisioning, at boot) | the OPPOSITE drift — a real `require()` call whose provider is NOT declared in `requires()` (under-declaration). `validate_requires` cannot see actual `require()` calls, so it can't catch this → **new `requirecheck` tool** |
| `public-api` verify stage | additive-only diff of contract crates | — (orthogonal) |
| `fortress` verify stage | every `cmd/*-svc` compiles + runs archcheck | — (host for the new archcheck rules + `requirecheck`) |

**Registry seam is the one seam with no static net.** `validate_requires` is a
manifest check (names vs names); it never observes a `ctx.registry().require::<T>(key)`
call. So a module that calls `require("characters.ownership")` but forgets
`requires("characters")` compiles, passes archcheck (the crate dep exists), passes
`validate_requires` (it doesn't declare the dep, so nothing to check) — and then
**panics at `init` in any split process where characters isn't co-hosted and no stub
is wired**. That is exactly the "fails loudly at startup" class the user wants moved
to static time.

**Why a runtime harness and not a cheaper syntactic scan (steelman + decision).** All
3 mandatory sites today use literal `key("characters","ownership")` args, so an
AST/regex scan of the source could extract them at zero foundation cost — and this is
NOT the case topiccheck's doc rejected (that rejection was about `linkme` annotation
macros *decoupled* from the real call; reading the actual `require::<dyn T>(key(...))`
call text is as honest as topiccheck reading `on_tx`). The runtime harness is
nonetheless the right choice for three concrete reasons, not dogma: (i) it is robust to
helper-fn / non-literal-key indirection a regex would miss if a future site stops using
a literal; (ii) it keeps ONE methodology across both registry and bus checkers (the
seam is a small faithful mirror of `Bus::set_transport`); (iii) `split-proof` already
exercises the *panic* path at runtime, so the static gap is specifically
"stub-masked under-declaration" — a co-hosted stub satisfies the require so NO runtime
symptom ever fires, and only static observation of the call-vs-declaration delta catches
it. requirecheck is a high-value item, but its marginal value over a syntactic scan is
robustness + uniformity, not correctness on today's 3 literal sites — stated honestly so
the cost is justified, not asserted.

**Structural limitation (must be documented in the tool + this plan).** The harness runs
only `register` + `init` (the two no-I/O phases). A `require`/`try_require` issued from
`start` (the real-I/O phase, never run here) or lazily inside a provided service's
request handler is **invisible** to requirecheck. None exist today (all 4 sites are in
`init`), but the tool's header comment MUST state the assumption: *requires must be
resolved in `init` to be checked* — otherwise the tool gives false confidence about a
seam it structurally cannot fully see.

### Baselines (current tree — every checker must PASS on commit)

- **requirecheck:** clean. Only 3 mandatory `require` sites exist
  (`inventory`→`characters.ownership` @ `modules/inventory/src/lib.rs:629`,
  `inventory`→`config.reader` @ `:651`, `match`→`rating.mmr_reader` @
  `modules/match/src/lib.rs:205`); all three providers are declared in the caller's
  `requires()`. One optional `try_require` (`gateway`→`accounts.sessions` @
  `modules/gateway/src/verifier.rs:114`) that deliberately is NOT declared.
- **topiccheck durability:** clean. All 7 cross-module subscriptions already use
  `on_tx`/`on_tx_raw`; zero plain `on()` to a foreign-defined topic.
- **archcheck api-transport-free:** clean. No `<name>api` crate depends on
  `tokio/quinn/axum/hyper/sqlx/edge/remote` (transitively verified).
- **archcheck cmd metrics:** clean — all 11 `-svc` + `server` list `metrics`.
  (NB: `messaging` is NOT universal — `admin-svc` and `gateway-svc` intentionally omit
  it; see Step 4 decision.)
- **archcheck schema-isolation:** clean. 3 `REFERENCES` in the tree, all same-schema
  (`accounts.*`→`accounts.players`, `inventory.*`→`inventory.items`).
- **archcheck inline-tests:** clean. Every module uses `#[cfg(test)] mod tests;`
  (declaration to a separate file), never an inline `mod tests { … }` body.

Every checker is therefore a **regression guard** that passes today — the correct
baseline (a checker that fails on commit would be blocking a pre-existing violation we
haven't decided to fix).

---

## Key facts pinned by research (the non-obvious ones)

1. **The registry has NO injection/introspection seam** (unlike the bus).
   `core/registry/src/lib.rs`: `Registry` is a concrete struct with a private
   `services: Mutex<HashMap<String, Box<dyn Any…>>>`; `Context` builds it internally
   (`context.rs:41`) and exposes only `ctx.registry() -> &Arc<Registry>` — **no
   `set_registry`, no `keys()`, no observer.** `require`/`try_require` only *read* the
   map, leaving no trace. So topiccheck's "swap a recording `dyn Transport`" trick has
   **no registry analogue today** — `requirecheck` needs a new generic hook in
   `core/registry` (Step 1). The hook sees only a `&str` key → foundation-safe (imports
   no module, no `api/` crate), the honest analogue of `Bus::set_transport`.
2. **Attribution requires a manual two-phase loop.** `App::build()`
   (`core/lifecycle/src/app.rs:51`) runs every module's `init` in one loop, so an
   observer that sees only the key can't tell *which* module called `require`.
   `requirecheck` must therefore NOT call `app.build()` — it replicates the two phases
   itself: `register(&ctx)` for all modules, then per-module set a "current module"
   marker on the recorder immediately before `m.init(&ctx)`. Both are public trait
   methods; `Caps::REGISTER` gating mirrors `App::build`.
3. **`require` vs `try_require` must be distinguished at the hook.** `require`
   (mandatory) and `try_require` (optional) both hit the same key. Only mandatory
   requires must be declared in `requires()`. So the observer takes a *kind* flag;
   `require` fires `Mandatory`, `try_require` fires `Optional`. requirecheck enforces
   `{Mandatory provider-prefixes} ⊆ {requires() names}` and reports Optional
   informationally. Without this, `gateway`'s deliberate un-declared `accounts.sessions`
   `try_require` would false-fail.
4. **`messaging` is a manifest-only dependency.** Every persisting/emitting module
   declares `requires("messaging")`, but `messaging` is consumed via `ctx.bus()`, never
   via a keyed `require()`. Its registry entry is a bare `"messaging"` marker
   (`core/messaging/src/lib.rs:315`) for `validate_requires` only. requirecheck must
   **allowlist `messaging`** as a name that legitimately appears in `requires()` without
   a backing require-key — else over-declaration reporting false-flags every module.
   (This is why the enforced direction is under-declaration: `require`-call ⊆
   `requires()`; over-declaration is advisory-only.)
5. **`requires()` returns provider MODULE NAMES**, not capability keys
   (`core/lifecycle/src/module.rs:60`, `Vec<String>`). A require key
   `"rating.mmr_reader"` maps to provider `"rating"` via the prefix before the first
   `.` — exactly how `registry::key(module, cap)` composes it
   (`core/registry/src/lib.rs`).
6. **Transport-free forbid-list must include `edge`/`remote`, not just raw crates.**
   No `api/` crate names `tokio`/`quinn`/etc. directly — the workspace routes transport
   through the `edge`/`remote` core crates (used legitimately by `<name>rpc`, never by
   `<name>api`). A raw-transport-only forbid-list would be a no-op tripwire; the
   realistic regression is an `<name>api` picking up `edge`/`remote`. Forbid both the
   raw crates (future-proofing) AND `edge`/`remote` (the real vector).
7. **`metrics` yes, `messaging` no, for the cmd rule.** CLAUDE.md documents only
   "every main lists `metrics::Metrics::new()`". Requiring `messaging` universally would
   FAIL on `admin-svc` (pure aggregator, no DB) and `gateway-svc` (pure transport, no
   DB) — both intentional per their Cargo.toml headers. So Step 4 enforces **`metrics`
   only** (a real, documented, currently-passing invariant). Messaging-universal is NOT
   a codified invariant and is dropped.
8. **Migrations are inline `SCHEMA_DDL` raw-string consts** in each
   `modules/<name>/src/lib.rs`, run via `sqlx::raw_sql` in `Module::migrate`. No
   `migrations/` dir, no `.sql` files. Schema name == module name via `CREATE SCHEMA IF
   NOT EXISTS <module>;`, tables schema-qualified `<module>.<table>`. A per-file scan
   (schema declared in this file vs schema in any `REFERENCES` in this file) is
   sufficient — DDL is self-contained per module.
9. **Inline-test discriminator:** key on `mod\s+tests\s*\{` / `mod\s+\w+_tests\s*\{`
   (brace = banned inline body) vs `mod tests;` (semicolon = allowed declaration). Do
   NOT key on `#[cfg(test)]` (present on every impl file). Watch the false-positive
   shape `modules/webui/src/lib.rs:63` — `#[cfg(test)] pub(crate) fn test_router()` is a
   test-only *fn*, not a `mod`, so a `mod`-anchored regex correctly ignores it.
10. **Tool crates may depend on module impl crates.** `tools/topiccheck/Cargo.toml`
    depends on all 12 module impls; archcheck classifies `tools/` as `Kind::Other`
    (unconstrained). The fortress rule does not apply to tools. `match`'s crate is named
    `match_module`.

---

## Step sequence

### Step 1 — `core/registry` require-observer seam  `[opus]`
**(a) What:** `core/registry/src/lib.rs` — add an opt-in observer to `Registry`.
- New field `require_observer: Mutex<Option<Arc<dyn Fn(RequireKind, &str) + Send + Sync>>>`
  (interior-mutable, mirrors `Bus`'s `Mutex<Option<Arc<dyn Transport>>>`).
- New `pub enum RequireKind { Mandatory, Optional }`.
- New `pub fn set_require_observer(&self, f: Arc<dyn Fn(RequireKind, &str) + Send + Sync>)`
  (takes `&self`, so no `Context` change needed — installs via
  `ctx.registry().set_require_observer(...)`).
- At the top of `require::<T>` fire `observer(Mandatory, name)`; at the top of
  `try_require::<T>` fire `observer(Optional, name)` — guarded `if let Some(obs) = …`.
- Unit test in `core/registry/src/tests.rs` (NEW or existing): install an observer,
  call `provide` + `require` + `try_require`, assert the recorded `(kind, key)` sequence;
  assert no-observer path is a no-op.

**(b) Why now / order:** every later requirecheck step depends on this seam existing;
it is the only foundation (`core/*`) change in the plan and the highest-risk
(hard-constraint crate), so it goes first and gets reviewed before anything builds on
it. Must stay foundation-pure — the closure sees only `RequireKind` + `&str`, imports
nothing from a module or `api/`.

**(c) How (non-mechanical):** the observer is behind a `Mutex<Option<…>>` exactly like
`Bus::set_transport` — read the existing bus pattern (`core/bus/src/lib.rs:167`) and
mirror it so the two seams are idiomatically identical. Fire the observer BEFORE
`self.services.lock()` (the map lookup) — both so a missing-key panic still records the
attempted require, AND so the observer runs OUTSIDE the services lock. **Doc invariant
(write it in the setter's doc comment): the observer closure MUST NOT call back into the
registry** — `std::sync::Mutex` is non-reentrant, so a re-entrant `require`/`provide`
from inside the observer would deadlock. requirecheck's recorder only touches its own
`Mutex<Recorder>`, so it is safe. The require/try_require methods are startup/wiring-only
(resolved once into `OnceCell`s, never per-request), so the added lock is negligible —
"hot path" is a non-issue here, but keeping the closure lock-free of the registry is the
real correctness point. Keep the no-observer path a plain `if let Some(obs) = …` no-op.

**(d) Dispatch:** `[opus]` — foundation seam, correctness-critical, separate context =
reviewer boundary. Effort embedded in prompt.

---

### Step 2 — `tools/requirecheck` new tool + harness  `[opus]`
**(a) What:**
- Root `Cargo.toml`: add `"tools/requirecheck"` to `[workspace] members` (next to
  `tools/topiccheck` @ line ~35).
- New `tools/requirecheck/Cargo.toml`: model on `tools/topiccheck/Cargo.toml`
  (`publish = false`, `[[bin]]`, workspace-version). Deps (all `{ workspace = true }`):
  `lifecycle`, `registry`, `app` (to also reuse/compare `validate_requires` if useful),
  `sqlx`, `tokio`, `async-trait`, `anyhow`, plus **all 12 module impl crates**
  (`config, characters, inventory, accounts, admin, audit, scheduler, match_module,
  rating, leaderboard, webui, gateway`). Does NOT need the `events` crates.
- New `tools/requirecheck/src/main.rs`: the harness.
- New `tools/requirecheck/src/tests.rs`: pure-diff unit test (the `unsubscribed`-style
  factored function — see (c)).

**(b) Why now / order:** consumes the Step-1 seam; must exist before Step 3 wires it
into verify. Second because it's the headline value.

**(c) How (non-mechanical):** clone `topiccheck::collect_subscriptions` structure with
two deltas:
- Build `ctx = Arc::new(Context::with_db(PgPool::connect_lazy(dsn)))` (lazy pool never
  connects — register/init do no I/O, constraint 8; same DSN fallback as topiccheck).
- **Install a no-op bus transport FIRST — mandatory precondition.** `inventory::init`
  (`modules/inventory/src/lib.rs:638`) and `match::init`
  (`modules/match/src/lib.rs:156`) call `ctx.bus().on_tx(...)`, and `Bus::on_tx_raw`
  **panics if no transport is installed** (`core/bus/src/lib.rs:250`). So requirecheck
  MUST `ctx.bus().set_transport(Arc::new(<no-op transport>))` before running the
  two-phase loop, exactly as topiccheck does (`main.rs:164`). Reuse topiccheck's
  `RecordingTransport` (or a trivial no-op impl of `bus::Transport` whose `enqueue_tx`
  and `subscribe_tx` do nothing) — without this the harness dies at the first `init`
  before observing a single `require`.
- Install the recorder: a `Arc<Mutex<Recorder>>` where `Recorder` holds
  `current_module: Option<String>` and `hits: Vec<(String /*module*/, RequireKind, String /*key*/)>`.
  `ctx.registry().set_require_observer(Arc::new(move |kind, key| { record with
  current_module }))`. The recorder MUST tolerate `current_module == None` (a `require`
  fired during phase-1 `register`, before any per-module marker is set) — drop or bucket
  it under a `"(register-phase)"` label rather than panicking; none exist today.
- **Manual two-phase loop** (NOT `app.build()`): iterate `monolith_modules()` (mirror
  topiccheck's list, but with real `messaging`? No — messaging isn't required via the
  registry, and register/init do no I/O, so include the same 12-module set topiccheck
  uses). Phase 1: `for m in &modules { if m.caps().contains(Caps::REGISTER) { m.register(&ctx) } }`.
  Phase 2: `for m in &modules { recorder.set_current(m.name()); m.init(&ctx)?; }`.
  Confirm the exact `Caps`/`register`/`init` signatures against `core/lifecycle` at
  implementation time.
- Collect declared requires: `for m in &modules { declared[m.name()] = m.requires() }`.
- **Diff (the pure, unit-tested function):** for each module, `observed_mandatory =
  { key.split('.').next() for (mod,kind,key) in hits if mod==m && kind==Mandatory }`;
  `violation = observed_mandatory - (declared[m] ∪ ALLOWLIST)` where `ALLOWLIST =
  {"messaging"}`. Any non-empty `violation` = under-declaration FAIL. Separately, an
  ADVISORY over-declaration report: `declared[m] - observed_mandatory - ALLOWLIST` (a
  declared provider never keyed-required — informational, since bus-only deps like a
  future marker legitimately land here; do NOT fail `--strict` on it).
- Output shape: mirror topiccheck — a table (MODULE | DECLARED requires() | OBSERVED
  require providers | VERDICT), `--strict` exits non-zero on any under-declaration.
- Factor the diff into a free function taking `(hits, declared, allow)` so
  `src/tests.rs` can unit-test it without the lifecycle harness (exactly how topiccheck
  factors `unsubscribed`).

**(d) Dispatch:** `[opus]` — new harness design, correctness-critical attribution
logic. Effort embedded.

---

### Step 3 — wire `requirecheck` + topiccheck-durability into `verify.sh` + `verify.ps1`  `[sonnet]`
**(a) What:** fold two BLOCKING invocations into the `fortress` stage (both are
hard-constraint regression guards like archcheck, which already lives there).
- `verify.sh:138-141` `fortress()`: append `&& cargo run -q -p requirecheck -- --strict`
  AND `&& cargo run -q -p topiccheck -- --durability-strict` (the durability half of
  Step 6 — hard constraint, must block). Chain order after `archcheck`.
- `verify.ps1` `Invoke-FortressStage` (~:94-106): add the same two as
  `$LASTEXITCODE`-checked commands after `archcheck`.
- Leave the EXISTING advisory `topiccheck` stage (full table, `--strict` in the `--all`
  tier) untouched — it still reports unsubscribed + durability advisorily.

**(b) Why now / order:** requirecheck (Step 2) and topiccheck's `--durability-strict`
flag (Step 6) must exist before they can be invoked — so this step sits after both are
authored. NB: this creates a soft cross-dependency (Step 3 needs Step 6's flag), so if
Steps are executed strictly top-to-bottom, DEFER the topiccheck line of Step 3 until
Step 6 lands, or reorder Step 6 before Step 3. Simplest: implement Step 6 first, then do
this combined wiring once. No install machinery needed — pure workspace `cargo run -p`
bypasses `ensure_tool` (confirmed).

**(c) How:** copy the exact archcheck invocation shape already in `fortress()` — same
`cargo run -q -p <tool>` idiom. No new stage function; fold into the existing chain so
both share the fortress PASS/FAIL row. Fully specified — no judgment.

**(d) Dispatch:** `[sonnet]` — mechanical, both scripts, fully-specified snippet.

---

### Step 4 — archcheck dep-graph rules: `<name>api` transport-free + `cmd/*-svc` lists `metrics`  `[sonnet]`
**(a) What:** `tools/archcheck/src/main.rs`.
- `classify()` (lines 47-66): add `Kind::Api(String)` — extend the existing
  `p.split("/api/").nth(1)` block with a sibling arm `parts[1] == "api"` mirroring the
  existing `"rpc"` arm exactly.
- New rule A (transport-free api): in the `cargo metadata` dependency loop, for each
  `Kind::Api` package, FAIL on any normal (non-dev) dep whose name is in
  `FORBIDDEN_API_DEPS = ["tokio","quinn","axum","hyper","sqlx","tonic","reqwest","tower","edge","remote"]`.
  (Raw crates for future-proofing; `edge`/`remote` are the realistic vector — fact 6.)
- New rule B (cmd metrics): for each `Kind::Cmd` whose dir ends in `-svc` **OR is
  `server`** (the monolith — CLAUDE.md says "every main lists `metrics`", and `server`
  is a main), FAIL if its normal deps do NOT include `metrics`. It's a one-line
  `cmd.ends_with("-svc") || cmd == "server"`, and closes the gap the reviewer flagged
  (keying on `-svc` alone would exempt the monolith from the very "every main"
  invariant). **Do NOT require `messaging`** (fact 7 — would false-fail
  admin-svc/gateway-svc, and it's not a documented invariant).
- Tests in `tools/archcheck/src/tests.rs`: add cases for `classify` → `Kind::Api`, a
  synthetic api-pkg-with-`edge` → violation, an svc-without-`metrics` → violation.

**(b) Why now / order:** independent of Steps 1-3; grouped here because both rules
operate on the same `cargo metadata` dependency loop archcheck already runs. Ordered
after the requirecheck block so the two headline items land first.

**(c) How:** both rules slot into the existing `for pkg in packages` loop that already
does the module→module + front-door checks — add two more match arms / conditionals,
reusing the existing `by_name` map and the dev-dep skip. Mirror the existing violation
`format!` message style. The forbid-list is a `const [&str; N]`.

**(d) Dispatch:** `[sonnet]` — additive to an existing loop, forbid-list + exempt
decisions fully specified above; no open judgment.

---

### Step 5 — archcheck source-walk tripwires: cross-schema FK + inline tests  `[sonnet]`
**(a) What:** `tools/archcheck/src/main.rs` — two new file-walk tripwires, mirroring the
existing `grep_option_edge_server` walk over `modules/`.
- Cross-schema FK: for each `modules/<name>/src/*.rs`, find `CREATE SCHEMA IF NOT EXISTS
  <own>;` (own == the `modules/<name>` path segment; assert they match as a sanity
  check) and every `REFERENCES <schema>.<table>`; FAIL if `<schema> != <own>`. Per-file
  scan is sufficient (DDL is self-contained per module — fact 8).
- Inline tests: for each `modules/<name>/src/*.rs` whose filename is NOT `tests.rs` and
  does NOT end in `_tests.rs`, FAIL on a line matching `mod\s+tests\s*\{` or
  `mod\s+\w+_tests\s*\{` (inline body). A `mod tests;` declaration passes. Do NOT key on
  `#[cfg(test)]`; a `#[path=...]` retarget line above `mod tests;` is fine (fact 9).
- Tests in `tools/archcheck/src/tests.rs`: a temp-dir fixture with a cross-schema
  `REFERENCES` → violation; a same-schema one → clean; a `mod tests { }` body →
  violation; a `mod tests;` decl + a `#[cfg(test)] fn test_x()` (webui shape) → clean.

**(b) Why now / order:** independent; grouped with Step 4 conceptually but split into
its own step because it uses the source-walk infrastructure (`grep_option_edge_server`)
rather than the cargo-metadata loop — two different mechanisms, cleaner as two diffs.

**(c) How:** copy `grep_option_edge_server`'s directory-walk skeleton
(`std::fs::read_dir` stack, `.rs` filter, comment-skip) and add the two line-matchers.
Reuse `workspace_root(meta).join("modules")`.
- **Cross-schema FK:** collect the file's own schema from `CREATE SCHEMA IF NOT EXISTS
  <s>;`. **Hard-assert exactly one `CREATE SCHEMA` per file and `<s>` == the
  `modules/<name>` path segment** (reviewer #8) — if a future module's schema name ≠ dir
  name or declares multiple schemas, FAIL loudly ("checker assumption violated") rather
  than silently mis-checking. Then flag any `REFERENCES <schema>.` where `<schema> != s`.
- **Inline tests — spell the str algorithm (don't hand-wave):** for a line containing
  `mod tests` / `mod <x>_tests`, take the substring after the module identifier,
  `trim_start()`, inspect the first byte — `;` = declaration (pass), `{` = inline body
  (FAIL); if the trimmed remainder is empty (brace on the NEXT line, rustfmt-legal), peek
  the next non-blank line's first non-ws char with the same rule. Handles `mod tests {`,
  `mod tests{`, and next-line-brace without a regex dep. **Decision:** if the
  `_tests`-suffix + next-line-brace handling turns gnarly in `str` ops, adding the
  `regex` crate to `tools/archcheck` for this one matcher is acceptable (it's a tool, not
  a foundation) — pick whichever is cleaner at implementation time, note it in the commit.

**(d) Dispatch:** `[sonnet]` — mirrors existing tripwire code; discriminators fully
specified.

---

### Step 6 — topiccheck durability-violation extension  `[opus]`
**(a) What:** `tools/topiccheck/src/main.rs` — add a second finding class: a DEFINED
(contract) topic that is subscribed on the in-process plane (plain `on()`), which for a
cross-module contract topic is a durability violation (constraints 3/7).

**(b) Why now / order:** independent of all prior steps; last of the checker steps
because it's the smallest and touches an already-wired advisory stage (no verify change
needed).

**(c) How (non-mechanical):** the harness already merges durable subs (from
`RecordingTransport`) and in-process subs (from `bus().subscribed_topics()`, merged
under the sentinel subscriber `"(in-process)"`) into `by_topic`
(`main.rs:154-189`). Every entry in `defined_topics()` IS a cross-module contract topic
(a `bus::define` static in an `api/*/events` crate). So the check is: for each
`defined` topic, if `by_topic.get(topic)` contains `"(in-process)"` → durability
violation. Emit it as a finding class DISTINCT from the existing UNSUBSCRIBED one, add a
column/marker to the printed table. ~15-20 lines; no `core/bus` change.

**Precision limit — acknowledge and mitigate (reviewer #3).** CLAUDE.md seam #3
explicitly PERMITS plain `on()` for *same-module* reactions. But `subscribed_topics()`
(`core/bus/src/lib.rs:142`) returns only topic strings with NO subscriber-module
attribution — every in-process sub collapses to the single `"(in-process)"` sentinel.
So this check cannot distinguish a legitimate same-module in-process subscription to the
module's OWN contract topic from a genuine cross-module violation. It is clean today
because ZERO in-process subs to any defined topic exist (verified). Two consequences the
plan commits to: (1) the rule's honest claim is the STRICTER *"no in-process subscription
to any defined (contract) topic, even your own"* — state it that way in the tool output,
not as "cross-module durability" which it can't actually prove; (2) add an
`ALLOW_INPROCESS_DEFINED: &[&str]` allowlist (mirroring the existing
`ALLOW_UNSUBSCRIBED`) so a future legitimate same-module reaction can be whitelisted
with a reason comment rather than being forced onto the durable plane. Without
subscriber attribution this allowlist is the only escape hatch — document why.

**Tier — this is a HARD constraint, so it must be BLOCKING (reviewer #2).** The existing
UNSUBSCRIBED finding is "dead vocabulary" → legitimately advisory. A durability
violation breaches CLAUDE.md seam #3 (constraints 3/7) → it must fail the DEFAULT
`verify.sh`, not only `--all --strict`. Mechanism (Step 3 wires it): add a
`--durability-strict` flag to topiccheck that runs the harness and exits non-zero ONLY
on a durability finding (ignoring unsubscribed), and invoke `topiccheck --
--durability-strict` inside the BLOCKING `fortress` stage. The full advisory table
(unsubscribed + durability, exit-gated by `--strict`) stays in the `--all` tier
unchanged. So topiccheck is invoked twice: blocking (durability only) + advisory (full).

**Fragility to note in a code comment:** the check keys off the `"(in-process)"` sentinel
string (`main.rs:185`) and the manual `defined_topics()` enumeration — both are
topiccheck's existing conscious edit points, so this rides the same maintenance surface,
not a new one. Add a unit test alongside the existing `unsubscribed` test asserting a
synthetic in-process-subscribed defined topic is flagged and that the allowlist
suppresses it.

**(d) Dispatch:** `[opus]` — durability semantics are subtle; small but
correctness-sensitive, separate-context review warranted.

---

### Step 7 (OPTIONAL) — automated cadence  `[sonnet]`
**(a) What:** there is NO CI and no git-hook precedent (`.git/hooks` is stock samples
only). Two non-exclusive options, pending user choice:
- `scripts/install-hooks.sh` + `.ps1` that install a `pre-push` hook running
  `./verify.sh --fast` (the hook body lives in a version-controlled `hooks/` dir and is
  copied into `.git/hooks/` by the installer, since `.git/hooks/` isn't tracked).
- A scheduled cloud agent (via `/schedule`) running `./verify.sh --all` nightly.

**(b) Why now / order:** last, optional, and gated on an explicit user decision — it's a
workflow/policy change, not a correctness net, and a pre-push hook that runs the full
BLOCKING tier can be disruptive on a fast-commit solo workflow. Do NOT implement without
the user picking an option.

**(c) How:** if pre-push — mirror `scripts/memory-sync.*` structure for the installer;
keep the hook fast (`--fast` only, ~build+clippy+test+fortress+split-proof). If
scheduled — a one-line `/schedule` routine; no repo change.

**(d) Dispatch:** `[sonnet]` if pre-push (mechanical script); the `/schedule` variant is
`[inline]` (a single tool call, nothing to hand off).

---

## Verification (per checker, before each commit)

- Step 1: `cargo test -p registry` (the new observer unit test); `cargo run -p archcheck`
  still clean (no new module edges).
- Step 2: `cargo run -p requirecheck` prints the table + exits 0 on the clean tree; a
  deliberately-broken local edit (add a stray `require` to a module without updating its
  `requires()`) makes `-- --strict` exit non-zero, then revert.
- Steps 4-6: `cargo test -p archcheck` / `-p topiccheck` (new fixture tests); run each
  tool on the clean tree → exits 0 (baseline PASS).
- Full: `./verify.sh --fast` green (requirecheck folded into fortress), then
  `./verify.sh --all` for the advisory topiccheck durability check.

## Commit plan
One commit per step (7 commits, Step 7 optional). Scopes:
`feat(registry): …` (S1), `feat(requirecheck): …` (S2), `test(verify): …` (S3),
`feat(archcheck): …` (S4, S5), `feat(topiccheck): …` (S6). Trailer per executing model
(opus steps → Opus 4.8, sonnet steps → Sonnet 4.6).

## Open decision for the user
- **Step 4 Rule B scope:** `metrics`-only (recommended, documented + passing) vs also
  requiring `messaging` with an `admin-svc`/`gateway-svc` exempt-list. Plan assumes
  metrics-only.
- **Step 7:** pre-push hook, scheduled agent, both, or skip.
