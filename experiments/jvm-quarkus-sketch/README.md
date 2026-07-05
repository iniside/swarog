# jvm-quarkus-sketch

The **same modular-monolith reference case** as `../jvm-kotlin-sketch` (accounts + characters +
inventory + admin), rebuilt on **Quarkus** — same Kotlin 2.4.0, same JDK 26, same Postgres, same
admin UI. Built to answer the follow-up question: *what happens to the hand-built seams when a
container that already ships them takes over?*

Short answer: **`core/` (~180 lines) disappears entirely — and most of what it knew turns out
to be unnecessary here, *because the architecture's own constraints already guarantee it*.**

## Seam-by-seam: what each one became

| Seam (framework-free sketch)                     | Here (Quarkus/CDI)                                    | Verdict |
|--------------------------------------------------|--------------------------------------------------------|---------|
| `core.Registry` — modules declare `dependsOn`, DFS **topo-sort**, cycles/missing deps fail loudly | `@ApplicationScoped` beans, discovered not listed. SYNC ordering = the injection graph (cycles still fail at build time). Migrations run at the **default** observer priority in ANY order — they're order-free *by construction*, since the architecture forbids cross-module FKs/schemas. The only real ordering need (demo seed after all migrations) is one late `@Priority` on the seed, knowing nothing about the modules. | **Dissolved.** The registry's ordering knowledge wasn't replaced — it was revealed as redundant: the isolation constraints make module startup commutative. |
| `Context.provide/require` — single-value service registry | plain constructor injection by type (`characters` implements `PlayerCharacters`, `inventory` injects it) | **Cleaner.** The seam dissolves into the language; nominal-typing note from the sketch still applies (`charactersapi` package). |
| `core.Bus` + `Topic<T>` — async fire-and-forget, virtual threads | CDI events: `Event<T>.fireAsync` → `@ObservesAsync T`; the payload **class is the topic** | **Equivalent**, minus one tool: no `awaitIdle`/drain hook, so the demo seed must poll the DB for the handlers' effects. |
| `Context.contribute(Slot)` / `contributions` — multi-value, **contribution-ordered** | contributor `@Produces` an `adminapi.Item` bean; admin injects `@All List<Item>` (container-defined order) and sorts by (section, label) itself | **Equivalent, order moved to the right owner:** modules must not care when they start, so presentation order can't come from contribution order — the renderer imposes it. |
| `admin` runs its own JDK `HttpServer` (`Starter`/`Stopper`) | a `@Path("/admin")` resource; container owns the HTTP lifecycle; `theme.css` served from `META-INF/resources` automatically | **Less code, less ownership.** FreeMarker → Qute is a 1:1 template port. |
| `Db.fromEnv()` + PGSimpleDataSource               | `application.properties` + injected Agroal-pooled `DataSource` | **Better.** Real pool, config-driven; comment out the URL and **Dev Services** boots a throwaway Postgres in Docker for `quarkusDev`. |
| `app/Main.kt` — "the ONLY place that lists modules" | gone; CDI discovers beans. `app/` is just the demo seed | **Double-edged:** one less file to touch per module (Open/Closed gets *stronger*), but the system's wiring is no longer written down anywhere — you read it back from annotations. |

Migrations, schemas, plain-column refs (no cross-module FKs), the event-driven deletion cleanup,
and the admin contract (`adminapi`) are **unchanged** — the architecture survived the transplant.

## Persistence: Panache everywhere (the sibling sketch is the raw-JDBC control group)

All three DB modules use **Panache Kotlin** (Hibernate ORM underneath) over entities
(`Player`, `Character`, `Holding`); the raw-JDBC version of the same code lives in
`../jvm-kotlin-sketch` for line-by-line comparison. Findings from the conversion:

- The DML one-liners are real: `Character.count()`, `Character.findById(id)?.playerId` (that's
  the entire `ownerOf` capability), `Holding.delete("id.ownerType = ?1 …")`, and the SQL upsert
  (`ON CONFLICT`) became load-modify — mutate the managed entity, dirty checking writes the
  UPDATE, no save call anywhere.
- **The line cut tracks the table shape.** `characters` (surrogate BIGSERIAL id — the
  ORM-friendly shape): 117 → 89+34, with every query gone. `inventory` (composite PK): 160 →
  131+40, barely moved — the `@Embeddable` id class, `@Transactional` on write paths, and one
  query that stayed native (count-distinct over the key prefix) ate the winnings. What improves
  everywhere is *what the lines say* — domain intent instead of statement/result-set plumbing.
