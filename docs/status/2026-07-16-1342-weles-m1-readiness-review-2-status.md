# Weles — drugi przegląd gotowości do M1 (Claude + Codex, po P1–P6)

Data: 2026-07-16 13:42. Przegląd zamawiany po zamknięciu hardeningu **P1–P6**
(commity `2350006`..`78f6fed`, wszystkie na HEAD). Cel: czy P1–P6 są solidne i czy
weles nadaje się do wejścia w **M1** (pull contract: `ORCHESTRATOR_URL`,
`hello`/`resolve`, SQLite, minting portów, `weles rollback`).

Metoda: 4 niezależne adwersarialne passy (`core-reviewer` × supervisor / prep /
control+lock; `general-purpose` × M1-gap) + niezależny **Codex** (`codex exec
--effort high`, osobny kontekst). Każdy pass czytał kod linia-po-linii, nie
streszczenia. Twierdzenia pivotalne (sibling shutdown-Err) zweryfikowane ręcznie
przez głównego reviewera.

## Werdykt łączny: **READY-with-a-short-pre-M1-batch**

M0 jest strukturalnie solidne — **żaden seam nie wymaga przeróbki**, a logika P1–P6
jest poprawna (potwierdzone 4 niezależnymi passami). ALE rollout P1–P6 zostawił
**dwie realne luki**, które sam powinien był domknąć (obie klasy „sweep for
siblings" / „prove the branch" z CLAUDE.md), plus zestaw pozycji, które M1
**wzmacnia**. Codex dał `FIX-FIRST`, moje subagenty `essentially-clean +
should-fix-during-M1` — rozbieżność jest, jak w pierwszym przeglądzie, w ETYKIECIE,
nie w faktach: obie strony zgadzają się CO zrobić (domknąć okno startowe/kontrolę +
nietkniętą ścieżkę shutdown-Err), różnią się czy to „przed M1" czy „na starcie M1".

## Zgoda wszystkich passów — co trzyma (P1–P6 poprawne)

- **P2 `stop_outcome`** — `Forced`=clean poprawne semantycznie (`Forced` wraca DOPIERO
  po potwierdzonym wyjściu, `platform/mod.rs:160-167`); cztery kwadranty Ok/Err ×
  clean/unclean zgadzają się z exit-code i persist-terminal. Test `Err→(Failed,false)`
  realnie prowadzi wcześniej-błędną gałąź.
- **P3 `checkpoint_critical`** — fail-closed, `_lock` czysto zdejmowany na wczesnym
  `?`-returnie (żaden wątek jeszcze nie wystartował). Test kontrastuje swallow vs
  fatal na realnie niezapisywalnej ścieżce. **Najlepiej udowodniony fix.**
- **P4 prune re-check** — TOCTOU domknięty (świeży `live_pinned_generation` przez
  atomowy `state::load` w każdej iteracji); częściowe skasowanie MARTWEJ generacji
  jest self-healing (`next_generation` liczy z obecnych nazw katalogów). Test failuje
  dokładnie jak bug (gen-3/server.exe znika).
- **P5 asymetria** — jednostronne `actual > recorded+SKEW`; żywy wolno-startujący
  supervisor ma `actual ≤ recorded` → NIGDY false-dead. Pokrycie mutacyjne realne
  (zły odczyt pola FILETIME / Hi-Lo swap łapane przez testy). `lock.rs` bit-compat z
  processctl potwierdzony (`1<<63`, DACL, SDDL identyczne).
- **P6 drop-order** — `drop(poller)` przed teardownem, `drop(control)` po; żaden wątek
  nie czyta zwolnionego Arc; concurrent `status`/`down` trafia ŻYWY endpoint w oknie
  teardownu.

## Punch-list — luki do domknięcia PRZED / na starcie M1

### A. Pozostawione przez rollout P1–P6 (powinny były być domknięte — tani fix)

| # | Sev | Miejsce | Rzecz |
|---|-----|---------|-------|
| **A1** | **HIGH / should-fix** | `supervisor.rs:1042-1054` (`Directive::Kill`) | **Sibling P2 NIE zamieciony.** Zatrzymanie zawieszonej usługi woła `proc.shutdown(HUNG_GRACE,HUNG_FORCE)`; przy `Err` (force nie potwierdził wyjścia = możliwa sierota) tylko `eprintln!`, po czym BEZWARUNKOWO zapisuje docelowy `phase`/`status`. Dokładnie ta sama klasa co P2, ale w pętli monitora zamiast w teardownie. Skutek: osierocony proces trzyma port, tabela statusu pokazuje Backoff/Failed zamiast sygnału o niepotwierdzonym stopie. **Fix = ta sama autorytet: przepuścić przez `stop_outcome`, zapisać unclean gdy stop niepotwierdzony.** Potwierdzone ręcznym odczytem (nie tylko Codex). M1 (minting portów) wzmacnia „trzyma port". |
| **A2** | should-fix | `supervisor_tests.rs` (P1), `control_tests.rs:345-356` (P6) | **Proof-thinness — dwa najsłabiej udowodnione fixy.** P1 (reset `STOP` + handler-placement + orphan-deferral) NIE ma testu przerywającego w trakcie żywego helpera prep — regres kolejności przeszedłby suite. P6 test sprawdza tylko `classify(Stopping)==Connect`, nigdy nie konstruuje `ControlServer` ani nie odpala teardownu — cofnięcie `drop(control)` przed teardown DALEJ by przeszło. Oba potwierdzone przez Codex I supervisor-pass. Dodać regresje na zmienioną gałąź. |
| **A3** | low | `supervisor_tests.rs:394` | `stop_requested_honors_the_threaded_fleet_stop` milcząco zależy od procesowo-globalnego `STOP==false`; przyszły test ustawiający `STOP` sfleiku'je ten. Higiena (reset w setupie). |

