# Weles pre-M1 backlog (#2-#5) — research synthesis

Research pod serię planów backlogu przed-M1 (zakres: całe #2-#5 wg
[2026-07-15-1120-weles-m0-pre-m1-backlog-status.md](2026-07-15-1120-weles-m0-pre-m1-backlog-status.md),
po erracie zamykającej #1/B1). Metoda: 6 subagentów Explore na nieprzecinających
się warstwach (po jednym na #2-#5 + fundament weles + prior art
devctl/processctl), synteza w main modelu. Wszystkie mapy z exact file:line.
NIC nie jest jeszcze planem — to baza dowodowa.

Numeracja backlogu jak w status doc (#2 control endpoint, #3 readyz monitoring,
#4 generacje deployu, #5 dyscyplina narracji/flavor).

## Cross-cutting: fundament i inwarianty (każda zmiana MUSI je zachować)

Z `weles/src/` (fundament):
- **Lock bit-compat** (`weles/src/lock.rs`): Windows lock = dokładnie 1 bajt na
  offsecie `1u64 << 63`, `LOCKFILE_EXCLUSIVE_LOCK|LOCKFILE_FAIL_IMMEDIATELY`;
  Unix = `flock(LOCK_EX|LOCK_NB)` na całym pliku; owner-only DACL (Win) / mode
  0600 (Unix). Desync = złamanie „one rollout at a time". Lock brany PIERWSZY
  (`supervisor.rs:362-365`), zwalniany PO teardownie.
- **Zero-sharing**: `weles/Cargo.toml` nigdy nie zależy od crate'a workspace;
  wzorce KOPIOWANE z devctl/processctl z komentarzem proweniencji, nigdy
  importowane. Każda zmiana kuszona `use core::…` łamie inwariant.
- **state.json** (`weles/src/state.rs`): atomowy tmp→rename; schema
  `FleetState/ServiceState/Status/FleetStatus/ProcessIdentity`; kontrakt
  „`control_endpoint` = None dopóki flota w pełni nie wstanie" (zmienia go #2).
- **Stop-authority separation** (`STOP` = Ctrl-C/SIGTERM vs `CONTROL_STOP` =
  request `down`): żadna awaria control-plane nie może udawać operatorskiego
  stopu; `stop_requested()` ORuje oba.
- **Containment**: każdy spawn przez `platform::spawn` (Job Object / process
  group, kill-on-drop, nigdy PID/name lookup). `weles never builds` — odpala
  tylko artefakty z `deploy/`.

## #2 — Control endpoint bindowany PRZED bootem

**Stan dziś:** bind DOPIERO po `boot()`. `run_up` (`supervisor.rs:359-473`):
lock → walidacje → prep (mint_ca/seed_admin) → `boot()` (:431, cała pętla
spawn+health-check) → **dopiero jeśli `run_result.is_ok()`** bind
`ControlServer::bind` (:433-460) → `monitor()`. Czyli `status`/`down` nie mają
z czym gadać, dopóki flota nie jest zdrowa.

**Dokładne punkty dotknięcia:**
1. Przenieść blok `control_endpoint_path` + `CONTROL_STOP.set` +
   `ControlServer::bind` + `set_control_endpoint` + `checkpoint` z **po** `boot()`
   na **przed** `boot()` (po walidacjach/prep, przed :431). Restrukturyzować
   match Ok/Err tak, by błąd bindu short-circuitował ZANIM `boot()` się zawoła
   (dziś bind-fail tears down w pełni zdrową flotę — drogo; cel: tanio, nic nie
   wstało).
2. **`CONTROL_STOP` musi powstać przed `boot()`** — dziś `OnceLock::set` jest
   na :439, ściśle po zwrocie `boot()`, więc `down` w trakcie bootu jest
   fizycznie niemożliwy. Mechanika pętli (`if stop_requested() { return Ok(()) }`
   na :505 i :527) nie wymaga zmiany — zmienia się TYLKO kolejność inicjalizacji.
   Nowa ścieżka (`down` ubija boot) NIE ma dziś testu (`supervisor_tests.rs` nie
   dotyka `CONTROL_STOP`) → wymaga pokrycia.
3. **Zachować inwariant „checkpoint AFTER bind"** (:444-445): nigdy nie
   publikować `control_endpoint: Some(...)` do state.json zanim listener
   akceptuje. Sekwencja `bind → set_control_endpoint → checkpoint` przenosi się
   w całości, nie rozrywa.
4. **`main.rs::connect_target` (:94-121)**: dziś gałąź „fleet still booting,
   endpoint not up yet — użyj Ctrl-C" jest wprost zależna od starej kolejności.
   Po zmianie `control_endpoint` jest wypełniony wcześnie → ten branch albo
   nieosiągalny, albo nowa semantyka (None tylko w wąskim oknie prep).
5. **Lifetime `ControlServer`**: handle musi teraz przeżyć `boot()` (dłuższy
   scope) i być `drop`nięty przed/w `teardown()`.

**Prior art (devctl — referencja, KOPIUJ nie importuj):** devctl binduje
control endpoint PRZED buildem (`tools/devctl/src/supervisor.rs:194-204`):
`set_control_endpoint` + `ControlServer::bind` + `checkpoint` zanim `build()`
(:209) w ogóle ruszy. To dokładnie docelowa kolejność weles. Nie importujemy
(zero-sharing + weles ma prostszą tożsamość: `identity_plausible` zamiast
devctlowego `observe_process_identity`/start-marker, `control.rs:10,183`).

**Rozmiar:** mały, samodzielny. Reorder + wiring `CONTROL_STOP` + poprawka
komunikatu w `connect_target` + test mid-boot `down`.

## #3 — Post-healthy monitoring `/readyz` → Degraded/NotReady (NIGDY restart na 503)

**Stan dziś:** weles NIE monitoruje nic po fleet-healthy. `monitor()`
(`supervisor.rs:568-609`, tick 100ms) → `observe()` woła `health::probe()`
TYLKO w `Phase::WaitingHealthy` (:631-639); dla `Healthy` zwraca wyłącznie
liveness (`try_wait`). `step()` dla `Healthy` restartuje TYLKO na
`Observed::Exited`. **Jedyny trigger restartu = śmierć procesu.** Serwis może
wisieć na 503 w nieskończoność i weles tego nie zauważy.

**Primitive już istnieje:** `health::probe(port)` (`weles/src/health.rs:30-56`)
— surowy TCP `GET /readyz`, `CONNECT_TIMEOUT=300ms`, `IO_TIMEOUT=500ms`,
tri-state `Ready`/`NotReady`(w tym 503)/`ConnectFailed`. Idealnie mapuje się na
Degraded vs NotReady vs (nadal) crash.

**`/readyz` w backendzie** (`core/app/src/lib.rs::readyz_response` :1300-1346):
agreguje DB `SELECT 1` (tylko gdy jest pool) + każdy kontrybuowany
`httpmw::ReadyCheck` (asyncevents-worker: dead/`delivery_stalled` 30s;
asyncevents-retention; invalidation: stale 60s; `stub:<provider>` cached probe;
scheduler). Każdy check bounded `READY_CHECK_TIMEOUT=2s`; pusta mapa → 200,
niepusta → 503 z JSON. **Koszt ~800ms/svc** bierze się z realnego DB pingu
(z pool-acquire wait do 2s) + serializowanych checków → przy flocie ~12 svc i
ticku 100ms round-robin budget jest OBOWIĄZKOWY, nie opcja.

**Punkt rozszerzenia (rozłączny z restartem):** dodać nowy wymiar `readiness`
na `Supervised`/`ServiceState`, zapisywany przez round-robin poll TYLKO gdy
`Phase::Healthy`, czytany przez `status`/`down`, **NIGDY nie konsultowany przez
`step()`** (którego directive musi dalej zależeć wyłącznie od
`Observed::Exited`/liveness — inaczej mrugnięcie Postgresa flipuje 12 svc =
restart storm). Dziś ani `Phase` (`supervisor.rs:151-160`) ani `Status`
(`state.rs:14-34`) nie mają wariantu Degraded/NotReady — do dodania jako
ortogonalne pole, NIE nowy `Phase`. Utrzymać czystość `step()`/`next_restart`
(pure, clock-injected, unit-testowalne).

**Rozmiar:** średni, net-new subsystem (poll round-robin + nowy wymiar stanu +
raportowanie w status). Powiązane z pamięcią
`memory/timing-sensitive-tests-doctrine.md` (nie ścigać zegara w testach).

## #4 — Generacje deployu (`deploy/gen-N/` + atomowy `current` + hashe)

**Chokepoint = jedna funkcja:** `Layout::binary(pkg)` (`weles/src/prep.rs:73-76`)
zwraca `bin_dir.join("<pkg>.exe")` gdzie `bin_dir = <root>/deploy` (płaskie,
jedna generacja). WSZYSCY konsumenci idą przez nią: `validate_binaries`
(:111), `spawn_service` (`supervisor.rs:807-808`), `mint_ca` (:226),
`seed_admin` (:287); zapis w `deploy()` przez `bin_dir.join` (:177). Wstawienie
`deploy/current/<pkg>.exe` (gdzie `current` = plik/symlink → `gen-N/`) w tej
jednej funkcji rozlewa indirekcję na wszystkich jednym ruchem.

**`deploy()` dziś** (`prep.rs:153-205`): kopiuje pakiety `src_dir → bin_dir`
**nadpisując w miejscu**, self-copy guard (kanonikalizacja), pętla nie przerywa
przy błędzie, **„no rollback in M0"** (skopiowane zostają staged). BRAK rollout
locka w `deploy` (lock tylko w `run_up`). **Windows live-exe:** nadpisanie
żywego exe FAILUJE głośno (running proc trzyma lock), Unix cicho sukces (proc
trzyma odlinkowany inode) — zaakceptowana asymetria M0, cyt.
`prep.rs:147-152`. Gen-N zamyka to KONSTRUKCYJNIE: nowa generacja pisze do
świeżego katalogu, żywa flota trzyma stare pliki otwarte, `current` flip nie
dotyka ich; respawn re-czyta ścieżkę świeżo (`supervisor.rs:693-711`) → flip
dotyczy tylko przyszłych spawnów (spójne z Windows lockiem).

**Greenfield:** zero hashy/wersji dziś (`Layout` = root/run_dir/bin_dir;
`state.json` = name/status/pid/restarts). Nowy rekord (hash per artefakt, gen
id) trzeba wprowadzić — nic nie migrujemy.

**Brak prior art:** devctl buduje-i-odpala z `target/<profile>/`
(`WorkspaceLayout`) — model buildu nie ma dwuznaczności current-vs-previous, bo
nic nie jest „live" w trakcie buildu. Nie ma kodu do skopiowania — to problem,
którego `WorkspaceLayout` nigdy nie miał. Strażnik zakresu (z status doc): NIE
package manager (bez podpisów, remote fetch, rejestru wersji).

**Rozmiar:** średni, greenfield. Refactor chokepointu + staging do gen-N +
atomowy flip + rekord generacji/hashy.

## #5 — Dyscyplina narracji / flavor (DEVELOPMENT, parytet z fleet.rs)

**Kluczowe odkrycie: „flavor" NIE jest pojęciem w weles.** Zero wystąpień
`flavor/prod/production` w `weles/src`. Development-ness jest CZYSTO niejawna —
jest tylko jeden zahardkodowany manifest (`manifest.rs`, dev porty + dev-seedy:
`APIKEYS_DEV_SEED`, `ACCOUNTS_DEV_AUTH`, `INVENTORY_DEV_GRANT`,
`ADMIN_COOKIE_SECURE=0`, admin/admin przez `seed_admin` `prep.rs:281-327`
BEZ żadnego gate'u). Jedyna oś CLI to `Topology{Split,Monolith}` — ortogonalna
do flavor.

**Realny `FleetFlavor{Development,Proof}` żyje TYLKO w
`tools/processctl/src/fleet.rs:106-109`** i jest dla weles niewidoczny
(zero-sharing). NIE MA `Production` nigdzie. `Proof` = harness bolted on top of
Development przez post-hoc mutację env (`fleet.rs:586-600,640-645`) — to
DOKŁADNIE anty-wzorzec, którego M1 ma nie kopiować (mutowanie tego samego pola
env pod `if flavor==Prod`).

**Parytet weles↔fleet.rs = TYLKO ręczny** (komentarze proweniencji +
self-goldeny `manifest_tests.rs` sprawdzające weles PRZECIW SOBIE, nie przeciw
processctl). `grep -rln weles tools/` = nic. **Brak automatycznego cross-checku
w JAKĄKOLWIEK stronę.** Zmiana portu/env/dep w fleet.rs cicho dryfuje od weles.
weles ma `validate_disk` (`manifest.rs:349-381`) ale sprawdza TYLKO
weles-vs-`cmd/*-svc`, nie weles-vs-processctl → **fałszywe poczucie
bezpieczeństwa** („drift is checked", a guardowana jest tylko jedna strona).

**Seam, który M1 ma trzymać czysto:** `ServiceDef.env_extra` to jeden
nietagowany worek mieszający wiring topologii (`TLS_MODE`, `PLAYER_EDGE_ADDR`)
z dev-seedami i security-CIDR (`TRUSTED_PROXY_CIDRS=127.0.0.1/32`). Upstream ma
to już jawne jako `overrideable_env` (`fleet.rs:120-122`), którego weles
świadomie NIE portował (deterministic-only). Wprowadzenie prod-flavor w weles
musi wymyślić rozróżnienie dev/prod od zera — i unikać post-hoc mutacji à la
`Proof`.

**Charakter zadania:** bardziej dyscyplina/guardrail niż kod. Konkretny
aktykowalny gap: **brak self-checkującego parytetu weles↔processctl** — pasuje
do `memory/didnt-forget-scripts-must-self-check.md` (narzędzie na ręcznej
liście MUSI diffować się z realnym źródłem prawdy). Zgodne z
`memory/config-as-code-anti-magic.md`. Ostrożnie: zero-sharing zabrania importu
— cross-check musi być kopią/kontraktem, nie importem fleet.rs.

## Rekomendowana sekwencja planów

Backlog jest wiążąco uporządkowany #2→#3→#4→#5; research to potwierdza jako
sensowną kolejność (rosnąca zależność/rozmiar), z jedną uwagą:

1. **#2 control endpoint** — najmniejszy, samodzielny, jest referencja devctl.
   Dobry pierwszy plan.
2. **#3 readyz monitoring** — net-new subsystem; primitive (`health::probe`)
   gotowy; kluczowa dyscyplina: readiness ortogonalny do restartu.
3. **#4 generacje deployu** — refactor jednego chokepointu + greenfield rekord.
4. **#5 flavor/parytet** — najbardziej guardrailowy; jeśli chcemy, mały
   self-check parytetu można wyciągnąć wcześniej jako niezależny drobiazg.

Każdy punkt = osobny plan (osobne troski, osobne pliki) — Plan Writing Workflow
per punkt (subagenty researchowe już zjedzone tutaj; do planu wchodzi już tylko
sekwencjonowanie + reviewer).

**Poza zakresem tego researchu (znane, śledzone osobno):** B2 (flake
`down_waits_...` pod pełną równoległością workspace) i B3 (jednoliniowa luka
`BUILD_ENV_ALLOWLIST` — brak `SYSTEMDRIVE`/`ProgramData`) — małe, niezależne,
mogą lecieć równolegle do backlogu.
