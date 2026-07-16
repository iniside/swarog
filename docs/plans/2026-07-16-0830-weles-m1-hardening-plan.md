# Weles pre-M1 hardening (P1–P6) — plan naprawczy

Plan naprawczy dla punch-listy z przeglądu gotowości M1
([review status](../status/2026-07-16-0810-weles-m1-readiness-review-status.md),
Claude + Codex). Baza dowodowa: 6 subagentów Explore (2×A/B/C), exact file:line.
Wszystko w `weles/` — NIC poza Welesem.

> **Rev 1 (po grumpy plan-review, Opus/think-hard):** naniesiono cztery High +
> M/L. Kluczowe rozstrzygnięcia autorytetu PRZED implementacją: (H1) start-time
> check **asymetryczny**, nie `|Δ|≤TOL` — symetryczny false-zabijał żywego
> wolno-startującego supervisora → retencja kasowała żywą gen (fail-safe→dangerous);
> (H3) **rename-first WYRZUCONE** — na Windows przenosi katalog żywego exe
> (FILE_SHARE_DELETE) → śmierć respawnu; P4 = re-check live-pinu przy delete +
> tolerancyjny `remove_dir_all`; (kolejność) **P5 przed P4**; (M1) dodany
> proof-auditor na P4/P5. Zmiany zapisane w Changelogu.

Cel: domknąć klaster defektów na ścieżkach błędu/kolejności zanim M1 (hello/resolve,
SQLite, minting portów, repliki) je wzmocni. Żaden nie jest twardym blokerem
normalnej ścieżki, ale każdy dotyka deklarowanego inwariantu weles (dokładny
raport, brak sierot, bezpieczeństwo pinu) i M1 kilka z nich amplifikuje.

## Inwarianty do zachowania (każdy krok)

Zero-sharing (weles importuje TYLKO deps zewnętrzne — `libc`/`windows-sys` już są,
wzorce z devctl/processctl KOPIOWANE z notą proweniencji, nie importowane); single
stop-authority (`STOP` sygnałowy ⊥ `fleet_stop` control — żadna ścieżka błędu nie
pisze fleet-stop); `step()`/`next_restart`/`status_of`/`readiness_for` pozostają
czyste (no I/O, clock-injected); monitor tick non-blocking; state.json atomowy
tmp→rename; **weles never builds**; readiness ⊥ restart (żaden P tego nie tyka).
**NOWY inwariant (H1):** żadna zmiana `supervisor_alive` nie może false-declarować
ŻYWEGO supervisora martwym — to odwróciłoby retencję z over-protect (benign) na
under-protect (kasuje żywą gen).

## Research — dlaczego naprawiamy, a nie przepisujemy

Trzy nakładające się systemy już istnieją; naprawa = wzmocnienie autorytetu w
miejscu, nie nowy mechanizm (Fix-the-Authority, nie hack-on-hack):
- **Dokładność teardownu** — `devctl::teardown_with` (`tools/devctl/src/supervisor.rs:622-696`)
  MA już wzorzec: shutdown-Err → `ManagedStatus::Failed` + `cleanup_failures` →
  flota `Failed` → exit≠0. weles go NIE ma (`weles/src/supervisor.rs:1013` ustawia
  `Stopped` bezwarunkowo). *Why not new:* kopiujemy wzorzec devctl (zero-sharing).
- **Kolejność handlera** — devctl instaluje handler w `supervisor.rs:184` PRZED
  lockiem/buildem. weles w `:626` PO ~60s helperów. *Why not new:* przeniesienie
  wywołania, nie nowy handler.
- **Checkpoint Result** — `state::checkpoint` (`state.rs:130-143`) JUŻ zwraca
  `Result`; defekt to połknięcie w `Reporter::checkpoint` (`supervisor.rs:518`).
  *Why not new:* wariant propagujący, nie nowy zapis.
- **Prune / classify / start-time** — `prune_stale_generations`, `classify`,
  `supervisor_alive` istnieją; dokładamy re-check live-pinu / reorder / asymetryczne
  porównanie czasu startu (prior-art `processctl/.../windows.rs:534-555`).

---

## Kroki (wiążąca kolejność; każdy = osobny commit)

