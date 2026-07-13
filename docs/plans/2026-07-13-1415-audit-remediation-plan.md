# Plan naprawczy: audyt 2026-07-13 — gate'y, poison-klasa, timeouty, budżety

> Po zatwierdzeniu: kopia do `docs/plans/2026-07-13-HHMM-audit-remediation-plan.md` (pierwsza czynność implementacji).

## Context

Zewnętrzny audyt (Codex) zgłosił ~16 defektów. Zweryfikowałem WSZYSTKIE w kodzie (14 potwierdzonych, 1 zdegradowany z korektą kontraktową, 1 z niuansem), a 13 subagentów researchowych (sonnet) dostarczyło projekt każdej naprawy z file:line. To 5. dzień remediacji — plan zamyka **authority**, nie objawy, i naprawia w pierwszej kolejności gate'y, którym dziś nie można ufać (martwy archcheck rule 9, topiccheck blokujący legalne v2, splitproof ufający staremu listenerowi, golden ślepy na serde, devctl/splitproof mijające się z CARGO_TARGET_DIR).

Kluczowe odkrycia researchu zmieniające podejście względem sugestii audytu:
- **HTTP drain (T1)**: nie trzeba ręcznej accept-pętli — ścieżka TLS w TYM SAMYM pliku (`serve_https`, axum-server 0.8) już ma poprawny ownership connection-tasków z hard-abortem po grace. Fix = przełączenie plain-HTTP na `axum_server::from_tcp` + `Handle::graceful_shutdown`. Zero nowych zależności.
- **asyncmig (T2)**: lock jest transakcyjny (`pg_advisory_xact_lock`), więc wystarczy `SET LOCAL lock_timeout='60s'` w samym DDL — prostszy mechanizm niż w lifecycle (tam lock sesyjny wymusza dedykowaną konekcję + RESET).
- **Retention (R1)**: fold floora jako `NOT EXISTS` w DELETE eliminuje wyścig CAŁKOWICIE (nie tylko między batchami) i scala oba ramiona SQL — lepsze niż per-batch recompute. Lock catalog↔GC (propozycja audytu) odrzucony: MinRetention kontraktowo obiecuje tylko `days` (retention.rs:297-298).
- **Envelope `code` (D5)**: to UDOKUMENTOWANE ODWRÓCENIE decyzji z planu 2026-07-11-1602 (Step 7 jawnie odrzucił Option C) — wymaga errata-note w tamtym planie + nazwania w commit message (Fix-the-Authority pkt 4). Uzasadnienie odwrócenia: potwierdzony false-positive (handler propagujący cudzy unknown-method przez `?` re-stampuje sentinel).
- **make_interval**: sufit zweryfikowany empirycznie na żywym PG18 = **9 223 372 036 854 s**.

Proces (uzgodniony): review planu wykonany (grumpy-senior fable think-hard + pomocniczy sonnet; Codex-job martwy — odnotowane); implementacja per krok, model subagenta nazwany wprost w tagu kroku (`[opus + think hard]` / `[sonnet]` — nigdy fable); commit po każdym kroku; **adversarial review każdego commitu: OPUS + CODEX**.

## Findings → kroki (mapa)

| ID | Defekt (anchor) | Krok |
|----|-----------------|------|
| G1 | archcheck rule 9 martwy (main.rs:345 `Kind::Other` vs `Kind::Core("bus")`) | 1 |
| G2 | topiccheck panic na v1+v2 (tests.rs:264 klucz=topic) | 2 |
| G3 | splitproof bez liveness childa (main.rs:89) + bez finalnego sweepa | 4 |
| G4 | CARGO_TARGET_DIR (devctl supervisor.rs:721, splitproof main.rs:345) | 3 |
| G5 | serde wire-shape niewidoczny (golden.rs:221) | 5 |
| P1 | inventory quantity int4 + brak górnej walidacji (inventory lib.rs:62,177,366) | 6 |
| P2 | scheduler interval > sufitu make_interval zabija skan (scheduler lib.rs:136,151) | 7 |
| T1 | HTTP drain porzuca connection-taski (app lib.rs:968) | 8 |
| T2 | asyncmig bez lock_timeout (store.rs:52) | 9 |
| T3 | credential admission bez deadline (keys.rs:294, verifier.rs:113) | 10 |
| T4 | remote boot-fill bez deadline (remote lib.rs:489) | 11 |
| B1 | brak budżetu połączeń PG (app lib.rs:591, fleet.rs) | 12 |
| D2 | RATE_LIMIT_BURST=0 asymetria (app lib.rs:857, player.rs:367) + siblingi invalidation env | 13 |
| D1 | route overlap /x/{id} vs /x/me (gateway lib.rs:561) | 14 |
| D3 | Epic link-start Err→login (epic_oauth.rs:264) + sibling :326 | 15a |
| D4 | adminctl username drift (adminctl lib.rs:48) | 15b |
| D5 | UnknownMethod text-sniffing (client.rs:133) | 16 |
| D6 | docs: JVM gateway.md jako current, hetzner ORDER BY id | 17 |
| R1 | retention floor snapshot (retention.rs:258) | 18 |

## Kroki

