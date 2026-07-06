# Plan: behavioral test coverage for the Quarkus sketch

> Branch `quarkus-per-service` (`experiments/jvm-quarkus-sketch/`). Goal: close the behavioral-test gap.
> Directive: comprehensive, "żeby niespodzianek nie było" — the writing subagent gets exact files + assertions.
> Tests written by **[opus]** subagents. Adds **Kover** (coverage report → later a gate) + **PITest** (mutation,
> scoped, best-effort). Backed by 10 research subagents.
>
> **v2 — reworked after a grumpy-reviewer pass (think-hard).** The review killed four things: P0-ROLES asserted
> schema-absence on a shared CREATE-IF-NOT-EXISTS DB (unobservable → now behavioral); the Kover gate was wired
> before the tests existed (→ report-first, gate-last); seam #1's default factory leaked a native transport per
> reconnect (→ retain transport, inject only `connect`); admin auth reads `System.getenv` not config (→ extract to
> a pure fn). Concurrency bug-fixes were pulled OUT into a separate follow-up. Regression tests for THIS session's
> own three bugs were added. Full punch-list resolution at the bottom.

## Context — what exists, and the false-comfort map (MANDATORY: don't re-cover)

**Existing tests (14) — what they ACTUALLY bite:** the 2 `@QuarkusTest`+DB domain tests (create→grant, delete→wipe
no-orphans; redelivery dedup + authz reject) **bite hard**; `InventoryAuthorizationUnitTest` (proxy fakes) proves
the DB-free authz short-circuit; `RoleConfigTest` bites the boolean only; `EdgeRemotePlayerCharactersTest` covers 3
parse cases; `EdgeLoopbackTest` + `MsQuicFoundationTest` bite; `CachedResourceModelCheckingTest` (Lincheck) bites.
**False-comfort:** `MsQuicEchoTest` is `assumeTrue`-SKIPPED without a cert (zero CI signal); `LincheckSmokeTest` is
framework-only; `ArchitectureTest`/`KonsistArchitectureTest` are structural-only (self-admittedly redundant with
the Gradle graph) — **do NOT count as behavioral coverage**. **Zero tests: `accounts`, `admin`, and `characters`'
own invariants** (only exercised as setup by inventory tests).

**Test-infra invariants the writer MUST honor** (non-negotiable, learned the hard way this session):
- DB = local `jvmsketch` Postgres (`gamebackend`/`gamebackend`). **NO Docker/Testcontainers/Dev Services.**
- `app/src/test/resources/application.properties` keeps `quarkus.http.test-port=8090` (relay self-fanout) +
  `app.seed.enabled=false` (Seed TRUNCATEs — must stay off).
- Cross-module `@QuarkusTest` → `app/src/test/kotlin/domain/`; pure-unit → owning module's `src/test`.
- Isolation: create-own-rows + `@AfterEach` delete-by-id; **never TRUNCATE**; `awaitOutboxDrained(id)` before delete;
  jsonb cleanup `payload->>'characterId' = ?` (NOT `LIKE`); async via `awaitTrue` poll (5s). `Thread.sleep` in plain
  helpers is fine.
- Strict flags + detekt apply to test code.
- **The shared DB is cumulative across `@TestProfile` boots** (seed off, per-row cleanup only) — so **no test may
  assert on schema presence/absence or global row counts**; assert only rows scoped to ids it created, or behavior.

**Deps to ADD:** `io.rest-assured:rest-assured`, `io.quarkus:quarkus-junit5-mockito` (to `app`, `admin`);
`junit-jupiter`+`junit-platform-launcher` to `accounts`/`characters`/`admin` (match existing pattern).

## Testability seams (production refactors — additive, PROVEN behavior-preserving)

1. **`EdgeRemotePlayerCharacters` — inject the `connect` boundary, NOT a transport factory.** Today a single
   `private val transport = MsQuicClientTransport()` is created once and reused across reconnects. **Do NOT** default
   to `{ h,p -> MsQuicClientTransport().connect(h,p) }` (that builds a new native registration per reconnect — a
   leak, NOT byte-identical). Instead: keep the single retained `transport` field; add a constructor param
   `connect: (String, Int) -> EdgeConnection = transport::connect` (bound to the retained field). Tests pass a fake.
   Production path = one transport, reused — identical. **[opus]**, verify the reused-transport semantics unchanged.