### Step 1 — P1: handler Ctrl-C przed helperami prep `[opus]`
**(a) Co:** `weles/src/supervisor.rs` — przenieść `install_ctrl_handler()?;`
(dziś `:626`, po `mint_ca` `:622` / `seed_admin` `:624`) na miejsce zaraz po
`lock::acquire` (`:556`), przed `validate_disk`/prep. Dodać `STOP.store(false,
SeqCst)` tuż przed instalacją (higiena — statyk współdzielony między wywołaniami w
tym samym procesie; devctl robi `INTERRUPTED.store(false)` `:185`).
**(b) Dlaczego teraz / order:** fundament, izolowany, zerowe zależności. Dziś
Ctrl-C w ~60s oknie prep (świeży checkout: `edgeca` do 30s + `adminctl` do 30s)
idzie domyślną dyspozycją OS. Handler dotyka TYLKO statyka `STOP` — brak zależności
od prep — więc przesunięcie jest bezpieczne.
**(c) Jak:** czyste przeniesienie wywołania + reset `STOP`. `boot()` (`:745`)
sprawdza `stop_requested` na szczycie, więc STOP ustawiony w trakcie helpera zostaje
uszanowany PO jego zakończeniu (deferral do ~60s — oba helpery IGNORUJĄ `STOP`;
korzyść P1 to **prevencja sieroty na unwindzie, nie responsywność** — L3).
- **Orphan risk jest platform-asymetryczny (L2):** na Windows helper biegnie w Job
  (KILL_ON_JOB_CLOSE) i dzieli konsolę weles → nieobsłużony Ctrl-C i tak go w dużej
  mierze ubija; realna sierota to Unix (helper we własnej grupie, reparent do init,
  gdy weles zginie niezreapowany). Fix i tak poprawny i tani — ale opisać warunkowo.
- **NON-GOAL + M1-amplified gap (M2, zapisany):** NIE przenosimy `ControlServer::bind`
  przed prep w TYM kroku. Skutek: `weles down` w oknie prep zwraca „very early
  startup" i nie zatrzyma floty (tylko odroczony Ctrl-C). **M1 (SQLite init, minting
  portów, hello/resolve) to okno WYDŁUŻA** — patrz „Poza zakresem" + decyzja dla
  Lukasza (fold-in czy osobno).
**(d) Dispatch:** `[opus]` core-implementer.

### Step 2 — P2: teardown raportuje dokładnie (shutdown-Err ≠ Stopped) `[opus]`
**(a) Co:** `weles/src/supervisor.rs`:
- Nowa CZYSTA fn obok `status_of` (`:252`) / `readiness_for` (`:274`):
  `fn stop_outcome(result: &Result<Outcome>) -> (Status, bool)` —
  `Ok(Graceful|Forced) => (Status::Stopped, true)`;
  `Err(_) => (Status::Failed, false)` (proces niepotwierdzenie martwy — `shutdown`
  Err = force nie zdążył / `force()` padł, `platform/mod.rs:156-159`).
- `teardown` (`:986`) — sygnatura na `-> bool` (clean); w gałęzi live-proc
  (`:1008-1013`) zamiast bezwarunkowego `Status::Stopped` użyć `stop_outcome`,
  `false` zbiera do `clean_all`. Głośny `eprintln!` przy Err zostaje + ostrzeżenie
  o możliwej sierocie. Zwrócić `clean_all`.
- `run_up` (`:705-712`) — jeśli `run_result.is_ok()` ale `!clean` → `terminal =
  Failed` i zwrócić `Err(anyhow!("teardown could not confirm all services stopped
  — see logs"))` (parytet devctl: exit≠0 przy niedokończonym teardownie).
**(b) Dlaczego teraz / order:** przed P6/P3 w klastrze run_up. Autorytet: dokładność
raportu + brak sierot to twardy wymóg dev-toolingu. `already-dead` gałąź
(`:997-1004`) zostaje `Exited` (poprawna).
**(c) Jak — dlaczego `Forced` = clean (L1, zapisać w doc-komentarzu):**
`Outcome::Forced` wraca DOPIERO po `force()` + `wait_for(force_timeout)`
potwierdzającym wyjście (`platform/mod.rs:152-155`) = brak sieroty. Krytyczne:
console-less weles na Windows DEGRADUJE KAŻDY shutdown do `Forced`
(`platform/mod.rs:137-150`) — gdyby `Forced` flagować jako unclean, każdy taki stop
dałby fałszywe exit≠0. Dlatego `Forced` MUSI być clean. Wzorzec `devctl::teardown_with`.
**Prove-the-branch:** case-table w `supervisor_tests.rs` (obok
`probe_result_maps_to_readiness_on_every_variant` `:461`) dla `stop_outcome`:
`Err`→`(Failed,false)`, oba `Ok`→`(Stopped,true)`. Zero-I/O; fixture NIE odtworzy
shutdown-Err (SIGKILL/TerminateJobObject nieblokowalny z userspace — C2) → dowód
czysto pure-fn.
**(d) Dispatch:** `[opus]` core-implementer.

