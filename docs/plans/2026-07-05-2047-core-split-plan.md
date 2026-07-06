# Plan: rozbicie `core/` na 4 pakiety + kill topoSort + typed registry

> **Nowy plik w repo (na zatwierdzeniu):** `docs/plans/2026-07-05-2047-core-split-plan.md`.
> Branch `core-split` off `go-parity`. Refaktor rdzenia — **upraszczający**, nie rozdymający.

## Context

`core/` trzyma **cztery różne koncepty** pod jedną nazwą (event bus, service-registry-jako-discovery, multi-value
slot, orkiestracja cyklu życia + Context) plus przeciążoną nazwę `Registry` (raz moduły, raz serwisy) — stąd
gubienie się. Dodatkowo ustaliliśmy dwie rzeczy: (1) **`DependsOn`/topoSort to smell** — jeśli izolacja jest
prawdziwa, kolejność startu jest przemienna (potwierdzone: żaden `Init` nie używa Require'owanego serwisu podczas
Init); (2) `Provide/Require` zwraca `any`+panic — nie-go-idiomatyczne. **Cel:** rozbić core na 4 skupione pakiety,
wywalić topoSort (two-phase), utypować Provide/Require generykami. Runtime behavior **bez zmian** — to czysty
refactor (weryfikowany regresją monolit+split z go-parity).

## Docelowa struktura (4 pakiety, Context w lifecycle)

```
bus/        Define[T]/Emit[T]/On[T], Bus, NewBus, Event, Handler, EventType   (z core/bus.go, package bus). Liść.
registry/   Registry (services map) + generyczne Provide[T]/Require[T] (string-keyed). Liść.
contrib/    Slots + Contribute/Contributions (multi-value slot). Liść.
lifecycle/  Module/Migrator/Starter/Stopper/Registrar interfejsy + App (orkiestracja: Add/Build/Migrate/Start/Stop,
            two-phase, BEZ topoSort) + Context (agreguje *bus.Bus/*registry.Registry/*contrib.Slots/Mux/DB/Log) +
            NewContext. Importuje bus+registry+contrib. Nic wewnętrznego nie importuje lifecycle poza cmd+modułami.
```
Graf importów (acykliczny): `moduły → lifecycle → {bus, registry, contrib} → (nic)`. `edge/`, `outbox/` bez zmian
(nie importują core). Przeciążenie rozwiązane: dawny `core.Registry` (orkiestracja) → **`lifecycle.App`**; serwisy
→ **`registry.Registry`**.

## Dwie zmiany semantyczne

### A. Kill topoSort (NIE DependsOn) — two-phase przez opcjonalny `Registrar`
**Kluczowe (B1/B2):** `DependsOn` **NIE jest tylko sortem** — `cmd/server/main.go planModules:112` czyta go, by
policzyć `needed` (które stuby stworzyć w splicie). Usunięcie zabiłoby ROLES-split (który Krok 2 weryfikuje). Więc:
- **Ginie TYLKO sortowanie (topoSort).** Deklaracja zależności ZOSTAJE — **przemianowana `DependsOn()` →
  `Requires() []string`** (nazwy serwisów, które moduł Require'uje). To wprost rozdziela „czego potrzebuję" od
  „w jakiej kolejności startuję" (Twój zarzut). `Requires()` napędza: (1) stub-planning w main.go, (2) walidację
  startową. **NIE napędza kolejności startu.**
- Two-phase przez OPCJONALNY `lifecycle.Registrar { Register(ctx *Context) error }` (idiom jak Migrator/Starter):
  **faza 1** — każdy `Registrar.Register` (konstrukcja providowanego serwisu + `registry.Provide`); **faza 2** —
  każdy `Init` (Require + routes + bus-subs + Contribute). Oba w kolejności rejestracji, ZERO sortu.
- **Providerzy (5) i ich Register (S3 — dokładnie co przenosimy):** accounts/characters/inventory — `Register`
  buduje `m.store` (potrzebuje tylko `ctx.DB`, dostępne w fazie 1) + `m.svc` + Provide; `Init` konsumuje `m.svc`
  do edge-handlerów + Require deps + routes. **rating (B3): value-receiver bez pól → zmień na POINTER receiver z
  polem `svc *Service`**; `Register` buduje+Provide+stash `svc`; `Init` subskrybuje `On` używając `m.svc`; **+
  `cmd/server/main.go:43` `rating.Module{}` → `&rating.Module{}`**. remote.Stub — `s.client` gotowy z `NewStub`,
  Register trywialny.
- **Fail-loud (S4):** `Require[T]` robi comma-ok lookup PRZED asercją → zachowuje `required service %q not found`;
  brakujący dep panikuje przy pierwszym Require w fazie 2 (start), nie w runtime.
- **Use-sites bez zmian** — `m.accounts` zostaje płaskim interfejsem (fix vs lazy-Require, który zmieniałby ~6-10
  use-sites na `.Get()`).
- **Stop (S5):** two-phase Stop = odwrotna kolejność REJESTRACJI (nie zależności). Żaden moduł nie używa depa w
  Stop (characters.Stop tylko zamyka edge-server) → behawior zachowany, ale gwarancja „reverse-DEPENDENCY"
  relaksowana do „reverse-registration" — świadome, udokumentowane (nie „identyczne").

