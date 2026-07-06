# Testing the Quarkus sketch — coverage map, conventions, and the scripted smoke

Durable reference for `experiments/jvm-quarkus-sketch/` (branch `quarkus-per-service`). Written after the
behavioral-coverage rollout that took the sketch from ~2 domain tests to a full suite at **90.9% aggregate line
coverage** (Kover), gated at **85%** in `check`. Plan: `docs/plans/2026-07-06-1226-quarkus-test-coverage-plan.md`.

## How the DB tests work (READ THIS before adding a test)

- **DB = local `jvmsketch` Postgres** (`gamebackend`/`gamebackend`, `jdbc:postgresql://localhost:5432/jvmsketch`).
  **NO Docker / Testcontainers / Dev Services** — this repo does not use them; integration tests need a running
  local Postgres, which is the correct target for an integration test (see [[local-postgres-is-the-test-db]]).
- **`app/src/test/resources/application.properties`**: `quarkus.http.test-port=8090` (so the outbox relay's default
  subscriber URL lands on the test's own server — exercises the REAL fanout) + `app.seed.enabled=false` (Seed
  `TRUNCATE`s every table on monolith boot — must stay off in test or it wipes the dev DB).
- **Where tests live:** cross-module `@QuarkusTest` (needs the full wired monolith) → `app/src/test/kotlin/domain/`
  (only `app` aggregates every impl). Pure-unit → the owning module's own `src/test`.
- **Isolation contract (non-negotiable):** create your own rows, delete them in `@AfterEach` **by the ids you
  created** — NEVER `TRUNCATE`. `awaitOutboxDrained(id)` before delete. jsonb cleanup uses
  `payload->>'characterId' = ?` (NOT `LIKE '%...%'` — the whitespace bug). Async assertions poll via `awaitTrue`
  (5s), never assert-immediately. The shared DB is **cumulative** across `@TestProfile` boots → **assert only
  id-scoped rows or behavior, NEVER schema presence/absence or global counts.** Disable the scheduler
  (`SchedulerDisabledProfile`) when you don't want background fanout racing your cleanup.
- **Strict flags + detekt apply to test code** (no swallowed/over-generic exceptions, no `!!`, no unused imports).
- **Substitution:** `@InjectMock` (quarkus-junit5-mockito) can replace a `@Produces @ApplicationScoped
  PlayerCharacters` AND one bean in an `@All List<AdminDataProvider>` (both proven). Pure-unit fakes: implement the
  interface directly, or use a `java.lang.reflect.Proxy` that throws on any call to prove a code path is I/O-free
  (`InventoryAuthorizationUnitTest`).

## Coverage map (what's tested, by tier)

- **P0 — dual-deploy safety + error seams:** role-gating BEHAVIOR (relay `drain()` short-circuits when its module is
  inactive; `CharactersEdgeServer.start()` monolith-skip; `migrate()` early-returns — all via faked `RoleConfig` +
  untouchable-`DataSource` proxies, NOT schema assertions); `InventoryResource.grant` HTTP mapping 200/400/**503**/
  default item-qty; admin graceful degradation (peer down → error card **200 not 500**).
- **P0.5 — regressions for this rollout's own history:** Seed-off honored under test; ArC gate flags resolve boolean
  `true` at runtime (so a re-added inline `.properties` comment fails a test, not just SRCFG01008 in a log); the
  outbox jsonb predicate matches where the old `LIKE` did not.
- **P1 — module behavioral:** accounts (register atomicity via a real forced-rollback; `PlayerRegistered` JSON
  round-trip; zero-subscriber drain marks-sent; non-2xx → retry via a loopback stub server); characters (create
  flush→id→outbox; delete-nonexistent no-op; `ownerOf` null-for-unknown; admin-data shape); inventory (same-item
  grant accumulation; `onCharacterDeleted` redelivery dedup; **firstSeen tx-rollback coupling** via same-tx doom;
  PLAYER-owner authz-skip; holdings ordering; wipe count; admin-data KPIs); characters-client (`ownerOf` happy/
  reconnect-once/`CharactersUnavailableException`/`addSuppressed`, via the injected `connect` seam + a loopback
  fake); admin slug (golden-master then the Go-matching rule).
