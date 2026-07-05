# Plan: jvm-quarkus-sketch → dual-deploy (monolit ↔ mikroserwisy) sterowany `install.ps1`

> **Kanoniczna kopia w repo** (źródło prawdy). Zatwierdzona 2026-07-05 11:53.
>
> **Status:** przeszedł grumpy-review (Opus, think hard) + re-research messaging seam. Wszystkie blocker-y
> (B1–B4) i should-fix-y (S1–S8) wchłonięte poniżej; log rozwiązań na końcu. Krok 0 (persist) ✅.

## Context

`experiments/jvm-quarkus-sketch/` to modularny monolit (accounts + characters + inventory + admin) na
Quarkus 3.37.1 / Kotlin 2.4.0 / JDK 26 / Postgres. Użytkownik skłania się ku **promocji tego sketcha na
main project**. North-star #2 („extractable to microservices") wymaga, by **jeden build** dał się deployować
albo jako jeden proces (monolit), albo jako N procesów (mikroserwisy) — różniących się tylko konfiguracją,
sterowane jednym `install.ps1 -Mode monolith|microservices`.

**Problem:** sketch zoptymalizował monolit *rozpuszczając* dwa seamy w CDI, a to dokładnie te, których
podział na procesy potrzebuje z powrotem jako transport-transparent:

- **Async bus = zdarzenia CDI** (`Event<T>.fireAsync` → `@ObservesAsync`) — *ściśle in-process*
  (`accounts/Accounts.kt:44`, `characters/Characters.kt:70,77`, `inventory/Inventory.kt:66,72`).
- **Sync = wstrzyknięcie po typie** — `InventoryModule(private val characters: PlayerCharacters)` z
  `CharactersModule : PlayerCharacters` (`inventory/Inventory.kt:42`, `characters/Characters.kt:28,85`).
- **Admin = domknięcie + `@All`** — `adminapi.Item.render: () -> SectionData` (niesеrializowalne)
  agregowane przez `@All List<Item>` (`admin/Admin.kt:30`) — fundamentalnie in-process.
- Brak mechanizmu `roles` — migracje (`@Observes StartupEvent`) i Seed odpalają bezwarunkowo.

**Decyzje użytkownika:** (1) multi-module Gradle **teraz**, (2) sync przez **gRPC**, (3) **production-grade**
(transactional outbox, DLQ, idempotentne konsumenty, admin fan-out, readiness), (4) reviewer: think hard.

**Intended outcome:** ten sam artefakt (fast-jar) uruchamiany 1× (`ROLES=all`) lub N× (podzbiory ról),
z internal-channel↔Kafka i local-bean↔gRPC przełączanymi configiem; `install.ps1` staje oba topologie
end-to-end i weryfikuje zdrowie.

## Research — czemu NIE nowy moduł / czemu tak (zweryfikowane)

- **Nie własny bus** — SmallRye Reactive Messaging: monolit = **brak connectora** (kanał wewnątrz JVM, wymaga
  OBU końców w procesie), rozproszenie = connector Kafka. `in-memory` connector jest *tylko testowy*.