2. **Admin slug extraction — take the WHOLE ordered list, not per-label.** `resolve()`'s dedupe is stateful across
   the sorted items (`seen`/counter). Extract a pure `fun slugify(items: List<...>): List<...>` that consumes the
   already-sorted list and returns slugged items — NOT a per-label function (per-label silently changes dedupe).
   **[opus]**. Land golden-master tests against CURRENT output BEFORE any rule change (see §Bugs #5 / Step 4c).
3. **Admin Basic-auth decode extraction (for §Bugs #1).** `unauthorized()` reads `System.getenv("ADMIN_USER")`
   (not `@ConfigProperty`), so the auth gate can't be toggled by `@TestProfile` and its malformed-header branch is
   unreachable in-JVM. Extract the header parse+validate (`decode Basic → user:pass → compare`) into a pure function
   taking `(authHeader: String?, expectedUser: String?, expectedPass: String?)`. Unit-test it (incl. malformed
   base64 → reject, not crash). The env-read stays a thin wrapper. **[opus]**.

## Coverage matrix — tiered by value

### P0 — dual-deploy safety (BEHAVIORAL gating) + the error seams
- **P0-ROLES — role-gating BEHAVIOR, not schema state** (reworked). Do NOT assert schema presence/absence (shared
  DB, CREATE-IF-NOT-EXISTS, no DROP → unobservable). Assert the gated side-effects behaviorally:
  - `CharactersOutboxRelay`/`AccountsOutboxRelay` under a scoped profile that excludes their module: inject the relay
    bean, call `drain()` directly, assert it **short-circuits** (no rows marked sent, no HTTP) — i.e. the `isActive`
    gate is honored. Use a relay-visible signal (e.g. a row it should NOT touch stays `sent_at NULL`), scoped to an
    id the test created, never a global count.
  - `CharactersEdgeServer.start()` monolith-skip: `isMonolith()==true` → returns before touching cert/transport
    (assert no exception with NO cert set, and `transport` stays null — may need a package-visible getter).
  - Migrate-gating: rather than schema-absence, assert the gate is CONSULTED — a pure/near-pure test that
    `InventoryModule.migrate` with a faked `RoleConfig` where `isActive("inventory")==false` performs no DDL (fake
    the `DataSource` to throw on use, proving the early-return, à la `InventoryAuthorizationUnitTest`'s proxy trick).
  - **Minimize `@TestProfile` count** (each = a full Quarkus reboot): prefer the bean-level/pure tests above over
    profile-per-role reboots. Enumerate the profiles actually used and justify each.
- **P0-GRANT-REST — `InventoryResource` HTTP mapping** (`@QuarkusTest`+rest-assured): 200 (owned), 400 (`-1` →
  IllegalStateException), **503** (throwing `PlayerCharacters`), default item/qty. **Gated on the QuarkusMock smoke
  (Step 3a) proving a `@Produces @ApplicationScoped PlayerCharacters` can actually be substituted** — if it can't,
  fall back to a pure-unit test of `InventoryResource.grant` constructed directly with a fake `InventoryModule`.
- **P0-ADMIN-DEGRADE — graceful degradation** (`@QuarkusTest`+rest-assured): remote fetch to a DOWN peer (point
  `admin.<id>.url` at a dead port via config) → `/admin` renders an error card, **200 not 500**. The remote branch
  is config-drivable (unlike auth). The local-provider-throws variant depends on substituting ONE bean in the
  `@All List<AdminDataProvider>` — **prove that's possible in the Step 3a smoke; if not, use a test-scoped
  `@Alternative` throwing provider** rather than QuarkusMock.

### P0.5 — regression tests for THIS session's own three bugs (cheap, high-signal)
The plan must guard the regressions we just fixed, or it's under-aimed:
- **Seed-off-under-test**: a test asserting the `Seed` observer no-ops under test config (e.g. after boot, a marker
  it would create is absent / `app.seed.enabled` is false and honored) — so a future re-enable can't silently wipe
  the dev DB again.
- **ArC flags actually ACTIVE**: assert the `quarkus.arc.detect-wrong-annotations` /
  `fail-on-intercepted-private-method` values resolve to boolean `true` at runtime (`@ConfigProperty` or
  `ConfigProvider`) — so re-adding an inline `.properties` comment (which silently parsed them to false) fails a
  test, not just SRCFG01008 in a log.
- **jsonb cleanup predicate**: a tiny test proving `payload->>'characterId' = '<id>'` matches a real serialized
  outbox row while the old `LIKE '%"characterId":<id>%'` does NOT (the space bug) — locks the fix.

### P1 — module behavioral gaps
- **P1-ACCOUNTS** (`accounts/src/test` + one `@QuarkusTest`): `register` writes 1 player + 1 outbox row atomically
  (force a failure → rollback leaves neither); `PlayerRegistered` JSON round-trip; zero-subscriber `drain()` marks
  sent immediately (no HTTP); non-2xx → `sent_at` stays NULL + retried (loopback stub server).
- **P1-CHARACTERS** (`characters/src/test`+DB): `create` flush→id→outbox `characterId` matches returned id;
  `delete(nonexistent)` silent no-op; `LocalPlayerCharacters.ownerOf` UUID/known, **null**/unknown (never throws);
  `CharactersAdminData.data()` KPI/table shape + `id DESC` top-10.
- **P1-INVENTORY-GAPS**: same-item repeated `grant` accumulates qty; `onCharacterDeleted` redelivery dedup; `firstSeen`
  rollback coupling (force `grant` throw in-tx → inbox row NOT left behind); PLAYER-owner `add` (authz-skip path);
  `holdings` ordering deliberate (reverse-insert → alpha); `wipe` count; `InventoryAdminData` KPIs (`distinctOwners`
  native SQL, 20-cap).
- **P1-CLIENT** (`characters-client/src/test`, seam #1): `ownerOf` happy path, reconnect-once-then-succeed,
  double-failure → `CharactersUnavailableException` (NOT null), `addSuppressed` preserves first exception; parse
  edges (non-numeric port → `NumberFormatException`, negative port, IPv6-ish `::1:9100`).
- **P1-ADMIN-SLUG** (`admin/src/test`, seam #2): golden-master of CURRENT slug output FIRST (lowercase, space→`-`,
  empty→`item`, collision→`-2`/`-3`, sort `(section,label)`). Rule CHANGE is a SEPARATE step (§Bugs #5).

### P2 — edge/codec pure-unit (cheap, zero native)
`EdgeCodec`/`Frame` round-trips + malformed (REQUEST-missing-method, PUSH-missing-topic, garbage bytes, type
mismatch); `EdgeRouter` isolation (register/dispatch/unknown-method/handler-throws incl. null-message/overwrite);
`EdgeClient` duplicate-cid, timeout branch, `nextPush` timeout→null, `call()` throw-on-`ok=false` incl. null-error;
`CachedResource` **create-throws-then-retries** + **close-throws-but-cache-still-clears** (SKIP the get-twice-same-
instance tautology — Lincheck covers it); `CallbackRegistry`, `Upcalls.Target.invoke` (swallow-Throwable→
INTERNAL_ERROR), `FrameReassembler` (split/coalesce/partial-tail + corrupt-length), `MsQuicServerTransport` ctor
thumbprint `require`.

### P3 — breadth (optional, cut for time)
REST tests for `InventoryEventSink`/`CharactersResource`/`*AdminDataResource`; migration idempotency (call twice, no
throw; `information_schema` cross-schema FK = 0 — this ONE schema-shape check is OK because it's an existence-of-FK,
not absence-of-schema); local-vs-remote admin parity.

### Not unit-testable (documented scripted smoke, don't fake)
`install.ps1 -Mode microservices` 2-process split; real-QUIC connect-timeout/ALPN/shutdown (extend `MsQuicEchoTest`
under the cert gate); native send-failure. → a `docs/` smoke checklist.

## §Bugs surfaced — fix-vs-lock (decide at approval)
**Fix (as part of their test's step):** #1 admin malformed-Basic → currently 500, should be 401 (via seam #3 pure
fn); #5 admin slug `/`-in-label breaks `/admin/{slug}` (Kotlin only replaces space, unlike Go's `[a-z0-9]`) — change
rule in P1-ADMIN-SLUG AFTER golden-master. **Lock+document** (test pins current behavior + TODO): #4 `CharactersResource`
malformed playerId → 500 (low stakes); #6 `RoleConfig` `ROLES=ALL` activates nothing (case-sensitive — pin it so ops
aren't surprised, or normalize — user picks); #7 admin `Page.err` dead branch; #8 `InventoryAdminData.recentRows`
misnomer.

**MOVED OUT of this plan (separate bug-fix change, own review):** the `EdgeClient` connection-death-doesn't-unblock-
pending fix and the `EdgeServer`/reader unguarded-`codec.decode` fix are **concurrency changes** — bundling them into
"write tests" is unsafe (the naive "complete pending on reader exit" races with `requestWithCid` inserting after the
drain — the exact hang it meant to kill). P2 LOCKS current behavior (a call after close hangs until its timeout) with
a documented TODO; the fix is a follow-up plan with proper Lincheck-style review.

---

## Implementation sequence

### Step 1 — infra deps + Kover REPORTING (no gate yet) `[sonnet]`
Add the test deps (above). Wire **Kover 0.9.8** (root `apply false` + reactive `plugins.withId(...)`, like detekt) —
**reporting only, NO `koverVerify` in `check`** (accounts/characters/admin have zero tests; a gate here red-lights the
build before Steps 2–5 land). Exclude `*.*Dto`/`*.*Payload`/`*edge.msquic.*`. **Smoke the real risk:** run
`:inventory:koverHtmlReport`, confirm `@QuarkusTest` classes show NON-zero coverage (Quarkus classloading has broken
JaCoCo before). Decide per-module vs aggregate report. **Verify:** `./gradlew check` still green (no gate); a coverage
report generates.

### Step 2 — the three testability seams `[opus]`, think hard
Seam #1 (retained-transport + injected `connect`), seam #2 (whole-list slug extraction), seam #3 (Basic-auth pure fn).
All additive; production paths PROVEN identical (one transport reused; slug output byte-identical; auth wrapper same).
**Verify:** `./gradlew build` green; existing domain + edge tests still pass unchanged.

### Step 3 — P0 + P0.5 `[opus]`, think hard
- **3a (smoke FIRST):** prove QuarkusMock can substitute a `@Produces @ApplicationScoped PlayerCharacters`, AND
  whether one bean in `@All List<AdminDataProvider>` can be replaced. Throwaway test. **If either fails, switch that
  case to the fallback** (pure-unit `InventoryResource.grant`; test-scoped `@Alternative` provider) BEFORE writing the
  suite. Report which path each took.
- **3b:** P0-ROLES (behavioral gating, minimal profiles), P0-GRANT-REST, P0-ADMIN-DEGRADE, P0.5 regression trio,
  §Bugs #1 (401 fix via seam #3). **Verify:** each test RED when its invariant is mutated (re-widen the InventoryResource
  catch → 503/500 test fails; re-comment an ArC flag → flags-active test fails; restore); DB delta-zero; `check` green.

### Step 4 — P1 module behavioral `[opus]`, think hard (3 batches: 4a accounts+characters, 4b inventory-gaps+client, 4c admin-slug)
Batch to bound each subagent + limit `build.gradle.kts` contention. 4c does golden-master slug FIRST, THEN the §Bugs
#5 rule change with updated expectations (two separable commits). §Bugs #4/#6 lock-or-fix per the approved decision.
**Verify:** per-batch `:<module>:test` green + deliberate-break per crown assertion; DB delta-zero.

### Step 5 — P2 edge/codec pure-unit `[opus]` (pattern-heavy; could be `[sonnet]`)
The edge pure-unit suite. LOCKS current connection-death/decode behavior with a TODO (fix deferred — §Bugs moved-out).
**Verify:** `:edge:test` green; deliberate-break on the codec/CachedResource assertions.

### Step 6 — Kover GATE + PITest (best-effort) `[sonnet]`, think hard
NOW (tests exist) add `koverVerify` to `check` at a floor measured from the actual post-Step-5 coverage (set the floor
just below current, e.g. if inventory is 70% set 65% — a ratchet, not aspirational). Then **PITest, best-effort,
capped:** wire `info.solidsoft.pitest` 1.19.0 + junit5-plugin 1.2.3 scoped ONLY to pure-logic modules (`platform`,
`characters-client` parse, `edge` codec, `inventory` authz-via-fakes) — NEVER `@QuarkusTest`/DB (issue #1287). **Expect
JDK26 ASM failure** (pitest 1.19.1 predates ASM 9.9/V26 — same class as detekt): **attempt once with the detekt-style
`java.version` spoof / a JDK-21 launcher for the pitest task; hard 30-min cap; if it won't run, STOP and report** —
PITest is a bonus, Kover + the suite are the deliverable. Skip arcmutate (commercial). **Verify:** `koverVerify` gates;
PITest either produces a report or is documented as deferred-on-JDK26.

### Step 7 — coverage doc + per-tier commits `[inline]`
`docs/reference/testing.md`: the coverage map + the scripted-smoke checklist for the not-unit-testable behaviors +
the moved-out concurrency-bug follow-up. Commit per tier (infra/seams/P0/P1-batches/P2/gate separately).

## Risks / conscious tradeoffs
- **Shared cumulative DB** — no schema/global-count assertions (only id-scoped rows + behavior); the reason P0-ROLES
  went behavioral. Every integration test needs id-scoped `@AfterEach` + drain-before-delete.
- **`@TestProfile` reboots are costly** — prefer bean-level/pure gating tests over profile-per-role; enumerate profiles.
- **QuarkusMock substitution unproven** — Step 3a smokes it before the suite depends on it; fallbacks named.
- **Kover×Quarkus classloading** — smoked in Step 1; gate deferred to Step 6 so it never blocks test-writing.
- **PITest×JDK26 likely fails** — capped, best-effort, droppable.
- **Concurrency bugs deferred** — §Bugs #2/#3 out to a separate reviewed change; P2 locks current behavior, doesn't
  fix it inline.
- **Scope** (~40 cases): P0+P0.5+P1 is the target; P2 cheap breadth; P3 optional. Batches keep tasks bounded.

## Dispatch summary (for approval)
Step 1 `[sonnet]` · Step 2 `[opus]` · Step 3 `[opus]` (3a smoke then 3b) · Step 4 `[opus]` ×3 batches · Step 5
`[opus]`/`[sonnet]` · Step 6 `[sonnet]` · Step 7 `[inline]`. Commit per tier; DB delta-zero + deliberate-break on every
crown assertion. **User confirms at approval:** the §Bugs fix-vs-lock split, and that the EdgeClient/EdgeServer
concurrency fixes are DEFERRED to a separate plan.

## APPROVED (2026-07-06)
Scope = **P0..P3 (everything)**. Bugs = **fix everything found**, INCLUDING the EdgeClient connection-death +
EdgeServer/reader unguarded-decode concurrency fixes — but those get a DEDICATED, carefully-reviewed step (not
smuggled into test-writing), with the add-after-drain race explicitly handled and a Lincheck/interleaving check.
The debatable §Bugs (#4 playerId→400, #6 RoleConfig normalize, #7 remove Page.err) are FIXED, not locked.

## Grumpy-reviewer punch-list resolution
- **#1 (BLOCKER) schema-absence unobservable** → P0-ROLES reworked to behavioral gating (relay short-circuit, edge
  early-return, migrate-gate via fake DataSource); "no schema/count assertions" made a hard infra invariant.
- **#2 (BLOCKER) Kover gate before tests** → Step 1 is report-only; the `koverVerify` gate moves to Step 6 after tests,
  floored just below measured coverage.
- **#3 (BLOCKER) seam #1 leaks transport** → retain the single transport, inject only `connect = transport::connect`.
- **#4 (BLOCKER) admin auth unreachable** → seam #3 extracts the Basic-auth decode to a pure fn; §Bugs #1 tested there.
- **#5 (SHOULD) profile reboots / no cross-profile cleanup** → prefer bean/pure tests; enumerate+justify profiles;
  schema state documented cumulative.
- **#6 (SHOULD) QuarkusMock unproven** → Step 3a smoke-proves both substitutions before the suite; fallbacks named.
- **#7 (SHOULD) concurrency fix smuggled in** → §Bugs #2/#3 moved OUT to a separate reviewed plan; P2 locks behavior.
- **#8 (SHOULD) doesn't test this session's bugs** → new P0.5 tier (seed-off, ArC-flags-active, jsonb-predicate).
- **#9 (SHOULD) slug extract then change** → whole-list extraction; golden-master current output first, rule change
  separate (4c).
- **#10 (NIT) relay-gating on shared DB** → assert the gate/short-circuit, not row counts (folded into P0-ROLES).
- **#11 (NIT) tautologies + PITest waste** → cut get-twice-same-instance; PITest capped best-effort 30-min.