### B. Skalujące — M1 je wzmacnia (should-fix-during-M1)

| # | Sev | Miejsce | Rzecz |
|---|-----|---------|-------|
| **B1** | **must-fix-before-replicas** | `supervisor.rs:963` (`observe`, `WaitingHealthy`) | **P7 — synchroniczny probe `/readyz` na wątku monitora** (~800ms/svc: 300ms connect + 500ms io). Po skoordynowanym respawnie R usług tick blokuje `Σ≈N×800ms` (12 svc ≈ 9.6s; 3× repliki ≈ 29s) — opóźnia detekcję crashów INNYCH usług i `down`. Znane-odroczone, OK dla samego kontraktu M0→M1, ale MUSI paść przed mintingiem portów replik. Fix: zdjąć probe z wątku monitora (poller już to robi dla fazy `Healthy`). |
| **B2** | should-fix | `main.rs:17` `DOWN_TIMEOUT=130s` | Stała odsprzężona od `fleet.len()×(STOP_GRACE+STOP_FORCE)`. Realny worst-case przy 12 svc = **120s** (nie „~110s" z komentarza) → margines ~10s, nie ~20s. Przy ≥14 usługach `wait_for_terminal` timeoutuje na LEGALNIE wolnym teardownie → fałszywy exit≠0. M1 dokłada usługi/repliki. Fix: wyliczać z rozmiaru floty + poprawić stały komentarz. |
| **B3** | should-fix | `prep.rs:259-278` (`validate_binaries`) | **sha256 zapisywany, nigdy nie weryfikowany na odczyt.** `validate_binaries` sprawdza tylko `is_file()`; skorumpowany/uciety `gen-N/*.exe` po flipie jest spawnowany, a zapisany hash bezczynny. To integralnościowa połowa `weles rollback` (M1). Fix w `validate_binaries`/`verify_generation`, nie w callerze. (+ B3′ opcjonalnie: `flip_current` bez fsync — power-loss może utrwalić rename przed bajtami; weryfikacja sha na odczyt łapie objaw.) |
| **B4** | should-fix (design) | `prep.rs:500` (`live_pinned_generation`) | Retencja zakłada **DOKŁADNIE JEDEN** żywy pin (jeden `state.json`). Poprawne pod one-up-at-a-time (rollout.lock). Gdy M1 dopuści nakładające się supervisory (blue/green), chroni tylko jeden z dwóch żywych genów. Rozstrzygnąć RAZEM z deploy-scoped lockiem (i tak M1). `protected: &[u64]` już jest slice'em — poszerzenie do zbioru lokalne. |

### C. Rezydualne / benign (nazwane, bez akcji w M0)

- **C1 — P5 wsteczny skok zegara >SKEW w podsekundowym oknie create→record** mógłby
  false-zabić żywą generację (jedyna ścieżka z powrotem do H1). Skrajnie wąskie,
  udokumentowane. Warte jednego zdania w ops-notes weles (nie tylko komentarz w kodzie).
- **C2 — P4 sub-window recheck→remove na Unix** — inherentne dla lock-free deploy,
  subsumowane przez M1 deploy-scoped lock. Bez zmian w M0.
- **C3 — Linux PID-only liveness** — over-protect benign. UWAGA M1: gdy `hello`/`resolve`
  wpuści ZDALNEGO callera, connect-time `SO_PEERCRED` (pid+uid) już nie backstopuje —
  zdalna autoryzacja M1 nie może się oprzeć na local peer-cred jak `status`/`down` dziś.

## Kluczowa decyzja dla Ciebie: **M2 — bind control PRZED helperami prep**

To pozycja, w której Codex mówi „hard blocker przed M1", a moje subagenty „early in
M1". Fakty (zgoda): między pierwszym checkpointem `Starting` (`supervisor.rs:657`,
`control_endpoint: None`) a bindem (`:708`) jest okno ~30–60s (`mint_ca` edgeca ~30s +
`seed_admin` PG round-trip). W tym oknie `weles down` NIE zatrzyma floty — `connect_target`
(`main.rs:120-136`) widzi brak endpointu, retry tylko `3×100ms`, zwraca mylące „very
early startup… try again in a moment" (realnie dziesiątki sekund), a helpery ignorują
`STOP` (Ctrl-C też odroczony). **M1 (init SQLite, minting portów, hello/resolve) to
okno WYDŁUŻA i dokłada punkty awarii.** Hardening-plan zapisał to jako świadomą
decyzję „fold-in czy osobno". Rekomendacja: **zbindować control przed wolnymi
helperami** (potrzebuje tylko `reporter.shared()`, już zbudowane) na starcie M1 —
zanim M1 dołoży pracę w tym oknie. To jest realny fix autorytetu (kontrola żyje przez
całe boot-gate), nie kosmetyka.

