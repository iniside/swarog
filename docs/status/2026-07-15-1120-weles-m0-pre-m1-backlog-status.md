# Weles M0 → M1: backlog przed-M1 + odkryte bugi (status)

Stan po zamknięciu rolloutu Weles M0 (2026-07-15; plan z erratą:
[2026-07-15-1055-weles-m0-plan.md](../plans/2026-07-15-1055-weles-m0-plan.md)).
Kierunek zatwierdzony przez Lukasza; NIC z poniższego nie jest rozpoczęte.

> **ERRATA 2026-07-15 (po diagnozie B1):** punkt 1 backlogu („stub re-dial —
> NAJPIERW") jest **bezprzedmiotowy jako rollout backendowy** — B1 nie
> reprodukuje się, hipoteza transportowa obalona (patrz
> [2026-07-15-1745-b1-stub-redial-diagnosis-status.md](2026-07-15-1745-b1-stub-redial-diagnosis-status.md),
> commity `cde5282`/`db0d65b`, pamięć
> `memory/edge-stub-no-reconnect-after-peer-restart.md`). Realny backlog
> zaczyna się od **punktu 2 (control endpoint przed bootem)**. Numeracja
> poniżej zachowana historycznie; pkt 1 przekreślony, nie usunięty.

## Backlog przed-M1 (kolejność wiążąca)

1. ~~**Stub re-dial po zmianie inkarnacji peera — NAJPIERW, rollout
   backendowy.**~~ **ZAMKNIĘTY jako bezprzedmiotowy (2026-07-15).** Warstwa
   połączenia (`core/remote`/`core/edge`) odzyskuje się poprawnie po restarcie
   peera w obu kształtach teardownu; brak trwałego 404 na żywym splicie (jeden
   call wisi ~30 s → 408, potem 200). Własność recovery przypięta testami
   regresyjnymi (`cde5282`). Restart-on-crash Welesa NIE jest blokowany. Jeśli
   trwałe 404 wróci — pierwszy podejrzany to dane/środowisko (gałąź D), nie
   transport. Ewentualna asercja splitproof `[B1-REDIAL]` pozostaje opcjonalna
   (nie dyskryminuje fixed/unfixed).
2. **Control endpoint bindowany PRZED bootem** (dziś: po fleet-healthy,
   `weles/src/supervisor.rs` — bind po pętli boot). `down`/`status` muszą działać
   na flocie utkniętej w boocie; pętla boot już sprawdza STOP, więc zmiana jest
   mała. Bonus: awaria bindu failuje `up` zanim cokolwiek wstanie.
3. **Post-healthy monitoring `/readyz` → stan `Degraded`/`NotReady` — NIGDY
   auto-restart na 503.** `/readyz` agreguje ping DB + liveness planów (+ stuby w
   gateway); mrugnięcie Postgresa flipuje 11 svc naraz — auto-restart na
   readiness = restart storm, który niczego nie naprawia. Restart pozostaje
   przywiązany wyłącznie do śmierci procesu. Wymaga budżetu sond per tick
   (round-robin) — synchroniczny probe kosztuje do ~800ms/svc (finding Low #1
   z review Step 5).
4. **Generacje deployu:** `deploy/gen-N/` + atomowe przełączenie wskaźnika
   `current` + hashe artefaktów. Zamyka: obserwowalność częściowego deployu,
   windowsową asymetrię nadpisywania żywych exe (nowa generacja nie dotyka
   plików floty), rollback = przestawienie wskaźnika; fundament rolling deployu.
   Strażnik zakresu: to NIE jest package manager — bez podpisów, remote fetch,
   rejestru wersji.
5. **Dyscyplina narracji:** dzisiejszy manifest to świadomie flavor DEVELOPMENT
   (statyczne porty, lokalny Postgres, dev-seedy, admin/admin — parytet
   fleet.rs). Weles = supervisor środowiska developerskiego z produkcyjnym
   szkieletem (containment, lock, control plane), nie „mały Nomad". M1 nie może
   przemycać produkcyjnych knobów do devowej ścieżki ani odwrotnie.

## Odkryte bugi (wszystkie POZA Welesem, żaden nienaprawiony)

### B1 — backend: stub module→module nie robi re-dial po restarcie peera (ZAMKNIĘTY 2026-07-15 — NIE reprodukuje się, hipoteza transportowa obalona)
> **Rozstrzygnięcie:** diagnoza wykazała, że warstwa połączenia odzyskuje się
> poprawnie po restarcie peera (oba kształty teardownu → `ConnectionFatal` →
> recovery); żywy split nie produkuje trwałego 404 (call wisi ~30 s → 408,
> potem 200). Oryginalna obserwacja „trwałe 404" była artefaktem środowiska
> tamtej sesji chaosu. Pełna diagnoza + dowody:
> [2026-07-15-1745-b1-stub-redial-diagnosis-status.md](2026-07-15-1745-b1-stub-redial-diagnosis-status.md).
> Poniższy opis zachowany historycznie (stan wiedzy z chwili odkrycia).

Znaleziony przez chaos-test Welesa (Step 7.3): kill + auto-restart
characters-svc pod żywym ruchem — topologia, której nic wcześniej nie
produkowało (devctl przy crashu ubija całą flotę; splitproof `rdy_dead`
asertuje tylko recovery readyz **gatewaya**).

- **Objaw:** po restarcie characters-svc (ten sam port :9000)
  `GET /inventory/{cid}` zwraca **404 trwale** — także dla postaci utworzonych
  PO restarcie; readyz inventory zielony, zero błędów edge w logach (cicha
  ścieżka mapowania błędu na 404).
- **Dowody:** durable write działa (starter grant wylądował w
  `inventory.holdings` w DB); create character 201 przez gateway→characters
  działa po restarcie (klient gatewaya się odzyskuje) — pada wyłącznie sync-owy
  authz `Ownership` inventory→characters po wewnętrznym QUIC edge. Asymetria:
  ścieżka gateway re-dialuje, ścieżka konsumenta module→module nie.
- **Fix:** przy autorytecie w `core/remote`/`core/edge` (reconnect-on-dead-
  connection dla stubów konsumenckich) + splitproof assertion j.w. Po fixie
  powtórzyć scenariusz chaos Welesa.
- Pamięć: `memory/edge-stub-no-reconnect-after-peer-restart.md`.

### B2 — devctl: flake testu pod pełną równoległością workspace
`tools/devctl/src/tests.rs:333` —
`down_waits_for_stopped_checkpoint_and_reports_failed_cleanup` padł w stage
`test` `verifyctl --fast` (assertion `wait_for_terminal(...).is_ok()`, padł w
0.20s — nie timeout), a przechodzi w izolacji i 2× w całej paczce devctl
(18/18). Podejrzenie: interferencja cross-package przy pełnym `cargo test
--workspace` (nowe procesowe testy weles równolegle z testami tożsamości
procesów devctl) — hipoteza NIEPOTWIERDZONA. Skutek: finalny `verifyctl --fast`
rolloutu M0 miał 1 FAIL (cała reszta, w tym split-proof, PASS). Klasa:
timing-sensitive-tests doctrine („full-workspace cargo test is the fragility
detector").

### B3 — processctl: luka `BUILD_ENV_ALLOWLIST` (utajona, jednoliniowa)
`tools/processctl/src/fleet.rs:8-14` — allowlista buildu nie zawiera
`SYSTEMDRIVE` ani `ProgramData`, a auto-detekcja MSVC rustca (gdy `link.exe`
nie jest w PATH — jak na tej maszynie) wymaga co najmniej jednej z nich.
Potwierdzone bisekcją w izolacji: sam `SYSTEMDRIVE` albo sam `ProgramData`
przywraca linkowanie. devctl'owi uchodzi, bo jego filtrowany build niemal
zawsze trafia ciepły cache (artefakty pre-zbudowane pełnym env) — linker nie
jest wołany. Ujawnione przez (usunięty już) build w weles; odnotowane też w
doc `weles/src/prep.rs`.

## Zaległości pozostałe z M0
- `verifyctl --fast` nie jest zielony wyłącznie przez B2.
- M1 właściwe (kontrakt hello/resolve, SQLite, minting portów, repliki z
  prerekwizytem replica-safety: rating→DB, advisory lock na relay) — osobny
  plan, po backlogu przed-M1.