- **P2 — edge/codec pure-unit:** codec round-trips + malformed frames; router dispatch/errors/overwrite; client
  duplicate-cid/timeout/`nextPush`/`call()`-on-error; `CachedResource` sequential (create-throws-retries, close-
  throws-still-clears); `CallbackRegistry`; `Upcalls.Target` swallow-Throwable→INTERNAL_ERROR; `FrameReassembler`
  split/coalesce/corrupt-length; `MsQuicServerTransport` ctor thumbprint `require`.
- **P3 — breadth:** REST endpoints (`/events/*` incl. the fault→500-not-swallowed path, `/characters`,
  `/admin-data/*`); migration idempotency + **zero cross-module FKs** asserted from `information_schema`; admin
  local-vs-remote DTO parity (in-JVM; a true two-topology comparison needs the process split — see smoke below).

**Every crown assertion was proven to BITE** (mutate production → test RED → revert) during the rollout.

## Bugs the coverage effort FOUND and FIXED
Writing the tests surfaced real defects (green build ≠ correct): malformed Basic header → 500 (now 401);
`CharactersResource` malformed playerId → 500 (now 400); `RoleConfig` case-sensitive `ROLES=ALL` activated nothing
(now normalized); admin slug kept `/`/punctuation → broke `/admin/{slug}` routing (now matches Go's `[a-z0-9]`
allowlist); `Page.err` dead branch (removed); `InventoryAdminData.recentRows` misnomer (renamed). **Concurrency
(fixed with a dedicated race-safe change, `1538c7a`):** `EdgeClient` didn't fail in-flight calls on connection death
(hung until timeout) — fixed with a lock-based terminal-state transition whose mutual-exclusion happens-before
argument closes the add-after-drain race (a lock-free `@Volatile`+re-check was analyzed and REJECTED as having a
genuine JMM hole: CHM weakly-consistent iteration + no HB on read-before-write); `EdgeServer`/`EdgeClient` readers
silently died on a malformed frame — now log-and-continue (isolated frames, DoS-safe).

## Gates
- **Kover** 0.9.8, aggregate report (the root merge is the only one that attributes cross-module `@QuarkusTest`
  coverage — per-module reports under-count because behavioral tests live in `app`). `koverVerify` floored at 85% in
  `check`. Excludes: `*.*Dto`/`*.*Payload`/`*edge.msquic.*` (native FFM)/`app.Seed` (dev-only).
- **PITest** — wired (`info.solidsoft.pitest` 1.19.0 + junit5 1.2.3) on `platform` but **NOT in `check`**: PIT's
  bundled ASM rejects JDK-26 class files (`Unsupported class file major version 70`) — a hard bytecode-version
  check, unfixable by the detekt-style `java.version` spoof. Deferred until PIT ships ASM 9.9+; run manually via
  `./gradlew :platform:pitest` under an older JDK if needed.
- **detekt** + **Konsist** + the **Jandex split-verification** + the **arch-rules Quarkus extension** — the
  architecture/static layer (see [[quarkus-compile-time-verification]]).

## Not unit-testable — the scripted smoke (run manually, don't fake as a unit test)
These need real processes/native/network and belong in a manual checklist, not `@QuarkusTest`:
- **2-process split:** `./gradlew` build then `install.ps1 -Mode microservices` → `characters-service` (:8080 +
  QUIC :9100) + `inventory-service` (:8081). Verify: `POST :8080/characters` grants a starter cross-process; owned
  grant 200 / unowned 400 / **A killed → 503** (not false 400); admin fan-out renders A's card, degrades to an error
  card when A is down. This is the real transport-transparent-seam + dual-deploy proof (the in-JVM admin-parity test
  only compares local-vs-own-endpoint, not a real network hop).
- **Real QUIC negatives** (extend `MsQuicEchoTest`, cert-gated): connect-to-nothing timeout, ALPN mismatch,
  shutdown-before-CONNECTED. `MsQuicEchoTest` is `assumeTrue`-SKIPPED without `edge.test.cert-thumbprint` — a green
  CI run without the cert means SKIPPED, not passed.
- **Native send-failure** buffer-free branch (needs native fault injection — inherently untestable in-JVM).