- **Transactions × the bus:** the JDBC version emitted events after its autocommitted write.
  With declarative `@Transactional`, `fireAsync` inside the method would leak the event BEFORE
  commit (and a rollback would leave a phantom event) — so the emitting modules run their writes
  in a programmatic `QuarkusTransaction.requiringNew()` block and fire after it returns. The
  ORM made event-vs-transaction ordering an explicit decision that autocommit used to hide.
- **The architecture rule the ORM tests:** Hibernate would happily map a `@ManyToOne` association
  ACROSS module boundaries — a temptation raw SQL never offered. `Character.playerId` and
  `Holding.ownerId` stay plain columns on purpose; the no-cross-module-FK constraint now needs
  discipline at the entity level too (ArchUnit's impl-privacy rule catches the import of another
  module's entity class).
- Mapping details the DDL forced: BIGSERIAL → `@GeneratedValue(IDENTITY)` (not `PanacheEntity`,
  whose default expects a Hibernate-named sequence); `created_at DEFAULT now()` →
  `insertable = false` so the DB keeps filling it.
- Migrations stay raw DDL owned by the module; Hibernate schema management is off. The ORM maps
  the schema, it doesn't own it.

## What Quarkus charges for this

- **The seams are no longer yours.** Registry/bus/slot semantics are ArC's; when you need a
  property they don't have (a drain-on-shutdown hook, an `awaitIdle`) you encode it *around*
  the container, not in it.
- `allOpen` compiler plugin (CDI proxies need open classes) — the first framework-shaped dent
  in the Kotlin.
- Build: ~2s incremental, but the first build downloads half of Maven Central; the jar tree is
  `build/quarkus-app/` (a lib directory, not one fat jar) unless you opt into uber-jar/native.
- On Windows, a running instance **locks the jars** — stop it before rebuilding.

## What Quarkus pays back

- `accounts`/`characters`/`inventory` shrank to *almost pure domain code* — wiring is annotations.
- Dev mode: `gradlew quarkusDev` = live reload on save + Dev Services (auto-Postgres via Docker).
- The platform on-ramp is now config, not code: `quarkus-oidc` (the Epic verifier as an extension),
  Flyway, health checks, metrics, GraalVM native image — none used here, all one dependency away.

## Enforced boundaries (unchanged on purpose)

`src/test/kotlin/architecture/ArchitectureTest.kt` runs the **same ArchUnit rules** as the
framework-free sketch — module impls reachable only from `app`, no cycles. The constraints
outlive the framework swap; that they're *needed* is also unchanged — everything still compiles
into one deployment unit where any class can import any other. (They also pay a second dividend
here: no-cross-module-FKs is exactly what makes startup order-independent.)

## Build & run

Same toolchain as the sibling sketch: Gradle 9.6.1 wrapper, Kotlin **2.4.0** (the exact version
Quarkus 3.37.1 ships in its BOM — every difference between the two projects is the framework,
not the language), JDK 26 toolchain, `jvmTarget = JVM_26`.

Uses the same dedicated `jvmsketch` database as the sibling (run one sketch at a time — each
truncates and reseeds the demo data at boot). If you don't have it yet:

```sql
CREATE DATABASE jvmsketch OWNER gamebackend;
```

```bash
./gradlew quarkusDev    # dev mode: live reload; or ./gradlew build && java -jar build/quarkus-app/quarkus-run.jar
```

(or `run.cmd` on Windows — finds the JDK and starts dev mode.)

Then open **http://localhost:8090/admin**. Expected console:

```
[accounts] schema ready
[characters] schema ready
[inventory] schema ready
  [inventory] granted starter_sword to character ...   (x3, via events)
  [inventory] wiped 1 holding(s) for deleted character ...
seeded: 3 characters created, 1 deleted (its holdings cleaned via event)
admin on http://localhost:8090/admin  (OPEN -- set ADMIN_USER/ADMIN_PASS to gate)
```

Gate with `ADMIN_USER`/`ADMIN_PASS` (HTTP Basic, hand-rolled for parity with the sketch — the
idiomatic move would be `quarkus-elytron-security-properties-file`).