### B. Typed Provide/Require (generyki, string-keyed) — jedyna wykonalna forma
```go
// registry/registry.go
func Provide[T any](r *Registry, name string, svc T)          // T inferowane; pod spodem services[name]=any(svc)
func Require[T any](r *Registry, name string) T               // services[name].(T); panic na brak/niedopasowanie
```
- Type-keyed odpada (mapa po typie nie zrobi structural lookup: provider daje `*service`, konsument chce wąski
  `charactersSvc` — różne `reflect.Type`). String-key zostaje; generyk przenosi asercję z inline `.(T)` do funkcji.
- **Uczciwie:** lookup+asercja dalej runtime (panic zostaje) — to win stylu/spójności z bus (`Define/Emit/On` już
  są package-generic), centralizacja komunikatu i „Require[charactersSvc]" jako intencja, NIE compile-time DI
  (prawdziwe compile-DI = constructor injection, którego świadomie nie robimy — psułoby late-binding w kształcie
  k8s-discovery, który uratował remote-stub w splicie).
- Call-sites: 4 Require → `registry.Require[T](ctx.Registry, "name")`; 5 Provide → `registry.Provide(ctx.Registry,
  "name", svc)`. `Contribute/Contributions` zostają jako **forwardujące metody na Context** (nie generyczne) →
  te 5 sites bez zmian. `core.Emit/On/Define` → `bus.Emit/On/Define`. `ctx.Bus/Mux/DB/Log` → bez zmian (pola).

## Powierzchnia (z researchu — dokładna)

- **Import `gamebackend/core`:** cmd/server/main.go, wszystkie 8 modułów (+ ich *events packages dla `Define`),
  modules/remote, testy (core/registry_test.go, modules/admin/admin_test.go, admin_fanout_test.go, accounts_test.go).
  `edge/`, `outbox/`, `modules/*/store.go`, adminapi, characters/admin.go, inventory/admin.go — NIE importują core.
- **Provide (5):** accounts.go:124, characters.go:92, inventory.go:109, rating.go:26, remote.go:63 (dynamiczny name;
  `Provide[any]` — `s.client any` trzyma konkretny typ, `any(any)` nie double-boxuje, `Require[charactersSvc]`
  asertuje czysto — N2 potwierdzone).
- **Require (4):** characters.go:85 (`accountsSvc`), inventory.go:84 (`accountsSvc`), inventory.go:85
  (`charactersSvc`), match.go:23 (`ratingService`).
- **Contribute (4 prod):** accounts:135, characters:93, inventory:110, remote:67. **Contributions (1):** admin:171.
- **`DependsOn`→`Requires` (rename, NIE usunięcie):** characters `["accounts"]`, inventory `["accounts","characters"]`,
  match `["rating"]`, reszta `nil`. main.go:112 czyta je do stub-planningu.

---

## Sekwencja implementacji

### Krok 0 — Persist plan do repo `[inline]`; branch `core-split` off `go-parity`.

### Krok 1 — Pełny split (atomowy) `[opus]`
- **(a)** Utwórz `bus/` (przenieś core/bus.go, `package bus`; symbole bez `core.` prefiksu). Utwórz `registry/`
  (`Registry` = services map + `Provide[T]`/`Require[T]` generyczne, comma-ok przed asercją — S4). Utwórz
  `contrib/` (`Slots` + `Contribute`/`Contributions`). Utwórz `lifecycle/`: interfejsy `Module{Name, Requires,
  Init}` (`Requires()` zamiast `DependsOn()`), `Migrator/Starter/Stopper`, nowy `Registrar{Register(ctx)error}`;
  `Context` (pola `Bus *bus.Bus`, `Registry *registry.Registry`, `Slots *contrib.Slots`, `Mux/DB/Log`;
  metody-forward `Contribute`/`Contributions`) + `NewContext`; `App` (dawny Registry: Add/Build/Migrate/Start/Stop)
  z **two-phase Build (Register-pass → Init-pass, kolejność rejestracji, ZERO topoSort)**. Bez osobnej walidacji
  (N1 — eager Require w fazie 2 już panikuje przy braku; main.go stub-planning i tak zapobiega). **Skasuj `core/`.**
