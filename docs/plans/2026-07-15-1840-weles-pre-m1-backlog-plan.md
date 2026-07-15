# Weles pre-M1 backlog (#2-#5) — jeden plan

Plan na całe #2-#5 backlogu przed-M1 (po erracie zamykającej #1/B1). Baza
dowodowa: [research synthesis](../status/2026-07-15-1815-weles-pre-m1-backlog-research-status.md)
(6 subagentów Explore, exact file:line). Wszystkie zmiany w `weles/` (+ jedna
blokująca stage w `tools/verifyctl` dla #5). NIC poza Welesem.

Cztery części, wiążąca kolejność #2→#3→#4→#5. Każda część jest samodzielnie
reviewowalna i commitowalna.

> **Rewizja 1 (po grumpy review, Opus/think-hard):** naniesiono cztery High +
> kilka Med. Kluczowe rozstrzygnięcia autorytetu podjęte PRZED implementacją
> (nie „reviewer confirm"): (C) generacja floty **pinowana raz w
> `Layout::discover`**, nie per-call; (B) readiness sondowany **na osobnym
> wątku**, nie na wątku monitora; (A) `fleet_stop` **przewleczony jako `Arc`**,
> `CONTROL_STOP` OnceLock usunięty; (D) porównanie na **pełnym znormalizowanym
> `compose_env`** z wyliczoną listą wykluczeń, stage **blokująca**. Zmiany
> semantyki (deploy↔up, kontrakt `control_endpoint`) zapisane jawnie w krokach.

## Context — dlaczego rozszerzamy weles, a nie budujemy nowe

Trzy nakładające się systemy (Research before planning):
- **Control endpoint** — istnieje (`weles/src/control.rs`, `ControlServer`),
  problem to KOLEJNOŚĆ bindu, nie brak mechanizmu. *Why not new:* devctl ma
  wzorzec bind-przed-buildem (`tools/devctl/src/supervisor.rs:194-204`) —
  kopiujemy kolejność do istniejącego kodu weles (zero-sharing: kopia, nie import).
- **Health probe** — istnieje (`weles/src/health.rs::probe`, tri-state). *Why not
  new:* #3 re-używa primitive'u; nic nowego w warstwie sieci.
- **Fleet manifest + drift check** — istnieje (`weles/src/manifest.rs` +
  `validate_disk`). *Why not new:* #5 nie dodaje flavor-enuma; dokłada self-check
  parytetu weles↔processctl (dziś tylko ręczny).

## Inwarianty do zachowania (każda część)

Lock bit-compat (bajt `1<<63`, brany pierwszy, zwalniany po teardownie);
**zero-sharing** (`weles/Cargo.toml` nie importuje crate'a workspace — wzorce
kopiowane z komentarzem proweniencji); state.json atomowy tmp→rename;
**stop-authority separation** (`STOP` sygnałowy vs control-`down` — żadna awaria
control-plane nie ustawia fleet-stop); spawn wyłącznie przez `platform::spawn`;
**weles never builds**; `step()`/`next_restart` pozostają czyste (no I/O,
clock-injected); **monitor tick pozostaje non-blocking** (nic nie blokuje ticku
w nieskończoność — dot. B).

---

## Część A — #2 Control endpoint bindowany PRZED bootem

### Step A1 — `fleet_stop` jako `Arc`, bind przed `boot()` `[opus]`
**(a) Co:** `weles/src/supervisor.rs` — `run_up` (:359-473), `boot` (:498-562),
`monitor` (:568-609), `stop_requested` (:71-76), usunięcie statyka
`CONTROL_STOP` (:66-69).
**(b) Dlaczego teraz:** fundament — `down`/`status` na flocie w boocie; bind-fail
failuje `up` zanim cokolwiek wstanie. Warunek dla A2/A3.
**(c) Jak:**
- **Usunąć `CONTROL_STOP: OnceLock`** (reviewer A1: set-przed-boot przy drugim
  `run_up` w tym samym procesie — testy — cichnie i `stop_requested()` czyta
  stary Arc). Zamiast tego przewlec `fleet_stop: &Arc<AtomicBool>` przez
  `boot(...)` i `monitor(...)`; `stop_requested(fleet_stop: &AtomicBool) -> bool`
  = `STOP.load() || fleet_stop.load()`. `STOP` (sygnałowy statyk) zostaje —
  handler OS dotyka tylko statyka. To usuwa hazard OnceLock i czyni `boot()`
  testowalnym (A3).
- **Kolejność w `run_up`** (po `reporter.checkpoint` :429):
```rust
reporter.checkpoint(&fleet);                       // status Starting, endpoint None

let endpoint = control_endpoint_path(&layout, &reporter.run_id);
let fleet_stop = Arc::new(AtomicBool::new(false));
let control = match ControlServer::bind(endpoint.clone(), reporter.shared(), Arc::clone(&fleet_stop)) {
    Ok(control) => control,
    Err(error) => {
        teardown(&mut fleet, &reporter, FleetStatus::Failed);  // nic nie wstało: no-op→Failed, tanio
        return Err(error).with_context(|| format!("bind control endpoint {}", endpoint.display()));
    }
};
// bind Ok = listener akceptuje → publikuj endpoint DOPIERO teraz (status wciąż Starting):
reporter.set_control_endpoint(Some(endpoint.to_string_lossy().into_owned()));
reporter.checkpoint(&fleet);

let run_result = boot(&layout, &inputs, &mut fleet, &reporter, &fleet_stop);
if run_result.is_ok() && !stop_requested(&fleet_stop) {
    reporter.set_status(FleetStatus::Running);
    reporter.checkpoint(&fleet);
    println!("weles: fleet healthy — press Ctrl-C or run `weles down` to stop");
    monitor(&layout, &inputs, &mut fleet, &reporter, &control, &fleet_stop);
}
drop(control);                                     // stop+join przed teardownem (każda ścieżka)
let terminal = if run_result.is_ok() { FleetStatus::Stopped } else { FleetStatus::Failed };
teardown(&mut fleet, &reporter, terminal);
run_result
```
- Efekt: `fleet_stop` żyje JUŻ w trakcie `boot()` → mid-boot `down` honorowany
  (mechanika pętli :505,:527 bez zmian, tylko źródło flagi). `ControlServer` żyje
  przez boot+monitor; `drop(control)` przed teardownem na KAŻDEJ ścieżce
  (bind-fail robi wcześniejszy `return`, więc jeden teardown per ścieżka —
  reviewer potwierdził brak double-teardown). Inwariant „checkpoint AFTER bind"
  zachowany.
- **Uwaga proweniencji:** zaktualizować komentarze „Fleet is healthy: bring up…"
  — bind poprzedza teraz boot.

### Step A2 — `connect_target`: semantyka pustego endpointu `[opus]`
**(a) Co:** `weles/src/main.rs::connect_target` (:94-121), gałąź `None` (:111-119).
**(b) Dlaczego teraz:** po A1 endpoint publikowany PRZED boot → komunikat „use
Ctrl-C to abort a boot" jest błędny (`down` działa w boocie), a gałąź osiągalna
tylko w sub-sekundowym oknie między checkpointem :429 a post-bind checkpointem.
**(c) Jak:** bounded retry (3× re-load z ~100ms) zanim uzna None za realny; jeśli
dalej None przy żywym non-terminal supervisorze — komunikat: fleet w bardzo
wczesnym starcie (przed bindem), spróbuj za chwilę; BEZ „Ctrl-C to abort". Nie
dotykać `Inactive`/`Stale`.

### Step A3 — kontrakt `control_endpoint` + testy #2 `[opus]`
**(a) Co:** `weles/src/state.rs` doc `control_endpoint` (:85-87);
`weles/src/supervisor_tests.rs`; ewentualnie `control_tests.rs`.
**(b) Dlaczego teraz:** kontrakt „None until booted" jest ODWRÓCONY przez A1
(endpoint w trakcie boot, status Starting) — zapisać w tym samym rolloutcie
(Fix-the-Authority reguła 4). Ścieżka „`down` ubija boot" jest NOWA i niepokryta.
**(c) Jak — prove-the-failing-branch, BEZ realnych binarek (reviewer A2):**
- Zaktualizować doc `control_endpoint`: „None tylko w wąskim oknie prep przed
  bindem; publikowany PRZED bootem (status wtedy Starting)".
- **stop podczas boot (branch po dekompozycji):** `stop_requested(&fleet_stop)`
  z `fleet_stop=true` (bez `STOP`) → true; `boot(..., &fleet_stop)` z
  `fleet_stop` już true na wejściu i **pustą flotą** → `Ok(())` bez spawnu
  (istniejący seam :505-507, teraz sterowany przez Arc). To pokrywa realny
  branch bez procesów.
- **`down` faktycznie flipuje Arc:** test na `ControlServer` — request `down`
  ustawia przekazany `fleet_stop` (`control.rs::response` :286-312 store true).
  Złożenie obu dowodzi „`down` w trakcie boot → boot wychodzi" konstrukcyjnie.
- **bind-fail przed boot:** wymusić `ControlServer::bind` Err (drugi bind na tej
  samej nazwie pipe’a/ścieżce UDS) i asserta Err + brak spawnu. Jeśli `run_up`
  za grube — wydzielona czysta funkcja sekwencjonująca „bind→(boot|early-return)"
  albo test integracyjny `weles/tests/` sterujący realnym `weles` (cięższy).

**Verify A:** `cargo test -p weles` (bez DB — weles nie dotyka Postgresa).

---

## Część B — #3 Post-healthy monitoring `/readyz` → Degraded (NIGDY restart na 503)

### Step B1 — readiness poller na OSOBNYM wątku + wymiar stanu `[opus]`
**(a) Co:** `weles/src/supervisor.rs` — nowy wątek pollera (wzorzec jak
`ControlServer` thread), `Reporter`/snapshot (:313-343); `weles/src/state.rs` —
`ServiceState` (:68-74) + enum `Readiness`; re-use `health::probe`.
**(b) Dlaczego teraz:** rdzeń #3; poprzedza render (B2) i testy (B3).
**(c) Jak — reviewer B1 (probe NIE na wątku monitora):**
- Nowy enum (state.rs, serde): `pub enum Readiness { Unknown, Ready, Degraded, Unreachable }`
  — `Ready`=probe 200, `Degraded`=`ProbeResult::NotReady` (503/torn),
  `Unreachable`=`ProbeResult::ConnectFailed`, `Unknown`=nie-`Healthy`/przed
  pierwszą sondą.
- `ServiceState` dostaje `readiness: Readiness` (do snapshotu :320-328).
  **Osobne od `status`** (status=restart-lifecycle, readiness=freshness).
- **Osobny wątek pollera** (NIE w `monitor`): `monitor` na wątku supervisora
  musi zostać non-blocking (inwariant + docstring :564-567); synchroniczny probe
  (~300ms connect / ~500ms read, do ~800ms dla wiszącego svc) na tym wątku
  opóźniłby detekcję crashów innych svc i odpowiedź na `down`/Ctrl-C (reviewer:
  „utrzymuje tick krótki" byłoby fałszem). Zamiast tego:
  - Wątek `weles-readiness` (spawniony w `run_up` obok bindu control-endpointu,
    dostaje `Arc<Mutex<Vec<Readiness>>>` indeksowany jak fleet + `Arc<AtomicBool>`
    stop + listę `(name, http_port)` + info które indeksy są `Healthy`).
    Round-robin: jeden `health::probe` na ~250ms, kursor po serwisach; wpis
    aktualizowany w `Arc<Mutex<..>>`. Serwisy nie-`Healthy` → `Unknown`.
  - **Which svc are Healthy** czyta z tego samego `Reporter.shared`
    (`Arc<Mutex<FleetState>>`) — poller patrzy na `services[i].status == Healthy`.
  - `monitor` na każdym ticku (bez blokowania) kopiuje readiness-wektor z mutexa
    do `Supervised`/snapshotu przy najbliższym checkpointcie. Zmiana readiness →
    checkpoint (żeby `status` widział świeżość).
  - Stop+join wątku pollera przez ten sam `fleet_stop` + `drop`/join przed
    teardownem (jak `ControlServer`).
- **KRYTYCZNE (authority):** wynik sondy NIGDY nie trafia do `observe()`/`step()`.
  `observe()` dla `Healthy` dalej wyłącznie liveness (:630); `step()` dla
  `Healthy` restartuje tylko na `Exited` (:209-212). Readiness jest zapisywany
  obok i czytany tylko przez checkpoint/status — mrugnięcie Postgresa → N×
  `Degraded`, zero `Respawn`.

### Step B2 — render `readiness` w `weles status` `[sonnet]`
**(a) Co:** `render_status` w `weles/src/control.rs` (konsumuje
`FleetState.services`).
**(b) Dlaczego teraz:** wymiar bez ujścia jest bezużyteczny; po B1 pole istnieje.
**(c) Jak:** dołożyć adnotację readiness dla serwisów `Healthy`
(`healthy [ready]`/`[degraded]`/`[unreachable]`); dla nie-Healthy pominąć
(Unknown). Bez zmian protokołu (to samo `status` request).

### Step B3 — testy #3 `[opus]`
**(a) Co:** `weles/src/supervisor_tests.rs`.
**(b) Dlaczego teraz:** at-risk branch = „readiness NIGDY nie restartuje" +
poller nie zakłóca monitora.
**(c) Jak — prove-the-failing-branch (reviewer B2: test `step()` był tautologią):**
- **Wydzielić czystą funkcję** przypisania readiness w ticku
  (`fn apply_readiness(prev, probe: ProbeResult, is_healthy: bool) -> Readiness`)
  i testować, że dla `Healthy` svc z `ProbeResult::NotReady` daje `Degraded`
  **nie dotykając `phase`/`restarts`/`Directive`** — to pinuje realny branch
  (readiness→checkpoint only), nie istniejący `_ => Stay` w `step()`.
- Mapowanie `ProbeResult → Readiness` na trzech wariantach.
- **Round-robin czysto:** wydzielić wybór kursora (`next probe index` po Healthy,
  pomijając nie-Healthy) i testować, że przy N Healthy każdy sondowany raz na N,
  nie-Healthy pomijane, `fleet.len()==0` nie panikuje (div-by-zero guard).
- **Poller ⊥ monitor:** test, że aktualizacja readiness-wektora przez poller nie
  zmienia `Supervised.phase`/`status` ani nie generuje `Directive` (czytamy tylko
  do snapshotu).

**Verify B:** `cargo test -p weles`.

---

## Część C — #4 Generacje deployu (`deploy/gen-N/` + `current` pinowany + hashe)

### Step C1 — generacje, pin-at-discover, hashe `[opus]`
**(a) Co:** `weles/src/prep.rs` — `Layout` (:46-77), `deploy()` (:153-205),
`validate_binaries` (:108-127); nowy rekord generacji.
**(b) Dlaczego teraz:** rdzeń #4; jeden chokepoint `Layout::binary` rozlewa
indirekcję.
**(c) Jak — reviewer C1-C4 (pin generacji, nie per-call):**
- **PIN raz w `Layout::discover`** (NIE per-call — reviewer C1: per-call
  `current` przy respawnie dałby mieszaną generację floty; C4: `binary()`
  zostałby fallible). `discover` czyta `deploy/current` (mały PLIK tekstowy z
  nazwą gen-dira — NIE symlink: Windows perms) JEDEN raz i zapisuje
  `active_bin_dir = deploy/<gen>/` w `Layout`. `Layout::binary(pkg)` zwraca
  `active_bin_dir.join(<pkg>.exe)` — **infallible, jak dziś**. Cała flota danego
  `up` biegnie jedną, spójną generacją; respawn (`supervisor.rs:694→808`)
  re-rozwiązuje TĘ SAMĄ przypiętą ścieżkę. Brak `deploy/current` (świeży
  checkout) → `discover` zwraca jasny błąd „nic nie zdeployowano — `weles deploy
  <dir>`" (nie surowy brak pliku w `validate_binaries`).
- **`deploy()`** (proces osobny od `up`): (1) następny `gen-<N>` (max
  `deploy/gen-*` +1); (2) `create_dir_all(deploy/gen-N)`; (3) kopiuj każdy pakiet
  `src → deploy/gen-N/<file>`, licząc SHA-256; (4) `deploy/gen-N/manifest.json`
  = `{gen, artifacts:[{pkg,file,sha256,bytes}]}` (greenfield rekord); (5) DOPIERO
  gdy WSZYSTKIE kopie+hashe OK — atomowo flip `deploy/current` (zapis
  `current.tmp`→rename, dyscyplina jak `state::checkpoint`). Partial-fail →
  `current` wskazuje STARĄ generację (nienaruszalną), `gen-N` zostaje jako
  obserwowalny śmieć, `bail!` z per-plik listą (zachować raport :192-203, ale
  **rollback = current nietknięty**, nie „no rollback").
- **Windows live-exe zamknięte KONSTRUKCYJNIE:** nowa generacja to świeży katalog,
  którego żywa flota nie trzyma otwartego → zero konfliktu locka; pin-at-discover
  gwarantuje, że żywy `up` NIGDY nie re-czyta `current`, więc równoległy `deploy`
  flipujący `current` nie wpływa na biegnącą flotę (reviewer C3: usuwa race bez
  brania up-locka). **`deploy` NIE bierze exclusive up-locka** (up trzyma go całe
  życie — deploy by się zablokował, niwecząc „deploy pod żywą flotą"); staging do
  świeżego gen-dira jest bezpieczny bez locka. Usunąć/przepisać doc-komentarz
  „Live-fleet safety is deliberately NOT enforced" (:147-152).
- **Retencja (reviewer C2 — nie usuwać żywej generacji):** po udanym flipie
  trzymać bieżącą + poprzednią generację (poprzednią może pinować równolegle
  biegnący `up`); usuwać tylko `gen < (current-1)`. **Delete tolerant** — plik
  zablokowany (Windows) → log-and-skip, NIGDY `bail!` (inaczej zamykamy
  „overwrite live exe" a otwieramy „delete live exe").
- **Zapis semantyki (Fix-the-Authority reguła 4):** zmiana kontraktu deploy↔up
  (deploy mutuje źródło żywej floty via `current`, bezpieczne dzięki
  pin-at-discover) — nazwać w commit message ORAZ w doc-komentarzu `prep.rs`.
- `self-copy guard` (:159-170) zostaje.
- Zakres: fundament generacji; `weles rollback` CLI = M1 (mechanizm przez plik
  `current` istnieje, ale komenda poza zakresem). NIE package manager (bez
  podpisów/fetch/rejestru).

### Step C2 — testy #4 `[opus]`
**(a) Co:** `weles/tests/prep.rs` + `prep` unit-testy.
**(b) Dlaczego teraz:** at-risk = atomowość flipu, partial-fail, pin.
**(c) Jak — prove-the-failing-branch:**
- **Pin-at-discover:** deploy gen-1 (current→gen-1); `Layout::discover` pinuje
  gen-1; deploy gen-2 (current→gen-2) PO discover; ten sam `Layout` dalej zwraca
  ścieżki gen-1 (dowód: biegnąca flota nie przeskakuje generacji).
- **Partial-fail NIE rusza current:** deploy z brakującym plikiem → `bail!`,
  `current` dalej wskazuje poprzednią, świeży `Layout::discover` rozwiązuje
  starą (to jest branch dziś „no rollback").
- **Hash:** `gen-N/manifest.json` SHA-256 zgodny z ponownie policzonym.
- **Retencja:** po 3 deployach istnieją tylko 2 najnowsze gen-diry; delete
  zablokowanego pliku → skip+log, nie błąd (symulować lock jeśli platform-owo
  wykonalne; inaczej test jednostkowy na „delete-tolerant" helperze).
- **Świeży stan:** brak `deploy/current` → `discover` daje „nic nie zdeployowano".

**Verify C:** `cargo test -p weles`. Manualny: `weles deploy target/debug` 2× →
`deploy/current` + `deploy/gen-*` + `manifest.json`.

---

## Część D — #5 Dyscyplina narracji: self-check parytetu weles↔processctl

### Step D1 — BLOKUJĄCA stage `weles-fleet-parity` w verifyctl `[opus]`
**(a) Co:** nowa stage w `tools/verifyctl` importująca `weles` (lib) +
`processctl` (lib), diffująca Development-flavor fleet.
**(b) Dlaczego teraz:** parytet weles↔processctl jest DZIŚ tylko ręczny;
`validate_disk` guarduje tylko weles-vs-`cmd/*-svc` → fałszywe poczucie
bezpieczeństwa (pamięć didnt-forget-scripts-must-self-check). Reviewer D3: weles
NIE jest ćwiczony przez splitproof, więc bez tej stage’y nie ma ŻADNEGO gate’u —
stąd **blokująca w `--fast`** (czysto in-memory, bez DB/rollout, natychmiastowa),
nie advisory.
**(c) Jak — reviewer D1/D2 (pełny znormalizowany env, nie 5 kluczy):**
- Porównanie na **znormalizowanym composed env**: `weles::manifest::compose_env(def, dummy_inputs)`
  vs `processctl` Development composed env
  (`game_backend_fleet_with_environment(Development)` / `game_backend_monolith(Development)`),
  per serwis, PLUS `name`/`http_port`/`edge_port`/`player_port`/`has_db`/
  `pool_max`/`dependencies`. Obejmuje peer-wiring (`*_EDGE_ADDR`/`*_HTTP_ADDR`) i
  `pool_max` — miejsca, gdzie dryf realnie boli (reviewer D1).
- **Wyliczona lista wykluczeń z per-pole powodem** (tylko klucze z definicji
  rozbieżne — np. te, które fleet.rs wystawia przez `overrideable_env`, a weles
  determinizuje; `DATABASE_URL` różne z natury). Każde wykluczenie z konkretnym
  uzasadnieniem — inaczej stage to teatr.
- Zero-sharing zachowany: **weles importuje NIC**; to VERIFYCTL importuje oba
  (weles lib target, processctl lib; verifyctl już zależy od processctl przez
  splitproof; archcheck rządzi `modules/`+`core/`, nie `tools/` → brak cyklu,
  reviewer potwierdził). Dodać do manifestu blokującego, tabela PASS/FAIL/SKIP.

### Step D2 — nota o seamie dev/prod `[sonnet]`
**(a) Co:** krótka nota w `docs/reference/` (lub doc-komentarz
`weles/src/manifest.rs`).
**(b) Dlaczego teraz:** #5 to dyscyplina narracji — udokumentować, że `env_extra`
miesza wiring topologii z dev-seedami i security-CIDR w jednym worku; M1 z
prod-flavor NIE ma kopiować post-hoc mutacji à la `FleetFlavor::Proof`
(`fleet.rs:586-600`), tylko strukturalnie rozdzielić.
**(c) Jak:** 1 akapit + wskaźnik do research doc. Bez kodu, bez flavor-enuma
(świadomie — nie ma dziś prod-flavor i nie przemycamy go).

**Verify D:** `cargo run -p verifyctl -- --fast` (stage widoczna, PASS na
zgodnym HEAD; ręczny drift w manifest.rs → FAIL).

---

## Dispatch — tagi (do zatwierdzenia przez Lukasza)

| Krok | Lane | Uzasadnienie |
|------|------|--------------|
| A1 fleet_stop Arc + bind przed boot | `[opus]` (core-implementer) | lifecycle ordering, usunięcie OnceLock — taksonomia core |
| A2 connect_target | `[opus]` | semantyka pustego endpointu |
| A3 kontrakt + testy #2 | `[opus]` | prove-the-failing-branch (mid-boot down) |
| B1 readiness poller (osobny wątek) | `[opus]` (core-implementer) | authority: readiness ⊥ restart; non-blocking tick |
| B2 render status | `[sonnet]` | mechaniczne, w pełni wyspecyfikowane |
| B3 testy #3 | `[opus]` | at-risk: „503 nie restartuje", poller ⊥ monitor |
| C1 generacje + pin-at-discover | `[opus]` (core-implementer) | atomowość, pin, cross-platform |
| C2 testy #4 | `[opus]` | pin / partial-fail / retencja |
| D1 parity stage (blokująca) | `[opus]` | design self-checku, znormalizowany diff |
| D2 nota dev/prod | `[sonnet]` | doc |

Sesja = Opus → `[opus]` to top-tier lane (osobny kontekst = granica
niezależnego reviewera). Każda część commitowana osobno; po każdej — jeden
adwersarialny pass (core-reviewer, metoda ≠ implementera, `model:` ≥ tier
implementera). B2/B3 (backend bugi) poza tym planem.

**Poza zakresem (świadomie):** żaden nowy flavor-enum w weles (#5 to guardrail);
`weles rollback` CLI (M1); skrócenie ~30s okna detekcji martwego peera (osobna
decyzja z diagnozy B1).

## Changelog

- **Rev 1 (2026-07-15, po grumpy review Opus/think-hard):** C→pin-at-discover
  (był per-call, mieszana generacja); C retencja delete-tolerant, nie usuwa
  żywej gen; C zapis semantyki deploy↔up; B→osobny wątek pollera (był na wątku
  monitora, blokował detekcję crashów); B3→czysta fn readiness zamiast
  tautologii `step()`; A→`Arc` fleet_stop zamiast OnceLock; A3→dekompozycja
  testu bez realnych binarek + update kontraktu `control_endpoint`; D→pełny
  znormalizowany `compose_env` + wyliczone wykluczenia; D→stage blokująca (weles
  bez innego gate’u).
