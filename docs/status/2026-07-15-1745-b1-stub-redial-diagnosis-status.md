# B1 (stub re-dial po restarcie peera) — diagnoza: NIE reprodukuje się, hipoteza transportowa obalona

Zamknięcie diagnostyczne buga B1 z
[2026-07-15-1120-weles-m0-pre-m1-backlog-status.md](2026-07-15-1120-weles-m0-pre-m1-backlog-status.md)
wg planu
[2026-07-15-1536-b1-stub-redial-after-peer-restart-plan.md](../plans/2026-07-15-1536-b1-stub-redial-after-peer-restart-plan.md)
(tabela decyzyjna; errata po Step 1). Decyzja Lukasza: diagnoza, zero fixów
(„jak się zjebie — raportuj, nie poprawiaj").

## Wynik w jednym zdaniu

Warstwa połączenia (`core/remote`/`core/edge`) odzyskuje się poprawnie po
restarcie peera w OBU kształtach teardownu (graceful close i twardy kill), a
scenariusz chaosu odtworzony na żywym splicie Welesa **nie reprodukuje
trwałego 404** — obserwowany przebieg to: jeden request wiszący ~30 s ścięty
jako **408** (okno detekcji idle × `HTTP_REQUEST_TIMEOUT_MS`), potem **200**
na stałe. Kod produktu nie zmienił się od chaosu M0 (dzisiejsze commity to
testy i docs), więc oryginalna obserwacja „trwałe 404" była najpewniej
artefaktem środowiska tamtej sesji chaosu, nie bugiem transportu.

## Dowody

1. **Repro jednostkowe/integracyjne (commit `cde5282`,
   `test(remote): pin stub redial recovery...`):**
   - graceful close (`core/remote/src/redial_tests.rs`): 1 iteracja
     `ConnectionFatal` („closed by peer") → reset → recovery; przy
     `OnceAfterReconnect` recovery w tym samym callu (reset+replay);
   - twardy kill bez ramki CONNECTION_CLOSE
     (`core/remote/tests/abrupt_kill_redial.rs`, helper-peer w procesie
     potomnym, TerminateProcess, rebind tego samego portu): pierwszy call
     blokuje ~30 s w `open_bi` (keepalive 5 s bez odpowiedzi →
     `CLIENT_IDLE_TIMEOUT_MS` 30 s uznaje połączenie za stracone) →
     `ConnectionLost` → **ConnectionFatal** → reset → recovery.
     **StreamLocal-pinning nie występuje** — podejrzenie z planu (gate
     `Reconnecting::call` resetujący tylko ConnectionFatal) jest na tej
     ścieżce bezprzedmiotowe.
2. **Żywy split pod Welesem (12 svc, restart-on-crash), 2026-07-15 ~17:42:**
   - priming: postać pre-kill + `GET /inventory/character/{cid}` → 200
     (żywe, cache'owane połączenie stuba inventory→characters);
   - `Stop-Process -Force` characters-svc → weles respawn w ~2 s (restarts 1);
   - create postaci PO restarcie: jeden **503** (gateway evictował własnego
     martwego klienta edge — mechanizm `!is_definitive_answer`), potem **201**;
   - `GET /inventory/{char2}` i `{char1}` → **200** (bez żadnego 404);
   - druga runda z ciasnym 1-sekundowym pollingiem od momentu killa:
     `+30.0s → 408` (request wiszący w oknie detekcji, ścięty przez
     `HTTP_REQUEST_TIMEOUT_MS=30000` inventory), `+31.1s → 200`. Trwałe
     recovery, restarts 2, logi err puste.

## Co z tego wynika

- **Backlog przed-M1 pkt 1 („stub re-dial — NAJPIERW") jest bezprzedmiotowy
  w zaplanowanej formie** — nie ma czego fixować w `core/remote`/`core/edge`;
  restart-on-crash Welesa NIE jest blokowany przez warstwę połączenia.
  Własność recovery jest teraz przypięta testami regresyjnymi (`cde5282`).
- Planowany Step 2 (poszerzenie gate'u resetu / evict-without-close) NIE
  wchodzi jako fix — mógłby wejść wyłącznie jako świadome wyrównanie semantyki
  z gatewayem; na dziś odłożony (minimal sufficient closure).
- Planowany Step 3 (asercja splitproof `[B1-REDIAL]`) pozostaje wartościowy
  jako committed dowód recovery-on-split (uczciwie: nie dyskryminuje
  fixed/unfixed) — do decyzji, czy dokładać go teraz, czy przy najbliższej
  pracy w splitproof.
- **Nota operacyjna:** okno detekcji martwego peera to ~30 s, w którym call
  na martwym połączeniu WISI (potem 408/503). Dla chaosu/restartów oznacza to
  do ~30 s degradacji pojedynczej ścieżki module→module po twardym killu.
  Ewentualne skrócenie (per-call deadline w kliencie edge / krótszy idle) to
  osobna, świadoma decyzja — nie bug.
- Jeśli trwałe 404 z oryginalnej sesji chaosu (Step 7.3 M0) wróci, pierwszy
  podejrzany NIE jest transport: szukać w danych/środowisku tamtej sesji
  (id postaci, env respawnu, stan DB) — patrz gałąź (D) tabeli decyzyjnej
  w planie.

## Artefakty

- Testy: `core/remote/src/redial_tests.rs`,
  `core/remote/tests/abrupt_kill_redial.rs`, seam
  `test_only_reconnecting_edge_caller` w `core/remote/src/lib.rs`
  (commit `cde5282`; review: core-reviewer, 6 punktów low/low-med naniesionych).
- Plan z erratami: `docs/plans/2026-07-15-1536-b1-stub-redial-after-peer-restart-plan.md`.
- Logi pollingu z żywego splitu: scratchpad sesji (nietrwałe, wyniki
  przepisane wyżej).
