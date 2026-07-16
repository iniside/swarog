# Weles — przegląd gotowości do M1 (Claude + Codex)

Dwa niezależne adwersarialne przeglądy `weles/` po zamknięciu backlogu #2–#5:
- **Claude (Opus 4.8)** — ręczny read seamów #2–#5 (`supervisor.rs`, `prep.rs`,
  `control.rs`) + weryfikacja bramki parytetu w verifyctl.
- **Codex (gpt-5.1-codex, effort high)** — osobny kontekst, `codex exec`.

**Werdykt łączny: READY-with-punch-list** — architektura i happy-path są solidne
(M0 stoi), ale Codex trafnie wypunktował klaster defektów na ścieżkach
błędu/kolejności, których mój pierwszy pass niedoważył. Żaden nie jest twardym
blokerem normalnej ścieżki, ale kilka jest tanich i M1 je **wzmacnia** (więcej
pracy startowej, repliki, większe poleganie na generacjach) — zrobić je PRZED /
na starcie M1, nie ślepo wchodzić w M1.

Rozbieżność werdyktów: Claude dał wstępnie READY (przegląd autorytetów
happy-path), Codex dał FIX-FIRST (młotek na error-swallowing + kolejność).
Codex miał rację co do faktów — wszystkie strukturalne twierdzenia potwierdzone
w kodzie linia-po-linii. Owning: mój pierwszy pass sprawdził, że autorytety
happy-path są poprawne, ale nie uderzył wystarczająco mocno w ścieżki
połykania błędów i kolejność instalacji — dokładnie ta klasa, którą taksonomia
core każe atakować w pierwszej kolejności.

## Co jest solidne (zgoda obu)

- **#2 fleet_stop Arc** — własność control-serwera, `STOP`(sygnał)⊥`fleet_stop`
  (control) rozdzielone; brak nawrotu OnceLock; jeden teardown per ścieżka.
- **#3 readiness ⊥ restart** — `fold_readiness`/`readiness_for`/`next_probe_index`
  strukturalnie bez dostępu do `phase`/`history`/`Directive`; 503 nigdy nie
  restartuje. Poller na osobnym wątku, własny `shutdown` (nie fleet-stop).
- **Lock bit-compat** (bajt `1<<63`, DACL) — Codex nie znalazł defektu.
- **state.json tmp→rename** single-writer — bez usterek.
- **#5 parytet** — bramka `weles-fleet-parity` potwierdzona jako `Blocking`
  (`tools/verifyctl/src/stages/mod.rs:83`).

## Punch-list (triaż wg mojego osądu, nie surowy relay Codeksa)

| # | Sev (mój) | Miejsce | Rzecz | M1 wzmacnia? |
|---|-----------|---------|-------|--------------|
| P1 | SHOULD-FIX | `supervisor.rs:626` | `install_ctrl_handler()` **po** helperach prep (mint_ca ~30s + seed_admin ~30s). Ctrl-C w ~60s oknie prep na świeżym checkoucie → domyślna dyspozycja OS, nie orderly teardown; może osierocić edgeca/adminctl. **Tani fix: przenieść handler zaraz po locku.** | tak |
| P2 | SHOULD-FIX | `supervisor.rs:1008-1014` | teardown ustawia `Stopped` **bezwarunkowo** nawet gdy `proc.shutdown()` zwróci `Err` (force-kill nieudany, proces żyje). `weles down` raportuje sukces mimo osieroconego procesu — łamie „report accurately / no orphans". | — |
| P3 | SHOULD-FIX | `supervisor.rs:518, 599` | wczesny pin-checkpoint (teraz **safety-critical** dla retencji #4) połyka błąd zapisu (`eprintln`, best-effort). Zapis fatal-na-fail dla TEGO checkpointu (albo jawny fallback), inaczej awaria zapisu + równoległy deploy = usunięta żywa generacja. | tak |
| P4 | NICE→SHOULD | `prep.rs:528-559` | Windows `remove_dir_all` nie jest transakcyjny: może skasować manifest + odblokowane pliki ZANIM padnie na zablokowanym `.exe`; „skipped" loguje sukces mimo częściowego zniszczenia. Wyzwalane tylko gdy ochrona pinu zawiodła (P3), ale osłabia całą obietnicę bezpieczeństwa #4. | tak |
| P5 | NICE-TO-HAVE | `control.rs:160-185, 421` | `started_unix` sprawdzany tylko „nie z przyszłości", nigdy porównany z realnym czasem startu procesu → PID-reuse. Retencja: kierunek bezpieczny (nadmierna ochrona). classify: próba martwego endpointu (pada czysto). | — |
| P6 | NICE-TO-HAVE | `supervisor.rs:698-701, teardown` | stale `control_endpoint` w oknie teardownu (~do 120s) — checkpoint `Stopping` trzyma `Some(endpoint)`, control już zdropowany → concurrent `status`/`down` trafia martwy endpoint. **Znane** (completion status). Fix: `set_control_endpoint(None)` po `drop(control)`. | — |
| P7 | NICE-TO-HAVE | `supervisor.rs:884-892` | probe `WaitingHealthy` w `observe()` jest **synchroniczny na wątku monitora** (do ~800ms/svc). Kilka jednoczesnych respawnów opóźnia detekcję crashów i stop. `#3` zdjął z wątku monitora TYLKO post-healthy poller; boot/respawn probe został. | **tak** (repliki) |
| P8 | odrzucone/obniżone | `supervisor.rs:599-624` | Codex: „pre-bind fail zostawia live-looking state". W praktyce pid-liveness gate (`supervisor_alive`) i tak raportuje martwego supervisora → status stale/dead, retencja nie chroni (pid martwy). Kosmetyk, nie „live-looking". | — |

**Nie zgadzam się z Codeksem** co do kwalifikacji 1/2/3 jako twardych
BLOCKER-for-M1: każdy wymaga rzadkiego warunku (dwa równoległe deploye /
awaria zapisu checkpointu / nieudany force-kill), a normalna ścieżka degraduje
do „niedokładny status" albo „operator ponawia", nie utrata danych ani zwis.
Ale klaster dotyka dokładnie deklarowanych inwariantów (dokładny raport, brak
sierot, bezpieczeństwo pinu), więc traktuję je jako wymagany pre-M1 hardening,
nie jako „zignoruj".

Codex-finding #1 (BLOCKER) w części dotyczy DWÓCH równoległych deployów — to już
udokumentowana dyscyplina operatora (one-deploy-at-a-time, guard = M1). Realna
resztka to P3 (połknięty błąd safety-checkpointu), którą wyodrębniłem.

## Rekomendacja

Kolejność: **P1 → P2 → P3 → P4** (tanie, dotykają dokładności/bezpieczeństwa i
M1 je wzmacnia), potem **P7** wraz z wejściem replik w M1, a **P5/P6** przy okazji
(trywialne). P8 pominąć.

Nie uruchamiano `verifyctl --fast`/`--all` (rollout z DB, one-at-a-time). Nowa
blokująca stage `weles-fleet-parity` nie była odpalona end-to-end na tej maszynie
— to jedyna czysto-weryfikacyjna zaległość z completion statusu, warta jednego
przebiegu przed zamknięciem fazy.