Konwencja: każdy krok = osobny commit (Conventional Commits), po nim adversarial review JA+CODEX, dopiero potem następny krok. Weryfikacja krokowa wg `safe-verification` (jeden rollout naraz); pełny `verifyctl --all --strict` po fazach A, C i na końcu.

---

### FAZA A — gate'y (najpierw, bo dopóki kłamią, żaden kolejny fix nie jest pewny)

**Step 1 — archcheck rule 9: ożywić martwą regułę** `[sonnet]`
- (a) `tools/archcheck/src/main.rs:345` + `tools/archcheck/src/tests.rs:247-278`.
- (b) Dlaczego teraz: gate chroniący seam AnyTx jest zielony z definicji od `70674db`; wszystkie późniejsze kroki polegają na archcheck.
- (c) Jak: wyekstrahować regułę do czystej fn `core_bus_sqlx_violations(&[Value]) -> Vec<String>` (wzorzec: `missing_svc_violations`), filtr `matches!(classify(m), Kind::Core(ref n) if n=="bus")`; `main()` woła fn; istniejące testy rule-C (które testują serde_json, nie regułę) zastąpić testami fixture wołającymi PRAWDZIWĄ fn: `bus`+dep sqlx ⇒ 1 violation; bez sqlx ⇒ 0. Sweep siblingów zrobiony (research #1): rule 9 to jedyny martwy filtr.
- Test failing-branch: fixture z `core/bus/Cargo.toml` + dep sqlx MUSI dać violation (na starym kodzie test czerwony).

**Step 2 — topiccheck: klucz (topic, version)** `[sonnet]`
- (a) `tools/topiccheck/src/tests.rs:226-285`.
- (b) Gate wprost zakazuje nakazanej przez CLAUDE.md additive evolution; musi być naprawiony zanim ktokolwiek doda v2.
- (c) Jak: wyekstrahować skan do helpera `parse_define_sites(text) -> BTreeSet<(String,u32)>` (parsowanie wersji: literal po przecinku za stringiem topicu — potwierdzone we wszystkich 7 call sites, same-line); klucz `(topic,version)`; panic tylko na duplikat pary; `from_defined` mapowane na pary. **Token wersji, który NIE jest integer-literalem (const, złamanie linii przez rustfmt) ⇒ głośny panic z file:line — nigdy cichy skip** (inaczej odtwarzamy klasę martwego gate'a z kroku 1). Testy: fixture 2-linijkowy `define("x",1,…)` + `define("x",2,…)` ⇒ 2 wpisy bez paniki; duplikat pary ⇒ panic; `define("x", VERSION, …)` ⇒ panic z komunikatem.
- Known-gap (zapisać w planie/commit): `ALLOW_UNSUBSCRIBED`/`ALLOW_INPROCESS_DEFINED` kluczują po samym topicu — dziś puste listy, więc latentne; NIE naprawiamy (minimal closure), odnotowujemy w komentarzu przy listach.

**Step 3 — WorkspaceLayout: jedno źródło prawdy o target-dir** `[opus + think hard]`
- (a) Nowy typ `processctl::WorkspaceLayout { root, target_dir }` w `tools/processctl/src/fleet.rs` (lub nowy moduł `layout.rs`); przepiąć: `tools/devctl/src/supervisor.rs:721` (`binary()`), `tools/splitproof/src/main.rs:345` (`workspace_dirs()`) i `:70-72` (`Ctx::spawn`), `tools/verifyctl/src/runner.rs:349-362` (`splitproof_executable` — skasować duplikat).
- (b) Wszystkie 3 toole już zależą od processctl; verifyctl ma poprawną logikę do uogólnienia; build już dziedziczy `CARGO_TARGET_DIR` (BUILD_ENV_ALLOWLIST), tylko lookup binarki się rozjeżdża.
- (c) Rozstrzyganie: (1) `CARGO_TARGET_DIR` z `EnvironmentSnapshot::build_environment()` (relative→resolve względem roota, absolute→verbatim; logika = dzisiejszy verifyctl), (2) fallback `root/target`; `current_exe`-two-levels-up TYLKO jako last-resort z głośnym warn. (Celowo BEZ `cargo metadata` w hot-path — devctl/splitproof mają root z lease/parametrów; `.cargo/config.toml build.target-dir` odnotować jako known-gap w doc-komentarzu typu.) `binary(profile, package)` na typie. **Doc-komentarz typu MUSI nazwać sprzężenie (review):** relative `CARGO_TARGET_DIR` rozwiązywany względem roota jest poprawny tylko dlatego, że te toole spawnują cargo z cwd=root (cargo rozwiązuje względem cwd inwokacji) — kto zmieni cwd, rozjedzie build i lookup.
- Testy: przenieść/zaadaptować `frozen_snapshot_ignores_poison_ambient_and_resolves_relative_target` do processctl; przypadki: unset / relative / absolute.

**Step 4 — splitproof: liveness childów** `[sonnet]`
- (a) `tools/splitproof/src/main.rs:89-103` (`wait_healthy`), boot-loop `:495-497`, pre-teardown `:513`.
- (b) Proof może dziś przejść na starym procesie — unieważnia całą fazę split-proof.
- (c) Jak: sygnatura `wait_healthy(&self, svc, child: &mut OwnedChild)` — na każdej iteracji `child.try_wait()?` ⇒ `Some(status)` = natychmiastowy bail z statusem (wzorzec 1:1 z devctl `supervisor.rs:539-562`); nowe nazwane asercje `[LV1]` (per-spawn, po health-gate) i `[LV2]` (fleet-wide sweep przed `drop(fleet)`). **Uwaga do dispatch-promptu (review): anchory liniowe splitproof są sprzed kroku 3 — lokalizować po symbolach (`wait_healthy`, boot-loop, pre-teardown), nie po numerach linii.**
- Test: jednostkowy na helperze + [LV1]/[LV2] w samym proofie (żywa asercja).

**Step 5 — contract-golden: serde fingerprint payloadów** `[opus + think hard]`
- (a) `tools/topiccheck/src/golden.rs` (+`golden_tests.rs`), 6× `api/*/events/src/lib.rs`, `tools/rpc-macro/src/lib.rs:438-463` i moduł-emisja `:213-262`, `tools/topiccheck/Cargo.toml` (+serde_json workspace dep), `docs/reference/contract-golden/contracts.txt` (bless).
- (b) Ostatni ślepy gate; musi działać zanim ktokolwiek ruszy shape'y w krokach późniejszych.
- (c) Jak: helper `flatten_shape(prefix,&Value,&mut BTreeSet<String>)` (rekurencyjnie `path:jsontype`); eventy — `golden_sample()` per events-crate: ręczne, **ZALUDNIONE** literały (każdy `Option` = `Some(...)`, każda kolekcja ≥1 element — np. `configevents::Changed.value = Some(...)`), wzór z produkcyjnych emitów; linie `payload topic=<t> {path}:{type}`; RPC — makro emituje `body_shapes() -> Vec<(&str, Value)>` **TYLKO dla metod `#[http]`** (fakt z kodu: `gen_request_struct` derive'uje `Default` wyłącznie gdy `m.http.is_some()` — rpc-macro lib.rs:436-443; metody wire-only zostają POZA fingerprintem świadomie: internal edge jest współ-deployowany z jednego commita, brak retained JSON ⇒ Known-gap 7), linie `rpc-body module=<m> method=<x> {path}:{type}` — pinuje `ReportId/Winner/Loser` po raz pierwszy.
- **Ograniczenie Default (review):** `Default` na `<Method>Request` daje `None`/puste kolekcje ⇒ rename na polu `Option`/`Vec` w body HTTP może być niewidoczny — zapisać wprost w `GOLDEN_HEADER` i w Known-gap 8; eventowa strona (retained JSON — realne ryzyko) jest w pełni pokryta zaludnionymi samplami.
- **Didn't-forget check (review):** `live_lines()` FAILUJE gdy jakikolwiek wpis `defined_topics()` nie ma linii payloadowych (nowy topic bez `golden_sample()` = głośny błąd z per-entry komunikatem, wzorzec `self_check_rpc_list`).
- Rozszerzyć `GOLDEN_HEADER` + test „covers all kinds"; jednorazowy `--bless-contract-golden` (czysto ADDITIVE).
- Test failing-branch: unit na `flatten_shape` z `#[serde(rename)]` na strukturze testowej ⇒ inna linia; test didn't-forget (fałszywy Contract bez sampla ⇒ błąd).

*Po fazie A: `verifyctl --all --strict` (pełny), commit-po-kroku już za nami.*

---

### FAZA B — poison-klasa durable plane

**Step 6 — inventory: jedna polityka Quantity** `[opus + think hard]`
- (a) `modules/inventory/src/lib.rs`: DDL `:62` — `quantity int` → `bigint` **z `CHECK (quantity >= 0 AND quantity <= 2000000)`** (2× cap aplikacyjny: CHECK pilnuje magnitudy także dla raw psql — SPÓJNIE z doktryną kroku 7, gdzie „legalny writer to psql ⇒ authority = DDL"; luz 2× bo CHECK ogranicza STAN po sumowaniu, a polityka pojedynczy grant); nowa `fn validate_quantity(i64) -> Result<i64, …>` (`0 < qty <= MAX_HOLDING_QTY = 1_000_000`), call sites: `starter_spec`/`grant_starter` degrade-branch (`:367`), `Holdings::grant` (`:474` ⇒ `Error::invalid`), oraz belt WEWNĄTRZ `grant_exec` (`:169`) — jedyny writer Rustowy.
- (b) Najgroźniejszy poison (jedna wartość config blokuje starter-granty wszystkim); sweep siblingów potwierdził: jedyna kolumna z pełnym wzorcem.
- **Dwie postury błędu tej samej polityki — celowe (review):** ścieżka config (durable handler) DEGRADUJE do defaultu z warn, bo błędna wartość admina nigdy nie może zatruć subskrypcji `inventory.character-created.v1`; ścieżka HTTP ODRZUCA 400, bo klient ma feedback i żaden checkpoint nie wisi na wyniku.
- (c) Rollout DDL: wipe (`DROP SCHEMA inventory CASCADE`) — zgodnie z filozofią repo; zaznaczyć w commit message. **Commit message nazywa też rationale `MAX_HOLDING_QTY=1_000_000`** (gameplay-facing zmiana semantyki: wielki grant, który dawniej przechodził, teraz = 400 — Fix-the-Authority pkt 4).
- Testy failing-branch: `starter_qty=2147483648` w config ⇒ grant degraduje do defaultu, delivery tx przechodzi (na starym kodzie: 22003 i czerwony test — wzorzec `grant_on_created_via_on_tx`, tests.rs:392); HTTP grant `i64::MAX` ⇒ 400 nie 500; raw-SQL INSERT ponad CHECK ⇒ 23514; boundary unit-testy polityki.

**Step 7 — scheduler: sufit interval_seconds** `[sonnet]`
- (a) `modules/scheduler/src/lib.rs:136` (CHECK → `interval_seconds > 0 AND interval_seconds <= 9223372036854`), `:151` DUE_SQL i FIRE_RECHECK_SQL (filtr → `BETWEEN 1 AND 9223372036854` — pas dla nie-wipe'owanej tabeli, ten sam wzorzec co dzisiejszy `> 0`), `modules/scheduler/src/tests.rs:255-266` (anti-drift substring update), `:293-311` (szablon dla nowego testu).
- (b) Authority = DDL CHECK, bo legalny writer to psql (potwierdzone: admin surface read-only, brak innych writerów). **Sufit 9223372036854 dostaje komentarz z proweniencją przy SAMYM CHECK-u (review):** „zweryfikowane empirycznie na PG18 — wewnętrzna reprezentacja interval to mikrosekundy w int64; re-weryfikuj przy zmianie major wersji PG".
- **Rollout DDL (review-BLOCKER): `CREATE TABLE IF NOT EXISTS` NIE zmieni CHECK-a na istniejącej tabeli — wymagany `DROP SCHEMA scheduler CASCADE` + świeży boot, tak samo jak inventory w kroku 6.** Bez wipe test 23514 będzie czerwony na każdej maszynie, która kiedykolwiek bootowała scheduler.
- Testy: `huge_interval_insert_violates_check` (23514, po wipe); `due_scan_survives_legacy_huge_interval_row` (INSERT przez raw_sql omijający CHECK na starej tabeli ⇒ skan `Ok`, zdrowe schedules due — na starym kodzie czerwony).

---

### FAZA C — timeouty / ownership

**Step 8 — HTTP: prawdziwy ownership connection-tasków** `[opus + think hard]`
- (a) `core/app/src/lib.rs:928-978`: plain-HTTP branch z `axum::serve` → `axum_server::from_tcp(listener.into_std()?)` + `Handle`; na sygnał `handle.graceful_shutdown(Some(cfg.http_drain_grace))`; SKASOWAĆ `select!`+sleep (`:968-978`) — timing przejmuje Handle; `into_make_service_with_connect_info` bez zmian (identyczny MakeService jak w `serve_https:1090`).
- (b) Jedyny fix w planie zmieniający zachowanie shutdownu każdego procesu; przed krokami 9-11, bo ich testy polegają na czystym teardownie.
- (c) Wzorzec = ścieżka TLS w tym samym pliku (axum-server per-connection task ownuje future i dropuje ją in-place po grace — server.rs:326-333 w axum-server 0.8). Zero nowych deps. **Do zweryfikowania w implementacji (review):** (i) `from_tcp` na tokio→std listenerze (nonblocking już ustawiony) działa poprawnie; (ii) `Handle::graceful_shutdown(Some(grace))` faktycznie ubija connection-taski, nie tylko wraca (HungModule test to dowodzi); (iii) zachować parytet operatorski: log `warn!("http drain grace expired…")` i kontekst błędu `bind http {bind}`.
- Test failing-branch (wzorzec `SlowRoutes`+`StopRec`, core/app/src/tests.rs:549-824): `HungModule` z handlerem śpiącym > grace, zapisującym `stopped.load()` do flagi; assert po `run()`: `touched_after_stop == false` — na starym kodzie czerwony.
- Known-gap (odnotować): edge'owy `RunningServer::shutdown` nie anuluje handlera wiszącego na nie-transportowym awaicie (close-and-hope) — poza zakresem tego planu, zapisać w sekcji Known gaps.

**Step 9 — asyncmig: lock_timeout w DDL** `[sonnet]`
- (a) `core/asyncevents/src/store.rs:50-52`: po `BEGIN;` dodać `SET LOCAL lock_timeout = '60s';` (transakcyjny — self-reset na COMMIT/ROLLBACK, bez dedykowanej konekcji); w `ensure_schema` (`:148-166`) detekcja SQLSTATE 55P03 + komunikat lustrzany do lifecycle („asyncevents: V2 plane advisory lock not acquired within 60s — another process is stuck mid plane-DDL; see pg_stat_activity").
- (b) Sibling pominięty przy fixie modułów (60s); wartość 60s = ta sama konwencja.
- Test failing-branch: trzymaj `pg_advisory_xact_lock(MIGRATE_LOCK_KEY)` w otwartej tx, wywołaj `ensure_schema` z krótkim testowym timeoutem (parametryzacja jak `migrate_with_lock_timeout`) ⇒ szybki błąd 55P03, nie wieszanie. Wziąć `WRITER_LOCK_CHOREOGRAPHY` (store_tests.rs:9-15) — tx trzymana przez await. *(Kroki 9-11: testy pisać/uruchamiać na kodzie PO kroku 8 — nowa semantyka drain jest założeniem ich negative-pathów.)*

**Step 10 — gateway: jeden budżet credential admission** `[opus + think hard]`
- (a) `modules/gateway/src/lib.rs`: nowa wspólna `async fn admit(&FrontDoor, api_key, bearer, auth_req, method) -> Result<Option<Identity>, AdmissionDenial>` wołana z `dispatch_matched_op` (`:805-819`, przed decode `:821`) i `handle_player` (`:396-421`, przed dispatch `:424`); całość w JEDNYM `tokio::time::timeout`; elapse ⇒ istniejące klasy `Unavailable` (503 HTTP / envelope `Unavailable` player) — zero nowych mapowań. Budżet: `Gateway::with_admission_budget(Duration)` (wzorzec `with_passthrough`), env `CREDENTIAL_ADMISSION_TIMEOUT_MS` (default 5000) czytany w `cmd/gateway-svc/src/main.rs` i `cmd/server` — nigdy w module.
- (b) Bezpieczeństwo timeoutu potwierdzone researchem: flight lock = `Arc<Mutex>` przez `lock_owned` — drop future zwalnia lock, `Weak` w tabeli nie upgrade'uje się, świeży Mutex dla następnych; cache nigdy nie zapisuje na Err/cancel. Admin fan-out (`admin.adminData`) = mTLS internal edge bez credential-verify — poza budżetem.
- Testy failing-branch: hung `FakeKeyVerifier`/`UnavailableVerifier`-style fake (never-resolving future) ⇒ oba fronty 503/`Unavailable` w budżecie; test lock-holdera: pierwszy lookup wisi > budżet, drugi o TEN SAM klucz nie deadlockuje (timeout obejmuje też czekanie na flight lock); **test recovery (review): po timeout'owanym admission NASTĘPNY request o ten sam klucz weryfikuje się poprawnie** (Weak flight-entry nie upgrade'uje się, świeży Mutex — przejściowy hang nie staje się permanentnym 503).

**Step 11 — remote: bound na boot hooki** `[sonnet]`
- (a) `core/remote/src/lib.rs:486-495`: `tokio::time::timeout(BOOT_TIMEOUT, (b.boot)())` w pętli; `const BOOT_TIMEOUT: Duration = 10s` (core-leaf, bez env; opcjonalne przełożenie przez konstruktor Stub JEŚLI kiedyś potrzebne — nie teraz).
- (b) Jedyny registrant dziś: configrpc boot-fill; hung config-svc wiesza dziś każdy start splitowego procesu. **Commit message nazywa zmianę zachowania (review):** boot storm z żywym-ale-wolnym config-svc teraz FAILUJE startup po 10s tam, gdzie dawniej (patologicznie) czekał — to decyzja, nie przypadek.
- Test: fake `RemoteBoot` never-resolving ⇒ `Stub::start` = `Err` w boundzie (istniejący seam fake dial/conn w testach remote).

*Po fazie C: `verifyctl --all --strict`.*

---

### FAZA D — budżet Postgresa

**Step 12 — DatabasePoolConfig + invariant floty** `[opus + think hard]`
- (a) `core/app/src/lib.rs`: pola `db_pool_max` (env `DATABASE_POOL_MAX_CONNECTIONS`, default 10 — bez zmiany zachowania), fail startup gdy `< 2` (floor z komentarza `core/lifecycle/src/app.rs:130-133` — dziś nieegzekwowany, self-deadlock migrate); `run()` `:591` → `PgPoolOptions::new().max_connections(...)`. `tools/processctl/src/fleet.rs`: `ServiceSpec.pool_budget { pool_max, dedicated }`, invariant w `FleetSpec::new`: `sum(pool_max + dedicated) <= 97` ⇒ nowy `FleetError::PoolBudgetExceeded` (wzorzec `DuplicateService`).
- **(review-BLOCKER) `pool_budget.pool_max` MUSI zasilać env spawnowanych procesów:** `base()` we fleet.rs wstrzykuje `DATABASE_POOL_MAX_CONNECTIONS={pool_max}` do env każdego DB-backed svc — jedno pole karmi ZARÓWNO runtime, jak i invariant; inaczej invariant waliduje fikcyjną tabelę, a procesy czytają default.
- **(review) Anti-drift dla `dedicated`:** liczby dedykowanych sesji NIE są wolnymi literalami — eksport stałych z realnych źródeł (`asyncevents::WORKERS` pub, analogicznie invalidation/scheduler ma 1 dedykowaną z natury implementacji) i test `fleet_pool_budget_matches_plane_constants` linkujący tabelę do stałych (wzorzec `seeded_schedule_names_are_contract`).
- (b) FleetSpec::new = miejsce istniejących invariantów strukturalnych; fail w konstrukcji łapie dryf w momencie dodania svc.
- (c) Wartości per-svc: tabela w fleet.rs (gateway-svc bez DB ⇒ 0); monolith osobno (`game_backend_monolith`). **Doc-komentarz na stałej budżetu 97 (review):** założone defaulty serwera (`max_connections=100`, `superuser_reserved_connections=3` — konfigurowalne po stronie PG) oraz JAWNE wykluczenie narzutu harnessów (pool sqlx splitproofa, psql devctl) — zapisana decyzja, nie magia.
- Testy: unit na parser env (0/1 ⇒ fail startup, unset ⇒ 10); unit na invariant (flota przekraczająca budżet ⇒ `PoolBudgetExceeded`); test linkujący dedicated↔stałe planów; istniejący fleet-drift bez zmian.

---

### FAZA E — drobnica (authority-fixy)

**Step 13 — parser pary (rps, burst) + env-siblingi invalidation** `[sonnet]`
- (a) `core/app/src/lib.rs`: `fn env_rate_pair(rps_var, burst_var, defaults, RateZeroPolicy) -> anyhow::Result<(f64,u32)>`; polityka pary: `Reject` + efektywne rps>0 + burst==0 ⇒ **fail startup** (komunikat z nazwami OBU varów, bez wartości operatora); `Allow` ⇒ burst==0 legalnie wyłącza warstwę (zachowanie player bez zmian). Zastąpić 3 pary call-sites (`:856-857`, `:222-228`, `:229-236`). Plus siblingi z sweepa: `core/invalidation/src/lib.rs:429-449` — jawne `0`/invalid w `INVALIDATION_POLL_INTERVAL_MS`/`INVALIDATION_CALLBACK_TIMEOUT_MS` ⇒ fail startup (wzorzec `EVENTS_HOUSEKEEP_INTERVAL`), absent ⇒ default.
- **(review) Dwie precyzje zakresu:** (i) samotne `RATE_LIMIT_RPS=0` przy Reject zachowuje DZISIEJSZĄ semantykę (warn + default, lib.rs:398-424) — hard-fail dotyczy WYŁĄCZNIE pary `rps>0 && burst==0`; nie rozszerzać. (ii) Fallibility: para gatewayowa jest parsowana w `run()` (zwraca `Result` — `?` działa); pary playerowe w `Config::from_env` mają politykę `Allow`, która NIGDY nie błądzi ⇒ `from_env` zostaje niefallible (expect z komentarzem „Allow-policy is infallible by construction"), zero ripple na sygnatury cmd/*.
- (b) Jedna semantyka zera decydowana w JEDNYM parserze; posture gateway-always-on-Reject przeżywa.
- Testy: 7 przypadków z researchu #8 (w tym: burst=0 na Reject ⇒ fail boot; player burst=0 ⇒ unlimited — regression pin; limiter `IpLimiter(rate,0)` deny-all pozostaje poprawny na poziomie mechanizmu; lone RPS=0 ⇒ warn+default bez zmian).

**Step 14 — gateway: detekcja overlapu tras** `[sonnet]`
- (a) `modules/gateway/src/lib.rs:548-585`: `fn pattern_overlaps(a,b) -> bool` (ta sama długość ∧ każda pozycja: Lit==Lit ∨ ≥1 Wild); w pętli kolizji `bail!` („may overlap", verb+obie ścieżki+obie metody — wzór z `:566-573`).
- (b) Audyt tras potwierdził: ŻADNA dzisiejsza para się nie nakłada ⇒ twardy reject nic nie łamie; konwencja loud-boot-failure (registry/edge panic na duplikat) — tu `bail!` z `RouteTable::build` propagowany przez `Gateway::start`. **(review)** Overlap ściśle nadzbioruje shape-equality (Wild/Wild spełnia „≥1 Wild") — stary check składa się w nowy, ale komunikat błędu MUSI nadal nazywać obie metody i obie ścieżki; potwierdzić przy implementacji, że `parse_pattern` nie ma form zmiennej długości (trailing `...` jest strip'owany — sprawdzić, czy nie oznacza multi-segmentu).
- Testy: unit na `pattern_overlaps` (para {id}/me ⇒ overlap; różne długości ⇒ nie; Lit≠Lit ⇒ nie; Wild/Wild ⇒ overlap jak dziś); test build ⇒ bail na nakładającej parze. routecheck łapie zestawy automatycznie (buduje realne module-sety) — bez zmian tam.

**Step 15a — Epic: Err ≠ brak sesji** `[opus + think hard]` *(review: granica bezpieczeństwa — auth-binding flow, nie mechanika; osobny commit)*
- (a) `modules/accounts/src/epic_oauth.rs:262-267`: match z trzema ramionami — `Err` ⇒ `tracing::error!` + `503 SERVICE_UNAVAILABLE` plain-text (webui obsługuje `!r.ok` generycznie — potwierdzone index.html:125-129), early-return PRZED `new_state`; sibling `:326` (callback LINK): dodać rozróżnienie `Err` (tracing::error + redirect error) od `Ok(None)` — bez zmiany zachowania widocznego, z logiem.
- (b) Konwencja 503-nie-401 (verifier.rs) — epic_oauth:264 to jedyne naruszenie w sweepie; Err w handle_start dziś po cichu przekierowuje LINK we flow LOGIN z realnym skutkiem ubocznym (duplikat konta).
- Testy: epic start z bearer + unreachable DSN ⇒ 503 i ZERO zmintowanego state; happy-path 200 z niepustym session_token w state.

**Step 15b — adminctl: wspólna normalizacja username** `[sonnet]` *(osobny commit)*
- (a) `modules/admin/src/lib.rs`: nowa `pub fn normalize_username(&str) -> Result<String, &'static str>` (trim ⇒ empty check ⇒ cap 128B); `login_submit` (`:813,831`) i `tools/adminctl/src/lib.rs:48-70` wołają JĄ (adminctl binduje wartość znormalizowaną).
- (b) Jedna funkcja zamiast dwóch przypadkowo zgodnych reguł; adminctl już importuje admin (USERS_DDL, hash_password) — sanctioned surface, brak problemu fortress (tool, nie moduł).
- Testy: adminctl `"  bob  "` ⇒ przechowane `bob`; 200B ⇒ Err; tabela zgodności normalize↔handler (128/129B, whitespace-only).

**Step 16 — edge: typed `code` w envelope** `[opus + think hard]`
- (a) `core/edge/src/wire.rs:29-36` (`Response.code: Option<&'static str>`-odpowiednik, `#[serde(default, skip_serializing_if)]`), `core/edge/src/server.rs:365-393` (unknown-method ⇒ `code: Some("unknown_method")`; zwykłe błędy ⇒ `None`), `core/edge/src/client.rs:124-137` (detekcja po `code`, prefix zostaje tylko human-readable), `From<edge::Error> for opsapi::Error` bez zmian.
- (b) **Odwrócenie udokumentowanej decyzji** — nazwać w commit message + errata-note w KONKRETNYM miejscu: `docs/plans/2026-07-11-1602-all-findings-remediation-plan.md`, sekcja Step 7 (tam padło odrzucenie Option C) — w TYM samym rollout'cie, jako dopisek datowany (archiwa się nie przepisuje — errata to dopisek, nie edycja treści). public-api baseline NIEDOTKNIĘTE (skanuje tylko api/*), contract-golden niedotknięte (envelope poza `rpc_modules()`); `Response` bez `deny_unknown_fields` ⇒ pole additive bezpieczne (potwierdzone przez review).
- Testy: e2e false-positive (handler propagujący inner-unknown-method przez `?` ⇒ OUTER błąd bez `code`, klient NIE klasyfikuje jako UnknownMethod — na starym kodzie czerwony); genuine unknown-method ⇒ `code=="unknown_method"` ⇒ `Error::UnknownMethod` ⇒ NotFound (istniejący łańcuch mapowania).

---

### FAZA F — docs + retention

**Step 17 — docs** `[sonnet]`
- (a) `docs/README.md:14-15`: usunąć gateway/edge-gateway-quic z „Current reference"; `docs/reference/gateway.md` i `edge-gateway-quic.md`: blockquote `> **ARCHIVED (JVM/Quarkus-era)** …` (wzorzec baas-feature-gap-matrix.md; treści NIE przepisywać — konwencja archiwów); `docs/reference/hetzner-deploy-checklist.md:61`: `ORDER BY created_at DESC`.
- (b) docs-current (linter mechaniczny) tego nie łapał i nie będzie — heurystyka treści = gold-plating (świadoma decyzja, odnotowana).

**Step 18 — retention: fresh floor w DELETE** `[opus + think hard]`
- (a) `core/asyncevents/src/retention.rs:255-318`: zastąpić pre-fetch floora + dwa ramiona jednym DELETE, którego korelowany podzapyt MUSI nieść PEŁNY predykat dzisiejszego floora (review): `NOT EXISTS (SELECT 1 FROM asyncevents.subscriptions s WHERE s.topic = $1 AND s.contract_version = $2 AND s.state IN ('active','paused') AND (s.cursor_generation, s.cursor_xid, s.cursor_tie) <= (e.generation, e.producer_xid, e.tie_breaker))` — **bez filtra `state` retired/completed subskrypcja pinowałaby GC na zawsze (defekt odwrotny)**; floor zawsze świeży, ramię None (brak aktywnych subów ⇒ vacuous true) scala się naturalnie; porównanie kompozytowe na typowanych wartościach (nie text-alias — regresja `floor_uses_numeric_xid_order_not_text` pozostaje wzorem); kursory NOT NULL (potwierdzone) — brak seamu NULL-comparison.
- (b) Tani, całkowity fix klasy zamiast zawężenia okna; odrzucony lock audytu odnotowany w Context.
- Testy: trigger-injection (BEFORE DELETE, raz, wzorzec z retention_tests.rs:387-426): nowa niższa subskrypcja w trakcie 1. batcha wielobatchowego GC ⇒ eventy ≥ jej kursora przeżywają (na starym kodzie czerwony); **retired subskrypcja z niskim kursorem NIE blokuje delete (review — pin filtra state)**; sibling regresji numeric-order dla composite-tuple.

---

## Weryfikacja (mapa fix→hook)

- Kroki 1-5: `cargo test -p archcheck -p topiccheck -p processctl -p splitproof -p devctl -p verifyctl` + stage'y fortress/contract-golden (krok 5 = jednorazowy bless, czysto ADDITIVE — diff obejrzany przy review).
- Kroki 6-7: testy modułowe na żywym PG (wzorce `grant_on_created_via_on_tx`, `zero_interval_insert_violates_check`); **wipe OBU schematów przy rollout** (`DROP SCHEMA inventory CASCADE; DROP SCHEMA scheduler CASCADE;` + świeży boot — CHECK-i nie wchodzą przez `CREATE TABLE IF NOT EXISTS`).
- Krok 8: `core/app/src/tests.rs` (HungModule) + split-proof W2/M-serie.
- Krok 9: store_tests z `WRITER_LOCK_CHOREOGRAPHY`; plane-testy `--test-threads=1` (memory: asyncevents self-deadlock).
- Krok 10: gateway tests (fakes) + conformance `InfraOutage503` probes gatewaya bez zmian stance'ów; split-proof K-serie.
- Krok 12: fleet_tests processctl; splitproof fleet-preflight bez zmian.
- Kroki 13-16: testy jednostkowe wg kroków; krok 16 NIE wymaga bless (public-api skanuje tylko api/*).
- Finał: `cargo run -p verifyctl -- --all --strict` + `--slow` na życzenie. Jeden rollout naraz (rollout.lock; sprawdzać cargo/rustc przed startem — subagenty implementacyjne dostają ten wymóg w prompcie).

## Known gaps (jawne, świadome — nie ciche)

1. Domain-op unbounded awaity: `inventory Holdings::list_character → owner_of` (lib.rs:444), `match report → mmr ×2` (lib.rs:193-194), `admin resolve_items → admin_data` (adminrpc lib.rs:81) — wszystkie na ścieżkach HTTP objętych `HTTP_REQUEST_TIMEOUT_MS` (408 whole-request); spójne z celową decyzją o niebounded player-dispatch. Zapisane, nie fixowane.
2. `ALLOW_UNSUBSCRIBED`/`ALLOW_INPROCESS_DEFINED` topic-only key — puste listy dziś (komentarz przy listach w kroku 2).
3. edge `RunningServer::shutdown` nie anuluje handlera na nie-transportowym awaicie (close-and-hope) — odnotowane przy kroku 8.
4. `policy_columns` `i32::try_from(days).unwrap_or(i32::MAX)` clamp — osiągalne tylko z hardcoded literals (days:7/30).
5. splitproof `status_or_err`/`player_burst` text-sniff — oracle testowy toola. Mocniejsze uzasadnienie (review): to ta sama anty-klasa co D5, ale sniffowany tekst pochodzi z NASZEGO edge::Error Display (kontrolowany producent, nie obcy peer), a błędna klasyfikacja = fałszywy wynik JEDNEJ asercji proof-u, widoczny w tabeli — nie cichy gate. Po kroku 16 (typed `code`) można tanio przepiąć oracle na typed status — zapisane jako follow-up, nie w tym rollout'cie.
6. `.cargo/config.toml build.target-dir` nieparsowany przez WorkspaceLayout (doc-komentarz na typie).
7. Wire-only `<Method>Request` shapes poza golden fingerprint (krok 5) — internal edge współ-deployowany z jednego commita, brak retained JSON; ryzyko dryfu cross-version nie istnieje w tym modelu deploymentu.
8. Default-blindness fingerprintu RPC-body na polach `Option`/`Vec` (krok 5) — zapisane w GOLDEN_HEADER; strona eventowa (jedyna z retained JSON) pokryta zaludnionymi samplami.

## Review i implementacja — protokół

0. **Status recenzji planu:** grumpy-senior NA TIERZE SESJI (fable, think hard) — 16 findings, wszystkie zaadresowane powyżej (znaczniki „(review)"); pomocnicza recenzja sonnet — 11 findings, zaadresowane; recenzja Codex planu — job w tle NIE zwrócił wyników (uznany za martwy, decyzja usera: nie czekamy). Codex pozostaje w pętli na poziomie COMMITÓW (pkt 3).
0a. **Budżet modeli (decyzja usera):** żadnych subagentów na fable. Tagi kroków nazywają model wprost: `[opus + think hard]` = praca złożona/correctness-critical (dawne subagent-complex), `[sonnet]` = praca mechaniczna (dawne subagent-mechanical). Adversarial review każdego commitu = **opus** (z uzasadnieniem w description) + Codex.
2. Implementacja: kolejność kroków jak wyżej; tag przy kroku = model subagenta wykonawczego (`[opus + think hard]` albo `[sonnet]`); nav-guidance i zakaz równoległych testów wklejane do promptów.
3. Po KAŻDYM commicie: adversarial review — subagent na **opus** (atak na nowe seamy fixu, weryfikacja failing-branch testu na kodzie, nie na podstawie podsumowania wykonawcy) + CODEX (drugi, niezależny przegląd; jeśli Codex znów nie odpowie, odnotować i kontynuować z samym opusem) — punch list wraca do wykonawcy, nie jest cicho absorbowany.
4. Memory-sync po każdej zmianie memory; push tylko na życzenie.