## Gotowość strukturalna do M1 (zgoda M1-gap + wszystkich passów)

Żaden seam M0 nie wymaga przeróbki; M1 rozszerza istniejące, czyste kształty:
- **rollback** = jeden verb CLI (generacje + `current` + sha256 już są, `prep.rs:211-218,
  477-484`).
- **hello/resolve** = dodać ramiona `match` do `response()` (`control.rs:294-320`) —
  ALE transport to dziś **serial, jednopołączeniowy operator-IPC** (named pipe/UDS),
  nie rejestr N usług; concurrency-model i osiągalność to realna praca. Uwaga:
  weles jest **std-only, bez tokio** — `axum` w Welesie łamie inwariant; blokujący
  mini-HTTP albo rozszerzony pipe/UDS zachowuje własność.
- **SQLite vs JSON** — decyzja napędzana WSPÓŁBIEŻNOŚCIĄ zapisu, nie storage: dziś
  single-writer (supervisor) → bezpieczny JSON; `hello`/`resolve`+minting wprowadzają
  N piszących → to jest sterownik SQLite. `rusqlite` dozwolony (external dep) ale
  potrzebuje `bundled` na Windows.
- **PUSH→PULL config** — najcięższa decyzja architektoniczna: inwersja wymaga, by
  `cmd/*-svc` maine (crate'y workspace) czytały `ORCHESTRATOR_URL` i wołały welesa;
  weles zero-sharing NIE poda klienta crate'om workspace → strona usługi potrzebuje
  własnego klienta (reqwest jest) lub skopiowanego kodeka. Dotyka „modules are
  topology-blind". **Rozstrzygnąć PRZED pisaniem kodu M1.**
- **Minting portów vs parity gate** — `weles-fleet-parity` (Blocking, pure in-memory,
  potwierdzony `stages/mod.rs:81-85`) asertuje statyczne porty = processctl. Minting
  wymaga przeramowania „statyczne porty" na „żądane domyślne" i redefinicji co gate
  asertuje — inaczej minting FAILuje `--fast`.

## Rekomendowana kolejność wejścia w M1

1. **A1** (sibling shutdown-Err w `Directive::Kill`) — tani, domyka inwariant „no
   orphan / accurate report", ta sama autorytet co P2. **+ sweep**: sprawdzić czy
   są inne miejsca wołające `proc.shutdown` poza teardownem/Kill.
2. **A2** (regresje P1/P6 na zmienioną gałąź) — domyka „prove the branch".
3. **Uruchomić raz `verifyctl --fast`** — jedyna nieodpalona end-to-end powierzchnia
   weryfikacji (gate `weles-fleet-parity` nigdy nie biegł na żywo na tej maszynie).
   To rollout z DB → one-at-a-time, sprawdzić brak aktywnego cargo/fleet najpierw.
4. **Decyzje projektowe PRZED kodem M1**: (a) PUSH→PULL przez granicę zero-sharing;
   (b) SQLite vs JSON (sterownik = współbieżni piszący); (c) M2 bind-before-prep
   (fold-in do startu M1).
5. **W trakcie M1**: B1 (P7 przed replikami), B2 (DOWN_TIMEOUT z rozmiaru floty),
   B3 (sha-verify-on-read = integralność rollbacku), B4 (retencja jako zbiór pinów
   przy deploy-locku).

## Rozbieżność Claude vs Codex (uczciwie)

Codex: `FIX-FIRST`, hard-blocker = „ustanowić lokalną kontrolę + własność
terminal-state PRZED SQLite/hello/resolve/portami". Moje subagenty: seamy zastępowalne
bez chirurgii, luki na rzadkich ścieżkach błędu → `should-fix-during-M1`. **Fakty
zbieżne w 100%** (A1 potwierdzony ręcznie; M2 okno potwierdzone przez oba). Różnica
to próg nazewniczy. Synteza: A1 + A2 zrobić PRZED M1 (tanie, domykają P1–P6); M2 +
decyzje projektowe to pierwszy krok M1, nie warunek wstępny — ale **nie wchodzić w
SQLite/hello/resolve zanim M2 (bind-before-prep) nie jest rozstrzygnięte**, bo M1
dokłada pracę dokładnie w tym niekontrolowalnym oknie.