- **(a)** Przepisz WSZYSTKIE call-sites wg mapy: importy `gamebackend/core` → nowe pakiety; `core.Emit/On/Define/
  Bus/NewBus` → `bus.*`; `ctx.Provide(...)` → `registry.Provide(ctx.Registry,...)`; `ctx.Require("x").(T)` →
  `registry.Require[T](ctx.Registry,"x")`; `core.Module/Migrator/Starter/Stopper/Context/NewContext/NewRegistry` →
  `lifecycle.*`; **`DependsOn()` → `Requires()`** (rename, zostaje); dla 5 providerów **przenieś budowę serwisu +
  Provide z `Init` do `Register(ctx)`** (S3); **`rating` → pointer receiver + pole `svc` + `main.go:43`
  `&rating.Module{}`** (B3). `Contribute/Contributions` jako `ctx.Contribute` (forward) — bez zmian.
- **(b)** Atomowy — rename/split rdzenia; kompilator wymusza spójność, nie da się „w połowie".
- **(c)** Zachowaj komentarze w stylu domu. `main.go`: `App.Build()` two-phase; `planModules` czyta `Requires()`.
  `.go-arch-lint.yml` (S1/S2): komponenty `bus/registry/contrib/lifecycle`; **`commonComponents:[bus,registry,
  contrib]` (BEZ lifecycle — inaczej liść mógłby importować lifecycle = cykl)**; `lifecycle` normalny komponent
  `mayDependOn:[bus,registry,contrib]`; **dodaj `lifecycle` do `mayDependOn` KAŻDEGO modułu + remote + cmd**;
  liście bez deps. Testy: rozbij `core/registry_test.go` (skasuj topoSort/cycle/Requires-ordering testy;
  `TestContributions`→`contrib/`; two-phase Build test→`lifecycle/`); zaktualizuj importy/konstruktory w admin/
  remote/accounts testach.
- **(d)** `[opus]` — design-bearing (two-phase, generyki, Context, brak cykli).
- **Weryfikacja:** `go build ./... && go vet ./... && go test ./...` zielone (kompilator łapie import-breaki,
  testy łapią behawior); `go-arch-lint check` OK; `golangci-lint` clean.

### Krok 2 — Weryfikacja regresji `[inline]`
Statyczne gate'y jw. **Runtime (to samo co go-parity Krok 7, musi zachować się identycznie):** monolit (register→
postać→inventory `starter_sword`); split A+B (ownerOf+verifySession po QUIC 200; event fanout; admin fan-out;
kill-A→503). Refactor NIE może zmienić zachowania. Potwierdź, że `core/` zniknął i nic go nie importuje
(`rg "gamebackend/core"` = puste).

## Log rozwiązań reviewera
- **B1** `DependsOn` jest load-bearing dla stub-planningu (main.go:112) — ZOSTAJE (jako `Requires()`), ginie tylko
  topoSort — §A + Krok 1.
- **B2** wykreślona fałszywa teza „characters nie Require'uje accounts" (characters.go:85 robi to) — §A.
- **B3** rating value→pointer + pole `svc` + `main.go:43 &rating.Module{}` — §A + Krok 1.
- **S1** `commonComponents` bez `lifecycle` (tylko liście) — Krok 1c.
- **S2** `lifecycle` w `mayDependOn` każdego modułu/remote/cmd — Krok 1c.
- **S3** Register buduje store+svc (ctx.DB w fazie 1), Init konsumuje svc — §A + Krok 1.
- **S4** `Require[T]` comma-ok przed asercją (zachowuje komunikat) — Krok 1.
- **S5** Stop = reverse-registration (nie reverse-dependency); benign, udokumentowane — §A.
- **N1** brak osobnej walidacji (eager Require już panikuje) — Krok 1.
- **N2** generyki wykonalne (`v.(T)` legalne, `any(any)` bez double-box) — potwierdzone; mapa call-sites trafna.

## Ryzyka / świadome
- **Atomowy diff** duży — mitygacja: pełna mapa z researchu (każdy call-site), kompilator+testy jako gate,
  runtime-regresja w Kroku 2.
- **Two-phase konstrukcja providera** — przeniesienie budowy serwisu do `Register` wymaga, by serwis dał się
  zbudować przed Init (ctx.DB dostępny w fazie 1 — jest). Uważać na moduły, gdzie provided-svc zależy od stanu
  budowanego w Init.
- **Generyki = win stylu, nie compile-DI** — udokumentowane; panic zostaje.
- **`core/` nietknięty" (teza go-parity) już nie obowiązuje** — to świadomy refactor SAMEGO rdzenia (upraszcza go),
  nie edycja rdzenia dla dodania funkcji.
