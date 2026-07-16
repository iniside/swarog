# Weles pre-M1 backlog (#2-#5) — UKOŃCZONE

Zamknięcie rolloutu backlogu przed-M1 wg planu
[2026-07-15-1840-weles-pre-m1-backlog-plan.md](../plans/2026-07-15-1840-weles-pre-m1-backlog-plan.md)
(rev 1 po grumpy review). Wszystkie cztery części wdrożone, każda z niezależnym
adwersarialnym passem; C i D dostały follow-upy domykające realne defekty
autorytetu wykryte na własnych seamach fixu. Backlog #1 był wcześniej zamknięty
jako bezprzedmiotowy (B1 nie reprodukuje się —
[diagnoza](2026-07-15-1745-b1-stub-redial-diagnosis-status.md)).

## Co wylądowało (łańcuch commitów)

| Commit | Część | Treść |
|--------|-------|-------|
| `52f3cb7` | A (#2) | control endpoint bindowany PRZED bootem; `fleet_stop` jako `Arc` (usunięty `CONTROL_STOP` OnceLock); `down`/`status` działa na flocie w boocie |
| `d82d551` | B (#3) | readiness poller na osobnym wątku; wymiar `Readiness` (Degraded/Unreachable/Ready) ortogonalny do restartu — mrugnięcie PG nie restartuje floty |
| `205b38a` | C (#4) | generacje deployu: `deploy/gen-N/` + `current` pinowany-at-discover + SHA-256 manifest; atomowy flip; retencja |
| `ff4ba7f` | C follow-up | retencja chroni generację przypiętą przez ŻYWEGO supervisora (autorytet: pozycja-numeryczna → live-pin-by-name); nowe pole `state.pinned_generation` |
| `6e33336` | C follow-up | pin zapisany PRZED helperami prep (mint_ca/seed_admin), nie po — zamyka okno niewidoczności live-pinu dla równoległego deploy |
| `ff88c09` | D (#5) | BLOKUJĄCA stage `weles-fleet-parity` w verifyctl: znormalizowany diff weles↔processctl (name/porty/has_db/pool/pełny composed env) |
| `9495322` | D follow-up | bramka sprawdza też budżet PG (`dedicated`/`PG_SESSION_BUDGET`) i `SERVICE_ENV_ALLOWLIST`; `ENV_EXCLUSIONS` derywowane z allowlisty |

## Rozstrzygnięcia autorytetu (przed implementacją, po grumpy review)

- **C — pin-at-discover, nie per-call:** cała flota jednego `up` biegnie jedną
  spójną generacją; per-call `current` przy respawnie dałby mieszaną generację.
- **C — retencja live-pin-by-name:** przy one-up-at-a-time żyje najwyżej jeden
  supervisor przypinający jedną generację; deploy czyta `state.json` +
  `supervisor_alive` i chroni ją. Na Uniksie `remove_dir_all` na biegnącym exe
  cicho się udaje → to był realny HIGH (psuł crash-respawn), teraz zamknięty.
- **B — readiness na osobnym wątku:** synchroniczny probe (~800ms) na wątku
  monitora blokowałby detekcję crashów innych svc i `down`. `step()`/`observe()`
  dla Healthy nietknięte — probe typowo niezdolny dotknąć restartu.
- **D — bramka blokująca + pełny znormalizowany diff:** weles NIE jest ćwiczony
  przez split-proof, więc to jego jedyny gate parytetu. Zamyka
  didnt-forget-scripts-must-self-check: wszystkie „Mirrors fleet.rs" stałe
  (porty, env, budżet PG, allowlist) mają teraz maszynowy cross-check zamiast
  komentarza.

## Weryfikacja

- `cargo build --workspace` — czysto (zmiany `pub` w processctl bez skutków).
- `cargo test -p weles` — 114 testów (102 lib + 1 bin + 6 platform + 5 prep
  integration), 0 failed. Wcześniejszy niepokój o flaky graceful-vs-forced
  shutdown rozwiązany (6/6 platform zielone).
- `cargo test -p verifyctl weles_fleet_parity` — 12/12 (pass-at-HEAD +
  fail-on-drift dla każdego porównywanego pola).
- Parytet weles↔processocessctl przy HEAD: **pełny, zero latentnego dryfu**
  (dedicated 4/4·5·0, `PG_SESSION_BUDGET`=87 obie strony, allowlist identyczna).
- NIE uruchamiano `verifyctl --fast`/`--all` (split-proof = rollout z DB);
  parytet jest czysto in-memory, więc walidowany testem jednostkowym.

## Znane, świadomie odłożone

- **Stale `control_endpoint` podczas teardownu** (pre-existing, nie z tego
  rolloutu): concurrent `status`/`down` w oknie teardownu trafia martwy
  endpoint. Trywialny fix (`set_control_endpoint(None)` po `drop(control)`),
  gdyby doskwierał.
- **`verifyctl --fast` z nową blokującą stage'ą** nie był odpalony end-to-end na
  tej maszynie (ciężki rollout z DB). Stage jest zarejestrowana w BLOCKING,
  frozen-manifest test zaktualizowany (16→17), testy jednostkowe zielone.
- **Concurrent `weles deploy`** (bez locka, by design) — udokumentowane jako
  dyscyplina operatora „one deploy at a time"; deploy-scoped guard = M1.
- **~30s okno detekcji martwego peera** (z diagnozy B1) — osobna decyzja, nie bug.

## Dalej: M1 właściwe

Kontrakt hello/resolve, SQLite, minting portów, repliki (prerekwizyt
replica-safety: rating→DB, advisory lock na relay) — osobny plan. Agent name
`rarog` zarezerwowany.
