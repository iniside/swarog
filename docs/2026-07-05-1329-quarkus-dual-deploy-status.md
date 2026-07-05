# Status: quarkus-dual-deploy — monolit zweryfikowany, split zablokowany na projekcie messagingu

Branch `quarkus-dual-deploy`. Plan: `docs/plans/2026-07-05-1153-quarkus-dual-deploy-plan.md`.
Data: 2026-07-05 13:29.

## Co zrobione (Kroki 0–9 zacommitowane)

| Krok | Commit | Stan |
|---|---|---|
| 0 plan → repo | 15dba62 | ✅ |
| 1 multi-module Gradle | c1d5493 | ✅ build zielony |
| 2 RoleConfig + gating startupu | d7ca57b | ✅ |
| 3 wire-kontrakty + outbox/inbox | c88f0a0 | ✅ |
| 4 outbox relay + Reactive Messaging | b421fea | ✅ (monolit) |
| 5 gRPC sync seam | 2baa4d6 | ✅ build zielony |
| 6 admin fan-out (bez closure) | a11f160 | ✅ |
| 7 profile topologii | 640c824 | ⚠️ split nie bootuje (niżej) |
| 8 docker-compose (postgres+redpanda) | ac21842 | ✅ (nieuruchomione — brak Dockera) |
| 9 install.ps1 + drivery + health | 9f5aee6 | ✅ |
| 10 weryfikacja | d59e435 | ✅ monolit / ⛔ split |

## Krok 10 — weryfikacja

### Monolit: ZWERYFIKOWANY end-to-end ✅
Uruchomiony `./install.ps1 -Mode monolith` (lokalny Postgres na 5432, JDK 26). Potwierdzone:
- **Async seam:** outbox → per-moduł relay (`@Scheduled`) → kanał wewnętrzny (bez connectora) → consumer
  `@Incoming @Blocking @Transactional`. `characters.outbox` unsent = `0/4`, w logu granty i wipe.
- **Idempotencja:** inbox-dedup — każda postać dostała **dokładnie jeden** `starter_sword` mimo re-drenażu
  starych zdarzeń między runami (dedup zadziałał).
- **Sync seam:** `ownerOf` (gałąź local) autoryzuje ręczny grant (`healing_potion x3`).
- **Admin fan-out:** `/admin` renderuje Characters+Inventory (gałąź local).
- **Drivery + health:** `POST /characters?name=Verify` → 200, nowa postać dostała `starter_sword` przez
  ścieżkę zdarzeń; `/q/health/ready` = 200. 0 wyjątków.

### Bug złapany w runtime (build nie mógł) → naprawiony
`jackson-module-kotlin` nie był na classpath → relay padał na `readValue` Kotlinowej data-class
(`InvalidDefinitionException: no Creators`). Serializacja działała (getters), deserializacja nie. Dodano
zależność (Quarkus auto-rejestruje `KotlinModule`) — naprawia relay **i** deserializery Kafki. Commit d59e435.

### Microservices: ZABLOKOWANY ⛔ (dwie niezależne przeszkody)

**1. Brak Dockera w tym środowisku** → nie da się postawić Redpandy, więc pełny split runtime jest
nieuruchamialny tutaj (monolit wymaga tylko Postgresa — stąd dał się zweryfikować).

**2. Realna wada projektu messagingu (ważniejsza).** Boot procesu `%inventory` pada:
```
SRMSG00073: channel names cannot be used for both incoming and outgoing: [characters.created, characters.deleted]
```
**Przyczyna:** w JEDNYM artefakcie ten sam kanał (`characters.created`) ma Emitter (relay, moduł characters)
**i** `@Incoming` (consumer, moduł inventory). Obie klasy są na classpath w KAŻDYM procesie (single-artifact).
- Monolit: OK — Emitter→@Incoming to wewnętrzny drut bez connectorów (ta sama nazwa w obu kierunkach jest
  dozwolona *bez* connectorów).
- Split B (inventory): potrzebuje `@Incoming`=Kafka **oraz** zarejestrowanego Emittera. `enabled=false` na
  outgoing → psuje wstrzyknięcie Emittera; connector na outgoing → SRMSG00073 (ta sama nazwa incoming+outgoing
  z connectorami). **Żaden wariant nie bootuje.**

To nie literówka — to strukturalna niekompatybilność „single-artifact + współdzielony kanał producent/consumer"
z modelem SmallRye. Wyszła dopiero w runtime (ryzyko sygnalizowane przy bumpie Kroku 7 do [opus]).

## Opcje naprawy splitu (decyzja użytkownika)

- **A — Osobne nazwy kanałów producent/consumer + most w monolicie.** Relay→`X.out`, consumer←`X.in`,
  procesor `@Incoming(X.out) @Outgoing(X.in)` wiąże je in-process (monolit), wyłączony per-profil w splicie;
  idle-Kafka na `X.out` w B rejestruje Emitter. Poprawne, ale gadatliwe (2 mosty + wiele `enabled=false`).
- **B — Abstrakcja Bus; consumery z powrotem na CDI `@ObservesAsync` (in-process), most Kafka tylko w splicie.**
  Relay publikuje przez port `EventPublisher`: monolit = zdarzenie CDI; split = surowy producent Kafki (bez
  `@Channel`, brak problemu wstrzyknięcia). W procesie konsumującym jeden Kafka-listener re-emituje zdarzenie
  CDI (wyłączony w monolicie). Czystsze koncepcyjnie, ale wraca ścieżka CDI-event.
- **C — Zawsze Kafka (nawet „monolit").** Najprostszy kod, ale monolit potrzebuje brokera → zabija footprint
  „JVM + Postgres", który jest sensem całości. Odrzucić.
- **D — Osobne cienkie app-shelle per serwis (zamiast single-artifact+ROLES).** `characters-service` = nowy
  moduł app zależny od {accounts, characters}; `inventory-service` = {inventory, admin}. Osobne artefakty →
  brak kolizji classpath → kanały są naturalnie jednokierunkowe per artefakt. **To jest dokładnie ekstrakcja,
  pod którą robiony był multi-module split w Kroku 1** — Gradle już to wspiera. Porzuca premisę „jeden artefakt,
  N ról", ale jest najbliższy realnym mikroserwisom i sygnałowo najczystszy.

**Rekomendacja:** jeśli trzymamy premisę single-artifact+ROLES → **A**. Jeśli dopuszczamy pivot → **D**
(najczystsze, a struktura modułów już gotowa). Pełna weryfikacja KAŻDEJ opcji wymaga Dockera/Redpandy.

## Następny krok
Decyzja: A / B / D dla splitu (+ udostępnienie Dockera do weryfikacji). Monolit jest gotowy i działa.