- **Bramkowanie kanałów per-rola** — `mp.messaging.incoming|outgoing.<ch>.enabled` **istnieje jako runtime
  property** (od Quarkus 2.2, issue #19318/PR #19461; SmallRye „does not register disabled channels"). Wybór
  profilu w runtime: `QUARKUS_PROFILE=<rola> java -jar …` — wszystkie profile zapieczone w jednym jarze.
  To jest single-artifact mechanizm. **In-method `if`-guard NIE wystarcza** — kanał Kafka i tak dołączyłby do
  consumer-group i cicho ackował-gubił wiadomości.
- **Nie własny registry** — `@Produces` czytający `RoleConfig` w runtime = jedyny single-artifact local↔remote.
  Wszystkie `@IfBuild*` odpadają (build-time → dwa artefakty).
- **Nie własny discovery** — Stork (static) integruje się z `@GrpcClient`.
- **Transactional outbox** zamiast fire-after-commit; relay **per-moduł** (Emitter jest statycznie związany z
  nazwą kanału w build-time — jeden generyczny relay niemożliwy).

## Docelowa architektura

- **Build:** multi-module Gradle; `io.quarkus` tylko na `app`; każdy nie-app moduł ma `META-INF/beans.xml`
  (dyskrecja CDI w Gradle multi-module — bez tego beany w subprojektach niewidoczne; `quarkus.index-dependency`
  bywa zawodne, `beans.xml` najpewniejsze).
- **Dwie osie gatingu:**
  - `RoleConfig` (`@ConfigProperty(name="roles") Set<String>`, env `ROLES`) → bramkuje **startup/migracje** i
    gałąź produced-bean local/remote (logika w runtime, w beanie).
  - `QUARKUS_PROFILE=<rola>` (mapowane z `ROLES` w `install.ps1`) → bramkuje **konfigurację**: `enabled`
    kanałów, connector Kafka vs brak, serwer gRPC, endpointy admin-data. Base/monolit profile = wszystko ON,
    brak connectorów (kanały wewnętrzne); role-profile = ON tylko końce, które ta rola posiada, + connector Kafka.
- **Bus:** per-event kanał; publish przez **per-moduł** outbox-relay → `MutinyEmitter` (jeden `@Channel` na
  topik); consumer `@Incoming @Blocking @Transactional`, idempotentny (inbox-table), DLQ (tylko Kafka-profile).
- **Sync:** `characters.proto` → `@GrpcService` (serwer, w characters, gated profilem) + `@GrpcClient`
  (klient); **jeden `@Produces PlayerCharacters` w impl `characters`** zwraca local-delegate albo gRPC-adapter
  wg `RoleConfig`; Stork static.
- **Admin:** `Item.render` closure → serializowalny `AdminDataProvider`; każdy feature-moduł wystawia
  admin-data (REST/JSON); admin fan-outuje do listy `admin.modules` — local bean lub REST-client via Stork,
  per-provider try/catch → error-card gdy zdalny padł.
- **Deploy:** `install.ps1 -Mode` buduje raz, stawia Postgres (+Redpanda w split), odpala 1/N procesów z
  `ROLES`+`QUARKUS_PROFILE`+portami+bootstrap+Stork, health-check `/q/health/ready`, sprząta.

## Topologia modułów Gradle (Krok 1)

```
settings.gradle.kts:
  include("accounts-events")
  include("characters-events", "characters-api")
  include("inventory-events")            # gdy inventory zacznie publikować
  include("admin-api")                   # Item/SectionData/Kpi/Table/Cell + AdminDataProvider
  include("platform")                    # RoleConfig + Outbox row-model + mark-sent helper (NIE relay)
  include("accounts", "characters", "inventory", "admin")   # impl (beans.xml; allopen/jpa gdzie @Entity)
  include("app")                         # JEDYNY io.quarkus; quarkusBuild; Seed dev-only
```
**Strzałki (wszystkie acykliczne):** `app → {accounts,characters,inventory,admin}` (+ kontrakty których używa
wprost); `inventory → characters-api, inventory-events, admin-api, platform`;
`characters → characters-api, characters-events, admin-api, platform`;
`accounts → accounts-events, platform`; `admin → admin-api, platform`; kontrakty (`*-api`,`*-events`,
`admin-api`) i `platform` zależą od niczego feature-owego. **`characters → inventory`(impl) NIGDY.**
Boundary jest teraz **fizyczny** (compile-time). ArchUnit zostaje w `app` jako defense-in-depth:
no-cycles między slice'ami, zakaz importu cudzej encji JPA, oraz reguła rozróżniająca klasę impl
(`*Module`/`AdminResource`) od pakietów `*api`/`*events` po ich **realnych** nazwach (impl są top-level:
`inventory.InventoryModule`, **nie** `inventory.impl`).

## Rozszerzenia (build.gradle.kts `app` + odpowiednie moduły)

`quarkus-messaging-kafka`, `quarkus-grpc`, `quarkus-smallrye-stork` + `-service-discovery-static`,
`quarkus-smallrye-health`, `quarkus-scheduler` (outbox relay), `quarkus-jackson`, `quarkus-rest-client-jackson`
(admin fan-out). Później/opcjonalnie: `quarkus-container-image-jib`, native.

## Mapa role→proces w trybie split (dla Kroków 7/9)

| Proces | `QUARKUS_PROFILE` | `ROLES` | HTTP | gRPC | Co robi |
|---|---|---|---|---|---|
| A „characters" | `characters` | `accounts,characters` | 8090 | 9090¹ | migracje accounts+characters; **serwer gRPC** ownerOf; **producent** Kafka (registered/created/deleted); admin-data REST dla characters |
| B „inventory" | `inventory` | `inventory,admin` | 8091 | — | migracje inventory; **konsument** Kafka created/deleted; **klient gRPC** ownerOf → A; admin fan-out (local inventory + remote characters via `stork://characters-service`) |

¹ gRPC może dzielić port HTTP (unified server) — jeśli `quarkus.grpc.server.use-separate-server=false`, osobny
port zbędny; ustalić w Kroku 5. Monolit = 1 proces, `ROLES=all`, brak profilu (base), port 8090.

---

## Sekwencja implementacji

### Krok 0 — Persist plan do repo `[inline]`
- **(a)** `docs/plans/2026-07-05-HHMM-quarkus-dual-deploy-plan.md` z treścią tego planu (realne HHMM).
- **(b)** Pierwsze — repo = źródło prawdy (MANDATORY); reszta się odwołuje.

### Krok 1 — Multi-module Gradle skeleton `[opus]`
- **(a)** `settings.gradle.kts` (include jw.); per-moduł `build.gradle.kts`; przenieś pakiety do subprojektów;
  `META-INF/beans.xml` do KAŻDEGO nie-app modułu; `io.quarkus`+Panache/REST/Qute na `app`; `allOpen`/`jpa`
  plugin tylko na impl-modułach z `@Entity`. Zaktualizuj `ArchitectureTest.kt` (patrz strzałki wyżej) i przenieś
  do `app`.
- **(b)** Najpierw — reszta ląduje w nowym layoucie; compile-time boundary przed dołożeniem transportu.
- **(c)** Pułapka Jandex: `beans.xml`, nie `index-dependency`. Kontrakty (`*-api`,`*-events`) bez adnotacji
  Quarkus; `admin-api` z DTO (Krok 6). Weryfikacja: `./gradlew build` widzi wszystkie beany.
- **(d)** `[opus]` — widoczność beanów correctness-critical.

### Krok 2 — `RoleConfig` + bramkowanie startupu `[opus]`
- **(a)** W `platform`: `@ApplicationScoped class RoleConfig(@ConfigProperty(name="roles",
  defaultValue="all") roles: Set<String>) { fun isActive(m:String)= "all" in roles || m in roles }`.
  Wstrzyknij do każdego `*Module`; `if (!roleConfig.isActive("<mod>")) return` na starcie każdej
  `migrate(@Observes StartupEvent)`. `Seed.seed` gated do `isActive("all")`.
- **(b)** Przed splitem — per-proces aktywacja migracji; monolit dalej działa `ROLES=all`.
- **(c)** `Set<String>` z comma-string konwertuje auto; env `ROLES`→`roles` auto-relaxed. **Bramkowanie
  kanałów/serwera gRPC/endpointów NIE tu** — to profil (Krok 7). `RoleConfig` = tylko startup + gałąź
  produced-bean (Krok 5).
- **(d)** `[opus]`.
- *(Footnote native, poza zakresem: `quarkus.arc.remove-unused-beans` mógłby usunąć bean widoczny tylko
  dynamicznie; beany mają realne `@Observes`/`@Incoming`/`@Path`/`@GrpcService`, więc reachable.)*

### Krok 3 — Payloady zdarzeń → wire-kontrakty + tabele outbox/inbox `[opus]`
- **(a)** W `*-events`: data-class płaskie (są), stała topiku per event (`const val TOPIC="characters.created"`),
  `@RegisterForReflection`. ObjectMapper `FAIL_ON_UNKNOWN_PROPERTIES=false` (reguła #6 = additywność). W migracji
  każdego **publikującego** modułu: `CREATE TABLE <schema>.outbox(id bigserial pk, topic text, payload jsonb,
  created_at, sent_at timestamptz null)`. W migracji każdego **konsumującego** modułu: `CREATE TABLE
  <schema>.inbox(event_id text primary key, processed_at)` — deduplikacja (S1).
- **(b)** Transport, outbox i idempotencja zależą od serializowalnych payloadów i storage.
- **(c)** `jackson-module-kotlin` (via `quarkus-jackson`). Nazwa topiku jawna (topic-by-type z CDI znika).
- **(d)** `[opus]`.

### Krok 4 — Outbox (per-moduł) + Reactive Messaging zamiast `fireAsync` `[opus]`
- **(a) Publikacja:** w każdym publikującym module — **usuń `QuarkusTransaction.requiringNew()`**; w JEDNEJ
  `@Transactional` z zapisem domenowym, po `persist()`/flush (żeby BIGSERIAL id był dostępny — `Characters.create`
  `ch.id!!`), wstaw wiersz outbox (topic+payload-json). Usuń stary komentarz o fire-after-commit.
  **Per-moduł relay:** klasa w impl modułu (nie w `platform`) z `@Channel("<topic>") MutinyEmitter` **per topik**
  i `@Scheduled` pollerem po `sent_at IS NULL` własnego schema → `emitter.send()`, `sent_at` po ukończeniu,
  retry przy błędzie (`platform` daje tylko row-model + mark-sent SQL helper).
- **(a) Konsumpcja:** `@ObservesAsync` → `@Incoming(TOPIC) @Blocking @Transactional`. Idempotencja: w tej tx
  `INSERT INTO inbox(event_id) ON CONFLICT DO NOTHING`; jeśli 0 wierszy → już przetworzone, `return`.
  Bez tego `grant` (`qty += qty`) **podwaja** startery przy redelivery — **nie jest** idempotentny sam z siebie.
- **(a) Consumer-less event (`PlayerRegistered`):** nikt go dziś nie konsumuje. W monolicie (brak connectora)
  kanał wyjściowy bez konsumenta = **boot fail SRMSG00019**. Rozwiązanie: **nie** twórz kanału/relay dla
  `accounts.registered` w tej iteracji (zostaw zapis do outbox jako log/future), ALBO dodaj no-op
  `@Incoming("accounts.registered")` sink w `accounts`. Wybór: **no-op sink** (production-grade — topik istnieje
  dla przyszłych konsumentów; w split bindowany do Kafki).
- **(b)** Rdzeń async seamu; zależy od Kroku 3.
- **(c)** Relay emituje do TEGO SAMEGO kanału w obu trybach: monolit = brak connectora (dostawa wewnątrz JVM,
  oba końce w procesie), split = Kafka. Bramkowanie końców per-rola = `enabled` w profilu (Krok 7), nie `if`.
  DLQ (`failure-strategy=dead-letter-queue`) **tylko** w Kafka-profile (connectorless go odrzuca) — S5. Ordering:
  klucz Kafka = id encji.
- **(d)** `[opus]` — tx/outbox/idempotencja/ordering correctness-critical.

### Krok 5 — Sync seam: gRPC + produkowany local/remote adapter w impl `characters` `[opus]`
- **(a)** `characters/src/main/proto/characters.proto`: `rpc OwnerOf(OwnerOfRequest{character_id})
  returns(OwnerOfReply{found, owner_id})`. Wydziel `ownerOf` z `CharactersModule` do
  `@ApplicationScoped class LocalPlayerCharacters` (konkret, **nie** wystawia `PlayerCharacters`).
  `@GrpcService class PlayerCharactersGrpcService` deleguje do `LocalPlayerCharacters`. **Jeden `@Produces
  PlayerCharacters` w impl `characters`** (nie platform/inventory — inaczej cykl/impl-on-impl): `if
  (roleConfig.isActive("characters")) LocalPlayerCharacters-delegate else GrpcPlayerCharactersAdapter(stub)`.
  `CharactersModule` przestaje `: PlayerCharacters`. `inventory/Inventory.kt:42,104` (`@Inject
  PlayerCharacters`, `characters.ownerOf`) — **bajt-identyczne**.
- **(b)** Drugi seam; zależy od Kroku 1/2.
- **(c) Ambiguity:** dokładnie JEDEN bean typu `PlayerCharacters` (produkowany w characters, zawsze na
  classpath) — w każdej kombinacji ról (proces z inventory+characters, i proces tylko inventory). **Bridging
  Uni→sync:** `.await().indefinitely()` rzuca na event-loopie → oznacz `@Blocking` metodę zasobu REST wołającą
  `inventory.add` (dziś woła tylko `Seed` na zwykłym wątku — latentne, ale realny endpoint tego wymaga) — S3.
  **Nieużywany klient w monolicie:** `@GrpcClient` trzymany w `GrpcPlayerCharactersAdapter`, adapter tworzony
  tylko w gałęzi remote; kanał gRPC leniwy (tworzony przy 1. wywołaniu) → w monolicie inertny; podaj inert
  `quarkus.grpc.clients.characters.host/port` w base-profile żeby wstrzyknięcie się powiodło — S4.
  **Serwer gRPC** gated profilem (start tylko w `characters`) — Krok 7. Stork:
  `quarkus.grpc.clients.characters.host=stork://characters-service` + `…name-resolver=stork`;
  `quarkus.stork.characters-service.service-discovery.type=static`. Ustal `use-separate-server` (N3) — jeśli
  false, brak osobnego portu gRPC.
- **(d)** `[opus]` — API design + pułapki ambiguity/bridging.

### Krok 6 — Admin fan-out (usunięcie closure) `[opus]`
- **(a)** W `admin-api`: `SectionData/Kpi/Table/Cell` → czyste serializowalne DTO; **usuń** `Item.render:
  () -> SectionData`; wprowadź `interface AdminDataProvider { val id:String; val section:String; val label:String;
  fun data(): SectionData }`. Każdy feature-moduł: (i) bean `AdminDataProvider`, (ii) `@Path("/admin-data/<mod>")`
  GET JSON zwracające `data()`. `AdminResource` fan-outuje wg configu `admin.modules=characters,inventory`: dla
  `roleConfig.isActive(mod)` → local bean; wpp `@RestClient` via `stork://<mod>-service`; **per-provider
  try/catch** → error-card gdy zdalny padł/boot; sortuj (section,label), renderuj Qute jak dziś.
- **(b)** Admin to jedyny seam czysto in-process; production-grade wymaga fan-outu.
- **(c)** ownerOf = gRPC, ale admin-data = **REST/JSON** świadomie (bogaty display-payload, blisko `Item`, admin
  i tak HTTP; proto dla `SectionData` = przerost). Discovery: statyczna lista `admin.modules` + `stork://`.
  Endpoint `/admin-data/<mod>` gated profilem (Krok 7) — jest tylko w procesie hostującym moduł.
- **(d)** `[opus]` — redesign kontraktu.

### Krok 7 — Profile konfiguracji topologii `[sonnet]`
- **(a)** `application.properties` **base** = monolit: `roles=all`, **bez** connectorów (kanały wewnętrzne),
  wszystkie `enabled` domyślnie true, inert `quarkus.grpc.clients.characters.host/port`, port 8090.
  **`%characters.`**: connector Kafka na kanałach outgoing (registered/created/deleted) + `enabled=true` tylko
  dla nich; incoming created/deleted `enabled=false`; serwer gRPC on. **`%inventory.`**: connector Kafka na
  incoming created/deleted `enabled=true`; outgoing `enabled=false`; klient gRPC `stork://characters-service`;
  serwer gRPC off; `admin.modules=characters,inventory`. Wspólne: `kafka.bootstrap.servers`, serializery
  (`ObjectMapperSerializer` / subclass `ObjectMapperDeserializer<T>`), datasource, `quarkus.stork.*` static,
  DLQ `failure-strategy=dead-letter-queue` tylko w Kafka-profilach. Udokumentuj ENV: `ROLES`, `QUARKUS_PROFILE`,
  `QUARKUS_HTTP_PORT`, `KAFKA_BOOTSTRAP_SERVERS`, `QUARKUS_STORK_<SVC>_SERVICE_DISCOVERY_ADDRESS_LIST`,
  `DATABASE_URL`.
- **(b)** Przed `install.ps1` — procesy potrzebują tych kluczy/profili.
- **(c)** `mp.messaging.*.enabled` = runtime (od 2.2); profil wybierany env `QUARKUS_PROFILE`. Disabled kanał nie
  rejestruje się → nie dołącza do consumer-group. `QUARKUS_PROFILE` mapowane z `ROLES` w `install.ps1`.
- **(d)** `[sonnet]` — config w pełni wyspecyfikowany.

### Krok 8 — `infra/docker-compose.yml` `[sonnet]`
- **(a)** `postgres` (zawsze) + `redpanda` (Kafka-API, bez ZooKeeper; profil compose `microservices`).
- **(b)** `install.ps1` na tym stoi.
- **(d)** `[sonnet]`.

### Krok 9 — `install.ps1` `[opus]`
- **(a)** `param(-Mode monolith|microservices, -SkipBuild, -SkipInfra, -Teardown, -DatabaseUrl, -BasePort)`.
  `$topology` = mapa role→proces (tabela wyżej: name/profile/roles/httpPort/grpcPort/storkPeers). Fazy:
  **Build** (`gradlew quarkusBuild` → jeden `build/quarkus-app/quarkus-run.jar`), **Infra** (compose `up postgres`
  [+`redpanda` w split], `Wait-ForTcp` 5432 [+9092]), **Launch** (monolit: 1 proc `ROLES=all`; split: foreach
  `$topology` — ustaw `$env:ROLES/$env:QUARKUS_PROFILE/$env:QUARKUS_HTTP_PORT/$env:KAFKA_BOOTSTRAP_SERVERS/
  $env:QUARKUS_STORK_*` PRZED każdym `Start-Process java -jar … -PassThru`, PID→`run/pids.json`), **Readiness**
  (`GET /q/health/ready` retry/timeout per proces; w split najpierw `Wait-ForTcp 9092`), **Teardown**
  (`-Teardown`: stop PID-y z `run/pids.json`, `compose down`; Postgres zostaje dev-DB).
- **(b)** Ostatni — komponuje wszystko.
- **(c)** PS7 primary: env-passing = ustaw `$env:*` sekwencyjnie przed `Start-Process` (dziedziczone), reset po.
  `finally`/trap = sprzątanie half-started. **Port** JDK-discovery z `run.cmd` do PowerShell (skan
  `"$env:USERPROFILE\.jdks\*26*"`) — przepisać, nie „reużyć" (batch≠PS).
- **(d)** `[opus]` — lifecycle/health/teardown correctness.

### Krok 10 — Weryfikacja `[opus]` (patrz niżej)

---

## Verification (end-to-end)

1. **Monolit:** `./install.ps1 -Mode monolith` → `http://localhost:8090/admin` renderuje Characters+Inventory;
   konsola: seed flow (3 startery przez zdarzenia wewnętrzne, 1 wipe). `SELECT count(*) FROM characters.outbox
   WHERE sent_at IS NULL` = 0. Redeliver ręcznie ten sam event → brak podwojenia (inbox).
2. **Mikroserwisy:** `./install.ps1 -Mode microservices` → proc A (8090) + B (8091). Utwórz postać przez API A
   → log B „granted starter_sword" (Kafka/Redpanda). `inventory.add` w B → autoryzacja **gRPC** ownerOf do A
   (zabij A → `add` faluje kontrolowanie). `/admin` w B fan-outuje REST do A (characters card) + local (inventory);
   zabij A → characters card pokazuje error, inventory dalej działa. DLQ topik pusty; consumer-group tylko B.
3. **Testy:** ArchUnit (slice no-cycles, no cross-module entity import, impl-privacy) w `app`; integracyjny test
   outbox-relay (wiersz→emit→sent_at) i inbox-dedup (podwójny event → jeden grant); test ról (`ROLES=inventory`
   nie tworzy schematu `characters`).
4. **Build gate:** `./gradlew build` (wszystkie moduły, Jandex widzi beany).

## Log rozwiązań reviewera (blocking/should-fix)

- **B1** relay per-moduł (Emitter statyczny per topik), `platform` tylko row-model/helper — Krok 4.
- **B2** `PlayerRegistered` bez konsumenta = SRMSG00019 → no-op sink — Krok 4(a).
- **B3** gating kanałów przez `mp.messaging.*.enabled` w profilu (runtime, potwierdzone) zamiast in-method `if`;
  `QUARKUS_PROFILE` z `ROLES` — Kroki 2/7.
- **B4** `@Produces PlayerCharacters` w impl `characters` (bez cyklu/impl-on-impl) — Krok 5.
- **S1** inbox-table dedup (grant nie jest idempotentny) — Kroki 3/4.
- **S2** usunięcie `requiringNew()`, outbox w tej samej tx po flush — Krok 4.
- **S3** `@Blocking` na zasobie wołającym gRPC — Krok 5.
- **S4** inert gRPC-client config + leniwy kanał w monolicie — Krok 5.
- **S5** DLQ tylko w Kafka-profilach — Kroki 4/7.
- **S6** `admin.modules` + per-provider try/catch/error-card — Krok 6.
- **S7** mapa role→proces (accounts w A, admin w B, porty) — sekcja + Kroki 7/9.
- **S8** `$env:*`-then-`Start-Process`, port JDK-scan do PS — Krok 9.
- **N1–N4** strzałki uzupełnione; impl top-level (nie `.impl`); `use-separate-server`/`name-resolver=stork`
  do ustalenia; native-note jako footnote — Kroki 1/2/5.

## Ryzyka / świadome cięcia

- **Jandex w Gradle multi-module** — mitygacja `beans.xml` per moduł.
- **Outbox relay** = prosty `@Scheduled` poller, nie Debezium CDC — proporcjonalne; do rewizji przy skali.
- **`use-separate-server`** — jeśli gRPC dzieli port HTTP, `$topology.grpcPort` zbędne (ustalić w Kroku 5).
- **Native** poza zakresem (osobno; audyt reflection dla payloadów Kafka/gRPC).
```