### Step 3 — P3: wczesny pin-checkpoint fail-closed `[opus]`
**(a) Co:** `weles/src/supervisor.rs`:
- `Reporter` — `fn checkpoint_critical(&self, fleet) -> Result<()>`: jak
  `checkpoint` (`:513`) ale zwraca błąd `state::checkpoint` zamiast `eprintln!`
  (nadal aktualizuje `self.shared` przed persist).
- `run_up` — wczesny pin-checkpoint (`:599`) → `checkpoint_critical`; przy `Err` →
  `return Err(...)` (fail-closed; nic nie wstało, `_lock` zdejmowany przez Drop na
  return; komunikat: „could not persist initial state / pin protection — refusing to
  start"). Pozostałe `checkpoint()` zostają best-effort.
**(b) Dlaczego teraz / order:** pin jest safety-critical dla retencji #4 — cichy fail
+ równoległy `deploy` = usunięta żywa generacja. Autorytet: zapis pinu MUSI być
fatal (fail-closed lepszy niż start bez ochrony).
**(c) Jak — zakres autorytetu (M3, zapisać):** to co realnie łapiemy to „katalog
state jest zapisywalny NA STARCIE"; **znany rezydualny gap:** katalog, który
zepsuje się PO checkpoincie #1 (drugi `:641` i wszystkie monitorowe zostają
best-effort) dalej degraduje cicho (brak odświeżenia pinu, status/down ślepe,
retencja ślepa). Świadomie NIE dokładamy sygnału „sustained checkpoint failure"
tu (osobny gap, jak stale-delivery w asyncevents) — zapisać w „Poza zakresem".
Kontrola przepływu nad `Result`, nie nowa pure fn (C1/C2: brak pure seamu).
**Prove-the-branch:** test w `supervisor_tests.rs` konstruujący `Reporter` z
`state_path` = `dir/missing/state.json` → `checkpoint_critical` zwraca `Err`, a
`checkpoint` (`()`) połyka (kontrast). `Reporter` jest w tym samym module (`#[path]`),
więc konstruowalny w teście.
**(d) Dispatch:** `[opus]` core-implementer.

