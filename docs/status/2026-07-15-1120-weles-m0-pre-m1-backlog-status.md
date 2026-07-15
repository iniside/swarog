# Weles M0 → M1: backlog przed-M1 + odkryte bugi (status)

Stan po zamknięciu rolloutu Weles M0 (2026-07-15; plan z erratą:
[2026-07-15-1055-weles-m0-plan.md](../plans/2026-07-15-1055-weles-m0-plan.md)).
Kierunek zatwierdzony przez Lukasza; NIC z poniższego nie jest rozpoczęte.

## Backlog przed-M1 (kolejność wiążąca)

1. **Stub re-dial po zmianie inkarnacji peera — NAJPIERW, rollout backendowy.**
   Restart-on-crash Welesa jest niewiarygodny, dopóki backend nie traktuje
   restartu pojedynczego peera jako normalnego stanu pracy (bug #1 niżej).
   Fix w warstwie połączenia (`core/remote`/`core/edge` client) — świadomie NIE
   czeka na M1-owy resolve (własność connection-layer, potrzebna w obu światach).
   Obowiązkowo: committed splitproof assertion „wywołanie module→module po
   restarcie peera".
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

### B1 — backend: stub module→module nie robi re-dial po restarcie peera (OTWARTY, krytyczny dla restartów)
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