### Step 4 — P6: control żyje przez teardown (stale endpoint zamknięty) `[opus]`
**(a) Co:** `weles/src/supervisor.rs` `run_up` (`:698-710`) — przenieść
`drop(control)` (`:701`) na miejsce PO `teardown(...)` (`:710`). `drop(poller)`
(`:700`) zostaje PRZED teardownem (poller sonduje tearowane svc — stopować pierwszy).
**(b) Dlaczego teraz / order:** po P2 (ten sam klaster run_up). Dziś control
dropowany przed teardownem, a checkpointy teardownu trzymają `control_endpoint:
Some(...)` → concurrent `status`/`down` klasyfikuje `Connect` (`classify` `:160`
ignoruje endpoint) i trafia martwy endpoint; gałąź `None` w `connect_target`
(`main.rs:126`) mówiłaby mylące „very early startup". Trzymając control żywy przez
teardown, `status` dostaje ŻYWY endpoint i widzi `Stopping`.
**(c) Jak:** czysty reorder. `ControlServer` trzyma `Arc` klony (`shared`,
`fleet_stop`) — oba przeżywają teardown; teardown mutuje `shared` przez checkpoint
(Arc<Mutex>). `down` w teardownie flipuje `fleet_stop` (nieszkodliwy no-op), handler
`wait_for_terminal` (`control.rs:201`) doczeka terminala z końca teardownu
(zweryfikowane: `DOWN_TIMEOUT` 130s > worst-case teardown ~110s — L4). NIE
`set_control_endpoint(None)` (dałoby mylący „early startup" w teardownie — A2).
**Prove-the-branch:** case w `control_tests.rs` (szablon
`classify_reports_inactive_for_a_terminal_fleet` `:345`): `sample_state(Stopping,
pid)` z `control_endpoint = Some(...)`, `alive=true` → `Disposition::Connect`.
Reorder sam strukturalny (L4: żaden test nie strzeże przyszłej re-inwersji — świadomie).
**(d) Dispatch:** `[opus]` core-implementer.

### Step 5 — P5: Windows start-time (ASYMETRYCZNIE) w supervisor_alive; Linux known-gap `[opus]`
**(a) Co:** `weles/src/control.rs`:
- Windows `supervisor_alive` (`:421-442`): po `OpenProcess` (handle ma już
  `PROCESS_QUERY_LIMITED_INFORMATION` — wystarcza dla `GetProcessTimes`) dołożyć
  `GetProcessTimes` → creation `FILETIME` → unix-sek (pure `filetime_to_unix`).
  Zwrócić `pid_alive && !reused_pid`, gdzie
  `reused_pid = actual_creation_unix > identity.started_unix + SKEW`.
- Czyste fn: `filetime_to_unix(ft) -> u64` (100ns od 1601 − offset 1601→1970, /1e7)
  i `is_reused_pid(recorded, actual, skew) -> bool` = `actual > recorded + skew`.
- **ASYMETRYCZNIE — kluczowe (H1/H2):** `started_unix` = `SystemTime::now()` przy
  narodzinach (`supervisor.rs:1070`), zawsze PO OS-owym creation-time, luka
  nieograniczona (AV-scan, obciążona maszyna, debug). Symetryczne `|Δ|≤TOL`
  false-zabiłoby wolno-startującego ŻYWEGO supervisora → `live_pinned_generation`
  (`prep.rs:502`) → `None` → retencja kasuje ŻYWĄ gen (nowa ścieżka utraty danych) I
  `classify` → `Stale` → `down`/`status` błąd na żywej flocie. Odrzucamy WYŁĄCZNIE
  reused-pid (proces startował PÓŹNIEJ niż recorded): żywy supervisor ma
  `actual ≤ recorded < recorded+SKEW` → NIGDY odrzucony. `SKEW` kilka sekund
  (absorpcja truncacji `as_secs`).
- Linux (`:444-448`) i „neither" (`:450-453`): BEZ ZMIAN — udokumentowany known-gap
  (PID-only, over-protect benign). Nota w doc-komentarzu + „Poza zakresem".
**(b) Dlaczego teraz / order:** PRZED P4 (P4 re-check live-pinu opiera się na
`supervisor_alive`; nie shipować P4 obok potencjalnie false-dead liveness).
Priorytet nadal niski, ale poprawność liveness to prerekwizyt P4.
**(c) Jak:** prior-art `tools/processctl/src/platform/windows.rs:534-555`
(`GetProcessTimes` na otwartym handlu, złożenie Hi/Lo w u64) — SKOPIOWAĆ (zero-sharing).
`windows-sys` features `Win32_System_Threading`+`Win32_Foundation` już włączone.
**Prove-the-branch:** `control_tests.rs` — case-table `filetime_to_unix`
(znane FILETIME→znany epoch) i `is_reused_pid` (`actual=recorded+10,skew=3`→true;
`actual=recorded-100`→false; `actual=recorded+1,skew=3`→false = wolny-live).
PLUS regresja `classify`: identity z `started_unix` PRZED „now" i live-but-slow
→ `Disposition::Connect` (H2). Integracja z realnym procesem — pomijamy; dowód=pure fn.
**(d) Dispatch:** `[opus]` core-implementer. **Review:** + proof-auditor (M1).

### Step 6 — P4: prune bezpieczny (re-check live-pinu przy delete; BEZ rename) `[opus]`
**(a) Co:** `weles/src/prep.rs` `prune_stale_generations` (`:528-561`): tuż PRZED
każdym `remove_dir_all(gen-N)` ponownie sprawdzić `live_pinned_generation(run_dir)`
i **pominąć**, jeśli `N` == żywy pin (domyka TOCTOU między budową `protected`
`:408-415` a pętlą delete). `remove_dir_all` zostaje **tolerancyjny** (skip+log przy
Err). Sygnatura `prune_stale_generations` dostaje `run_dir: &Path` (dziś ma tylko
`bin_dir`).
**(b) Dlaczego teraz / order:** ostatni, po P5 (poprawny liveness). **rename-first
WYRZUCONE (H3):** na Windows running-image otwarty z `FILE_SHARE_DELETE`, więc
`rename(gen-N.trash)` ŻYWEJ generacji MOŻE się udać → unieważnia przypięty
`active_bin_dir` (`prep.rs:160`) → KAŻDY respawn nie znajdzie `.exe` (differentiator
weles). Dziś częściowy `remove_dir_all` przynajmniej zostawia zablokowany żywy
`.exe`. Rename-first było netto GORSZE na jedynej ścieżce, która boli.
**(c) Jak — minimal closure (Fix-the-Authority):** autorytet „in use" = żywy pin;
wzmacniamy go świeżym odczytem tuż przed destrukcją zamiast ufać migawce sprzed
pętli. **Częściowe skasowanie MARTWEJ (nie-protected, nie-pinned) generacji jest
nieszkodliwe i self-healing:** to śmieć, `next_generation` (`:461`) i tak ignoruje
częściowy katalog, następny prune dokańcza `remove_dir_all`. Problem istniał TYLKO
dla żywej gen — a tę chroni pin (+P5). Zapisać w doc-komentarzu.
**Prove-the-branch (H4 — modelować mapped-exe, nie `File`):** test w `prep_tests.rs`
(szablon `prune_tolerates_an_undeletable_generation` `:392`): zapisać `state.json`
z żywym pinem `gen-N` (supervisor = własny pid, non-terminal), zawołać prune z
`protected` NIE zawierającym `N` → assert `gen-N` NIETKNIĘTE (removed nie zawiera go)
i przypięta ścieżka binarki dalej resolwuje; inne gen sprzątnięte. To pina, że
re-check chroni żywy pin nawet gdy `protected` go zgubił — dokładnie gałąź buga.
**(d) Dispatch:** `[opus]` core-implementer. **Review:** + proof-auditor (M1).

---

## Review

Jeden adwersarialny pass **`core-reviewer`** (`model:opus`) po całym rolloucie —
metoda ≠ implementera, class-keyed do taksonomii core. Atak na własne nowe seamy:
`stop_outcome` (`Forced`=clean poprawne?), `checkpoint_critical` (`_lock` czysto
zdejmowany? sustained-fail gap?), reorder `drop(control)` (thread nie dotyka
zwalnianego?), asymetria P5 (czy na pewno żywy nigdy nie odrzucony? SKEW dość duży?),
P4 re-check (czy TOCTOU faktycznie domknięty? częściowy-delete-martwej naprawdę
nieszkodliwy?).

**PLUS `proof-auditor`** scoped na **P5 i P4** (M1 — drugi trigger CLAUDE.md: „test/gate
sam jest powierzchnią ryzyka"): P5 pure-fn zastępuje nietestowane FFI-liveness; P4
test musi FAILować dokładnie tak jak bug (mapped-exe + resolvability przypiętej
ścieżki, nie „untouched OR trash-lingering"). Auditor sprawdza czy testy trafiają
gałąź, nie sąsiedztwo.

## Dispatch — tagi

| Krok | Lane | Uzasadnienie |
|------|------|--------------|
| S1 P1 handler ordering | `[opus]` core-implementer | kolejność lifecycle |
| S2 P2 teardown accuracy | `[opus]` core-implementer | autorytet raportu, pure seam |
| S3 P3 checkpoint fail-closed | `[opus]` core-implementer | fail-closed nad I/O Result |
| S4 P6 control-through-teardown | `[opus]` core-implementer | ordering teardown/control |
| S5 P5 Windows start-time (asym) | `[opus]` core-implementer | FFI, asym authority, pure conv |
| S6 P4 prune re-check | `[opus]` core-implementer | live-pin authority, no rename |

Sesja = Opus → `[opus]` = top-tier lane (osobny kontekst = granica reviewera). Każdy
krok osobno commitowany, trailer `Claude Opus 4.8`. Po całości — jeden core-reviewer
+ proof-auditor (P5/P4).

## Weryfikacja

`cargo test -p weles` (bez planu DB — bezpieczne, one-rollout-at-a-time). Nowe testy
pinują każdą wcześniej-błędną gałąź. `verifyctl --fast` NIE w tym planie (rollout z
DB) — warto raz odpalić blokującą `weles-fleet-parity` przy okazji.

## Poza zakresem (świadomie)

- **Bind control PRZED prep helperami (M2)** — DZIŚ `down` w ~60s oknie prep zwraca
  „very early startup" i nie zatrzyma floty; **M1 to okno wydłuża** (SQLite/porty/
  hello). To realny fix autorytetu (control-before-prep: helpery biegną do końca,
  ale `down` honorowany na boot-gate zamiast błędu). Odłożone/decyzja Lukasza:
  fold-in do S1 czy osobny krok. Zapisane, nie przemycone.
- **Sustained checkpoint failure w trakcie runu (M3)** — po pin-checkpoincie kolejne
  zapisy zostają best-effort; zepsuty state-dir MID-RUN degraduje cicho. Osobny gap
  (analog stale-delivery), nie w tym rolloucie.
- **Linux start-time epoch-math (P5)** — `/proc/<pid>/stat` starttime + `/proc/stat`
  btime + `sysconf(_SC_CLK_TCK)` → epoch to realna, nieprzetestowana arytmetyka bez
  prior-artu (processctl trzyma opaque StartMarker, nie epoch); known-gap, PID-only
  (over-protect benign).
- **Skrócenie ~30s okna detekcji martwego peera** (diagnoza B1) — osobna decyzja.
- **Concurrent-`deploy` lock / `weles rollback` CLI** — M1.

## Changelog

- **Rev 1 (2026-07-16, po grumpy plan-review Opus/think-hard):**
  - **H1** P5 symetryczne `|Δ|≤TOL` → **asymetryczne** `actual > recorded+SKEW`
    (symetryczne false-zabijało żywego wolno-startującego → retencja kasowała żywą gen).
  - **H2** dodany regresyjny test `classify` (live-but-slow → Connect).
  - **H3** P4 **rename-first WYRZUCONE** (na Windows przenosi katalog żywego exe →
    śmierć respawnu; gorsze niż dziś) → re-check live-pinu + tolerancyjny remove.
  - **H4** test P4 modeluje mapped-exe + resolvability przypiętej ścieżki (nie `File`
    handle, nie „untouched OR trash").
  - **kolejność** P5 przed P4 (P4 zależy od poprawnego liveness).
  - **M1** dodany proof-auditor scoped na P5/P4.
  - **M2/M3** zapisane w „Poza zakresem" z uzasadnieniem M1-amplifikacji.
  - **L1** P2: cytat dlaczego `Forced`=clean (console-less degraduje każdy stop do Forced).
  - **L2/L3** P1: orphan platform-asymetryczny; korzyść = anty-sierota, nie responsywność.
  - **L4** P6: reorder strukturalny, `DOWN_TIMEOUT`>teardown zweryfikowane.
- **Rev 2 (2026-07-16, po core-reviewer + proof-auditor na P5):**
  - **Zamknięta luka nietestowanego FFI-seamu (proof-auditor):** dodany
    `#[cfg(windows)]` test `supervisor_alive_is_false_for_a_reused_pid_through_real_getprocesstimes`
    prowadzi gałąź reuse→dead przez REALNE `GetProcessTimes` na żywym pid
    (`started_unix: 1`). Wcześniej PIERWOTNA gałąź (reuse→dead przez prawdziwe FFI)
    była pokryta tylko przez czystą `is_reused_pid` — under-read czasu utworzenia
    (odczyt wyzerowanego `exited`/`kernel` zamiast `created`, albo Hi/Lo swap w
    kierunku reuse) był NIEWIDOCZNY (`filetime_to_unix(0)=0 > recorded+5` = false →
    nie-reused → wszystkie testy nadal zielone). Test failuje, gdy FFI czyta złe
    pole FILETIME. Fix komentarza w `classify_connects_for_a_live_slow_start_supervisor`
    (mówił „well BEFORE now", kod ustawia `started_unix = now_unix()`; ćwiczony jest
    `actual_creation < recorded`). BEZ zmian logiki `supervisor_alive`.
  - **KNOWN RESIDUAL (core-reviewer L2):** utrwalony WSTECZNY skok zegara ściennego
    `> CREATION_SKEW` (5s), który wyląduje w podsekundowym oknie między utworzeniem
    procesu OS a przechwyceniem `unix_now()`, mógłby fałszywie uznać ŻYWEGO
    supervisora za martwego — jedyna teoretyczna ścieżka z powrotem do H1;
    akceptowalna, nazwana, skrajnie wąska.
  - **Linux:** P5 daje ZERO ochrony przed reuse na Linuksie (tylko pid; over-protection
    jest benign — nigdy w niebezpiecznym kierunku fałszywego zabicia żywego).
