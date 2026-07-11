# Plan naprawczy — ustalenia z zewnętrznej recenzji (2026-07-11)

> **Repo-copy obowiązek:** krok 0 kopiuje ten plan do
> `docs/plans/2026-07-11-<HHMM>-review-remediation-plan.md` (plan-mode pozwala pisać
> tylko tutaj; repo jest źródłem prawdy — CLAUDE.md „Plans & Status Docs").

## Context

Zewnętrzna recenzja (Codex) zgłosiła 7 ustaleń + drift dokumentacyjny. Wszystkie
zweryfikowałem w kodzie (celowane odczyty + 6 subagentów researchowych). Zakres
zatwierdzony przez użytkownika: **#1–#5 + #7 + pełny sweep docs** (#6 — per-service
certy — zostaje na checklistcie Hetzner, poza zakresem).

Problemy, które plan zamyka:

1. **Podwójny `match.report` w splicie** — `core/remote` (Reconnecting::call,
   `core/remote/src/lib.rs:187-198`) retry'uje po KAŻDYM błędzie; jeśli backend
   zacommitował a odpowiedź zginęła, powstaje drugi mecz i drugi `match.finished`
   (rating/leaderboard naliczą 2×). Łamie parity monolith↔split.
2. **Formularze admin (apikeys + config) częściowo commitują** — `apply_edit` to
   sekwencja autocommitów z first-error-wins; walidacja pola B nie cofa zapisu pola A,
   a `admin.action` nie jest emitowane przy Err mimo zmienionego stanu.
3. **Invalidation bez deadline'ów** — `Registration::run` czeka na callback bez
   timeoutu (blokuje kolejne NOTIFY, poll fallback i startup), `stop()` czeka na taski
   bez ograniczenia — jedyny plane bez `*_GRACE_MS`.
4. **Epic OAuth LINK: `Taken` = fałszywy sukces** — `epic_oauth.rs:231-238` redirect
   `/?epic=linked` bez sprawdzenia, czy tożsamość należy do bieżącego gracza.
5. **Gateway `/readyz` 200 przy martwej flocie** — DB-less proces bez żadnego checku;
   stuby lazy-dial, nic nie kontrybuuje `ReadyCheck`.
6. (#7 recenzji) **Player-QUIC :9100 bez limitu połączeń** — quinn przyjmuje
   nieograniczenie, task per połączenie; limity są tylko per-connection (streams/bufor).
7. **Drift dokumentacyjny** — 26 STALE hitów w 12 plikach (README, CLAUDE.md,
   AGENTS.md — najbardziej rozjechany, doc-comments, 2× Cargo.toml) + brak bannera na
   Go-erowym `docs/reference/baas-feature-gap-matrix.md`.

**Why not extend / depend on X (Research-before-planning):** żaden krok nie dodaje
modułu ani nowej capability. Wszystko to poprawki wewnątrz istniejących seamów:
dedup `report_id` w module match (wzorzec `inventory.wiped_characters` ON CONFLICT DO
NOTHING), transakcyjność przez `_tx`-warianty istniejących store'ów, timeout wg wzorca
`readyz_response`, ownership-check przez ISTNIEJĄCE `player_by_identity`, readiness
przez ISTNIEJĄCY `httpmw::READINESS_SLOT` kontrybuowany z `remote::Stub` (infra
composition-root, nie moduł domenowy — precedens: `PEER_SLOT` w tym samym `init`),
limity połączeń w `core/edge` z knobami wg wzorca `RATE_LIMIT_*` w `core/app`.

**Decyzje projektowe (rozstrzygnięte, nie „TBD"):**
- `report_id` jest **wymagane** w body (`ReportId` — spójne z Go-parity caps
  `Winner`/`Loser`); duplikat → 202 + brak emisji (nie error); puste →
  `Error::invalid` (jedyny istniejący konstruktor — `core/opsapi/src/lib.rs:188`).
  Zmiana kontraktu ⇒ re-bless `matchapi` public-api + **pełny sweep callerów** (patrz
  Step 5 — obejmuje też klienta C#: gbclient [C5]/[C6] w verify, regenerację
  `clients/csharp` pod blokujący stage `codegen-fresh` i goldeny
  `tools/csharp-client-gen`).
- Kolizja Epic (Taken przez INNEGO gracza) → redirect `/?epic=error` (webui zna tylko
  `linked`/`error`; nowa wartość byłaby cicho połknięta — bez zmian w webui). Taken
  przez TEGO SAMEGO gracza → `/?epic=linked` (idempotentny re-link).
- Po utransakcyjnieniu apply_edit **emit admin.action przy Err pozostaje wyłączony
  i staje się PRAWDZIWY** (Err ⇒ nic się nie zmieniło) — moduł admin NIE wymaga zmian.
- Invalidation: knob `INVALIDATION_CALLBACK_TIMEOUT_MS` (default 10_000) czytany
  w `core/invalidation` (wzorzec `poll_from_env`, lib.rs:374-382); timeout liczy się
  jako zwykły failure (istniejący `inc_failure` + tracing). `stop()` self-bounded:
  join z `tokio::time::timeout(5s)` + `abort()` po przekroczeniu (stała
  `DEFAULT_STOP_GRACE_MS: u64 = 5000`, jak inne plane'y). Startup first-refresh też
  pod timeoutem — timeout = głośny fail startu (spójne z fail-loud).
- Readiness per-stub: `Stub::init` kontrybuuje `ReadyCheck("stub:<provider>")` =
  parse addr + `shared_dev_ca` + `timeout(1s, edge::Client::dial)` + close. Działa we
  WSZYSTKICH procesach ze stubami (nie tylko gateway) — stub to twarda zależność sync,
  więc „peer nieosiągalny ⇒ not ready" jest semantycznie poprawne.
- Player-QUIC: `PLAYER_MAX_CONNS` (default 1024) + `PLAYER_MAX_CONNS_PER_IP`
  (default 32), env czytany w `core/app::Config::from_env`, threaded do
  `PlayerServer` builderem; odmowa `incoming.refuse()` PRZED handshakiem
  (`quinn::Incoming::remote_address()` dostępne pre-handshake); zliczanie RAII-guardem
  (wzorzec `ShutdownState::InFlightGuard`, `core/edge/src/server.rs:144-207`).

**Dispatch:** effort dla subagentów impl: **high** ([sonnet]-mechaniczne: medium).
Commit po każdym kroku, Conventional Commits, trailer wg wykonującego modelu.
Praca na masterze (memory: work-on-master). Testy: JEDEN run naraz — **prompt KAŻDEGO
subagenta uruchamiającego testy musi zawierać pre-check
`Get-Process | Where-Object { $_.ProcessName -match '^cargo$|^rustc$' }` i zakaz
równoległych runów** (CLAUDE.md, twardy protokół).

**Punch lista reviewera (think hard) — rozpatrzona:** #1 C#-surface → wcielone do
Step 5; #2 stale AGENTS.md research → Step 1 skorygowany (grep-verify przed edycją);
#3 `Error::invalid`; #4 właściciel wipe'u (jawny DROP SCHEMA w Step 5); #5
`$RUN_SUFFIX` w ReportId; #6 koszt probe odnotowany; #7 doc modułu invalidation +
deadline w `Registration::run(d)`; #8 wspólny snapshot faz; #9 `validate_ident`
jawnie w fazie 1; #10 scope commita Step 1; #11 protokół testowy w promptach.

---

## Step 0 — kopia planu do repo [inline]

- **(a)** Nowy plik `docs/plans/2026-07-11-<HHMM>-review-remediation-plan.md` (treść =
  ten plan).
- **(b) why now:** repo = źródło prawdy; kolejne kroki linkują do planu w commitach.
- **(c)** Wklejenie 1:1 + commit `docs(plans): review remediation plan`.

## Step 1 — sweep dokumentacyjny (26 STALE hitów + banner + resync AGENTS.md) [sonnet]

- **(a) co:** wg kompletnej tabeli z researchu:
  - `README.md`: :46-49 (outbox/relay/inbox → shared XID-log + pull + checkpoint-tx),
    :85 („12 fortresses" → 11 + gateway), :99 (in-memory MMR → persistent projection),
    :125 (usuń `outbox/` z layoutu).
  - `CLAUDE.md`: :157 i :367 (12→11 fortresses), :198 (7 topics → 6 ledger topics =
    6 subskrypcji `on_tx_raw` + 7. niezależna subskrypcja prune na `scheduler.fired`).
  - `AGENTS.md` — **UWAGA (korekta po review):** plik został już zresynchronizowany
    z CLAUDE.md (git diff --no-index = tylko sekcje workflow); 13-hitowa lista z
    researchu była NIEAKTUALNA — NIE stosować jej. Do zrobienia wyłącznie te same
    3 poprawki co w CLAUDE.md: 12→11 fortresses (nagłówek „Domain modules" + linia
    layoutu `modules/`), 7→6 audit ledger topics (+ 7. subskrypcja prune), oraz
    smoke-curl `match/report` (`AGENTS.md:319`) — ten ostatni dopiero w Step 5 razem
    z resztą curl-i. Przed edycją ZAWSZE grep-verify, że linia zawiera oczekiwany
    stary tekst; jeśli nie — pomiń i odnotuj.
  - `core/bus/src/lib.rs:20`, `api/accounts/events/src/lib.rs:9`,
    `modules/audit/src/lib.rs:19-22` (+ „all five" → six),
    `modules/gateway/src/lib.rs:21-22,26` (usuń klauzule `POST /events`),
    `cmd/leaderboard-svc/Cargo.toml:14` (inbox-dedup tx → delivery tx),
    `cmd/audit-svc/Cargo.toml:10` (inbox → subscription checkpoints).
  - `docs/reference/baas-feature-gap-matrix.md`: banner na górze
    „> **ARCHIWALNE (Go-era, 2026-07-07)** — opisuje port Go; struktura
    (m.in. `outbox/`) nieaktualna. Stan bieżący: CLAUDE.md."
- **(b) why now:** zero ryzyka, zero zależności; zamyka największą liczbę ustaleń od
  razu i czyści kontekst przed zmianami kodu.
- **(c) how:** czysto tekstowe edycje; NIE dotykać `experiments/`, `docs/plans/*`,
  `docs/summaries/*` (zapis historyczny), sekcji SANCTIONED (negacje, legacy-drop DDL,
  „relay" w sensie HTTP-byte-forwarding). Po edycji kontrolny grep
  `outbox|inbox|EVENTS_|POST /events` po README/CLAUDE/AGENTS — 0 hitów STALE.
  Weryfikacja: `cargo build --workspace` (doc-comments się kompilują; pre-check
  `Get-Process cargo|rustc` przed runem). Commit
  `docs(readme,claude-md,agents,bus,audit,gateway,cmd): retire push-plane vocabulary;
  fix fortress/topic counts` (scope obejmuje też doc-comments .rs i 2× Cargo.toml —
  to nadal `docs`, zmiany czysto komentarzowe).
- **(d)** [sonnet], effort medium.

## Step 2 — Epic LINK: ownership-check przy Taken [opus]

- **(a) co:** `modules/accounts/src/epic_oauth.rs` (branch LINK, :227-239) +
  `modules/accounts/src/tests.rs`.
- **(b) why now:** najmniejszy fix o realnym skutku; niezależny od reszty.
- **(c) how:** w gałęzi `Err(StoreError::Taken)` wywołać ISTNIEJĄCE
  `svc.store.player_by_identity("epic", &subject)` (store.rs:126-143): `Some(owner)`
  i `owner.id == p.id` → `/?epic=linked`; inaczej (inny gracz lub `None` — race) →
  `tracing::warn!` + `/?epic=error`. Żadnych zmian w store/DDL/webui. Testy:
  (1) rozszerzyć `link_identity_attaches_and_rejects_duplicates` o rozróżnienie
  same-player/other-player na poziomie store NIE trzeba (store słusznie zwraca Taken
  w obu przypadkach) — nowe testy na poziomie handlera: sklonować
  `epic_oauth_link_flow_end_to_end` (fake JWKS + token endpoint, tests.rs:449-530) na
  (2a) kolizję cross-player → 303 `/?epic=error` + `identities_of` gracza A bez zmian,
  (2b) idempotentny re-link tego samego gracza → 303 `/?epic=linked`, bez duplikatu.
  Commit `fix(accounts): epic link reports success only for the same owner`.
- **(d)** [opus], effort high.

## Step 3 — invalidation: per-callback deadline + bounded stop [opus]

- **(a) co:** `core/invalidation/src/lib.rs` (+ `gauges.rs` bez zmian — timeout idzie
  istniejącą ścieżką failure), `core/invalidation/src/tests.rs`.
- **(b) why now:** przed krokami 5–6 (readiness/QUIC), żeby teardown był już spójnie
  bounded, gdy split-proof zacznie mocniej gonić lifecycle.
- **(c) how:**
  - `InvalidationPlane`: nowe pole `callback_timeout: Duration`; env
    `INVALIDATION_CALLBACK_TIMEOUT_MS` (default 10_000, wzorzec `poll_from_env`
    lib.rs:374-382, czytany w `InvalidationPlane::new`); builder
    `with_callback_timeout(Duration)` (lustro `with_poll_interval` :231-234).
  - Deadline mieszka w **`Registration::run(d: Duration)`** (sygnatura przyjmuje
    deadline; NIE w RunCtx) — bo wołają go DWA miejsca: `RunCtx::run_one` (steady
    state) ORAZ pętla first-refresh w `start()` (:253-257), która NIE przechodzi
    przez run_one. Implementacja: `match tokio::time::timeout(d, (self.refresh)())
    .await { Ok(r) => r, Err(_) => bail!("refresh timed out after {d:?}") }` — 3-way
    idiom z `readyz_response` (`core/app/src/lib.rs:833-865`). W steady state timeout
    płynie przez istniejący `run_one` (`inc_failure` + `tracing::error!`); w `start()`
    timeout ⇒ głośny fail startu (oba boot-fille mają własne wcześniejsze gwarancje:
    `Config::start` refresh, `RemoteBoot` — ciasny deadline bezpieczny).
  - Doc modułu (`core/invalidation/src/lib.rs:24-32`) opisuje kontrakt
    startup/refresh — DODAĆ zdanie o deadline'ie i stop-grace (plik jest częścią
    kontraktu, nie pominąć).
  - `stop()` (:296-303): `for t in tasks { if timeout(STOP_GRACE, t).await.is_err()
    { t.abort(); } }` — uwaga: `t.await` konsumuje handle, więc wzorzec:
    `let h = t; match timeout(..., &mut h)…` lub `abort_handle()` przed await; stała
    `DEFAULT_STOP_GRACE_MS = 5000` (bez env — jak `READY_CHECK_TIMEOUT`, to nie
    tuning surface).
  - Testy (wzorce już w tests.rs): (1) no-DB hand-built `RunCtx` — wiszący callback
    (pending oneshot) z krótkim `with_callback_timeout` NIE blokuje siblinga
    `counting` (wzór `failing_callback_does_not_block_sibling` :167-204);
    (2) `first_refresh_timeout_fails_start` (wzór `first_refresh_failure_fails_start`
    :208-219, DSN nieużywany); (3) `stop_returns_despite_hung_callback` — plane z
    wiszącym callbackiem, `tokio::time::timeout(10s, plane.stop())` w teście
    przechodzi.
  - Commit `fix(invalidation): bound callback refreshes and plane stop
    (INVALIDATION_CALLBACK_TIMEOUT_MS)`.
- **(d)** [opus], effort high.

## Step 4 — transakcyjny apply_edit (apikeys + config) [opus]

- **(a) co:** `modules/apikeys/src/{store.rs,admin.rs,admin_tests.rs}`,
  `modules/config/src/{lib.rs,tests.rs}`. Moduł admin BEZ zmian (patrz decyzja).
- **(b) why now:** niezależny; przed krokiem 5 (match), żeby split-proof przechodzić
  raz po obu zmianach domenowych.
- **(c) how:**
  - **Jeden snapshot dla obu faz (anty-TOCTOU):** odczyt stanu (`store.list()` /
    `svc.all()`) wykonywany RAZ; faza 1 waliduje na tym snapshocie i buduje listę
    zaplanowanych zapisów; faza 2 wykonuje dokładnie tę listę w tx — żadnego
    ponownego odczytu/diffu między fazami.
  - **apikeys**: w `Store` dodać warianty na `&mut PgConnection`:
    `set_policy_tx/insert_tx/revoke_tx` (SQL bez zmian; istniejące metody zostają dla
    innych callerów). `apply_edit` (admin.rs:182): **faza 1 — pełna walidacja bez
    zapisu** (wszystkie `check_policy` dla diffów + tripla `_new_*`; pierwszy błąd →
    `return Err`, zero zapisów); **faza 2** — `let mut tx = svc.store.pool.begin()`
    (pool jest `pub` w Store :24-26), wszystkie zapisy `_tx`, `tx.commit()`.
  - **config**: walidacja identów żyje dziś WEWNĄTRZ `set` (:222-228) — wyciągnąć ją
    do współdzielonego `validate_ident(ns, key)` wołanego JAWNIE w fazie 1 (także dla
    tripla `_new_*`), a `set_tx` też ją woła (defense in depth) — inaczej add-row
    ominąłby fazę 1 i wróciłby partial-commit. `Service::set_tx(&self, conn, ns, key,
    value)` (upsert :229-237 bez zmian); `apply_edit` (:505) — ta sama dwufazówka.
    Trigger jest FOR EACH ROW, więc
    N zapisów w 1 tx nadal emituje N `config.changed` z rosnącą rewizją — commit
    czyni je widocznymi atomowo; NIE ruszać testu
    `each_mutation_emits_one_event_with_operation_value_and_revision` (:247-292),
    on dotyczy autocommitowych `set()`.
  - Testy: dla OBU modułów nowy „mixed submit": jedno pole poprawne + jedno błędne w
    JEDNYM wywołaniu → `Err`, store NIETKNIĘTY (w apikeys też: poprawny add-row +
    błędna polityka istniejącego klucza → nic nie weszło); dla config dodatkowo:
    udany batch 2 zmian → 2 zdarzenia `config.changed`
    (`asyncevents::testing::events_count`).
  - Commit `fix(apikeys,config): validate whole admin form, apply in one transaction`.
- **(d)** [opus], effort high.

## Step 5 — idempotentny match.report (report_id) [fable]

- **(a) co (PEŁNY sweep callerów — uzupełniony po review):**
  `api/match/api/src/lib.rs` (trait), `modules/match/src/lib.rs` (DDL + insert +
  report) i testy, `docs/reference/public-api-baseline/matchapi.txt` (re-bless),
  `split-proof.sh` + `split-proof.ps1`, smoke-curl w `CLAUDE.md`, `README.md` (jeśli
  jest) i `AGENTS.md:319`, **oraz powierzchnia C#**: asercje [C5]/[C6] gbclient w
  `verify.sh:519-531` / `verify.ps1:505-514` (dodać ReportId do wywołania),
  regeneracja `clients/csharp/Generated/Client.cs` (blokujący stage `codegen-fresh` =
  regenerate + `git diff --exit-code`), goldeny `tools/csharp-client-gen`
  (`src/tests.rs:64,97-99`, `testdata/manifest.golden.json:212-227`,
  `testdata/Client.golden.cs`). Na końcu kroku kontrolny grep `report(` /
  `match.report` po całym drzewie (poza experiments/) — zero pominiętych callerów.
- **(b) why now:** po 1–4, bo to jedyna zmiana kontraktu publicznego (re-bless) i
  wymaga rozszerzenia split-proof — domykamy nią rollout przed finalnym verify.
- **(c) how:**
  - Trait: `async fn report(&self, report_id: String, winner: String, loser: String)`
    z `body_names(report_id = "ReportId", winner = "Winner", loser = "Loser")` —
    glue (`matchrpc`) regeneruje się w 100% z traita (meta-callback macro; pierwszy
    parametr nie-Identity jest legalny — special-case dotyczy tylko typu `Identity`),
    zero zmian ręcznych w `api/match/rpc`.
  - Walidacja: puste `ReportId` → `Error::invalid` (`core/opsapi/src/lib.rs:188`) —
    brak klucza nie może cicho degradować dedupu.
  - DDL (wipe-strategy — kolumna prosto w `CREATE TABLE IF NOT EXISTS`):
    `report_id text NOT NULL, … UNIQUE (report_id)`. **Wipe ma właściciela:** w tym
    kroku, PRZED pierwszym testem, jawnie
    `psql … -c "DROP SCHEMA IF EXISTS match CASCADE"` na lokalnej bazie (CREATE IF
    NOT EXISTS nie doda kolumny do istniejącej tabeli; nic w verify/split-proof nie
    robi wipe'u za nas).
  - `insert_tx` → `INSERT … ON CONFLICT (report_id) DO NOTHING RETURNING id::text`,
    `fetch_optional`; `None` (duplikat) ⇒ **pomiń `emit_tx`, zwróć Ok(())** (wzorzec
    `inventory.wiped_characters`, `modules/inventory/src/lib.rs:394-407`). MMR-read
    zostaje bez zmian (sync dep exercised jak dotąd).
  - Split-proof (OBA skrypty, sh: [K3]/[K4] :643-661, MATCH TRIO :1053-1149; ps1
    lustrzanie): dodać `"ReportId"` do istniejących POSTów — **każde id per-run-unique
    z `$RUN_SUFFIX`** (skrypt czyści leaderboard/rating między runami, ale NIE
    `match.matches` — stały ReportId zdedupowałby się przy DRUGIM uruchomieniu
    split-proof i [MT2]/[MT4] by padły); [MT1] i [MT4] dostają różne id. Nowa asercja
    **[MT6] duplicate-report-idempotent**: re-POST z ReportId z [MT1] → 202,
    leaderboard `wins` bez zmiany (nadal 2), `match.matches` count dla tego report_id
    = 1 (psql). Rating [MT5] bez zmian (dalej 1030/970 — duplikat nic nie dodał).
    [K3]/[K4] mogą użyć `$RUN_SUFFIX` też (K4 przy stałym id testowałby ścieżkę
    dedup zamiast insertu w kolejnych runach).
  - Re-bless: `./verify.ps1 -BlessPublicApi` (tylko `matchapi.txt` może się zmienić —
    zweryfikować diff przed commitem).
  - Docs: smoke-curl `-d '{"ReportId":"demo-1","Winner":"alice","Loser":"bob"}'`.
  - Testy modułu: nowy test „duplicate report_id emits once" (dwa `report` z tym
    samym id → 1 wiersz, 1 event przez `asyncevents::testing::events_count`).
  - Commit `feat(match)!: idempotent report via client ReportId (dedup on retry)`.
- **(d)** [fable], effort high.

## Step 6 — readiness per-stub (bounded dial) [opus]

- **(a) co:** `core/remote/src/lib.rs` (`Stub::init`), test w `core/remote` (lub
  `core/app/src/tests.rs` — tam już są fake-checki), split-proof: nowa asercja.
- **(b) why now:** po kroku 5, żeby nowe asercje split-proof dodawać na już-zielonym
  skrypcie; niezależny od 2–4.
- **(c) how:** w `Stub::init` (obok istniejącej kontrybucji `PEER_SLOT`, :350-359)
  kontrybuować `httpmw::ReadyCheck::new(format!("stub:{provider}"), …)`: body = parse
  `peer_addr` → `edge::shared_dev_ca()` → `tokio::time::timeout(1s,
  edge::Client::dial(addr, &ca))` → `close()`; własny wewnętrzny timeout (nie polegać
  tylko na zewnętrznym `READY_CHECK_TIMEOUT`, żeby nie zostawiać wiszącego dialu).
  UWAGA: `core/remote` NIE zależy dziś od `core/httpmw` — dodać dependency (core→core,
  legalne — archcheck rule 16 banuje tylko core→module/api). Kontrybucja z `init` OK
  (READINESS_SLOT czytany lazily per request). Efekt fleet-wide: każdy svc ze stubami
  raportuje osiągalność swoich peerów — semantycznie poprawne (stub = twarda zależność
  sync); wszystkie peery wszystkich bootowanych svc split-proof podnosi (gateway 6,
  admin 4, inventory 2, match 1), a split-proof polluje dziś tylko `/healthz`, więc
  boot się nie zakleszczy. **Koszt świadomie zaakceptowany:** probe = świeży QUIC
  dial per stub, sekwencyjnie (gateway: 6 mTLS handshake'ów per /readyz; martwa flota
  ≈ 6×1s) — akceptowalne dla lokalnego/dev deploya; odnotować w doc-commencie checku.
  Monolith: zero stubów ⇒ zero zmian. Test Rust: check na nieosiągalny adres zwraca
  Err w <2s; check na żywy `edge::Server` (wzorzec z `core/edge/src/server_tests.rs`)
  zwraca Ok. Split-proof: asercja **[GW-RDY]** — `GET :8082/readyz` = 200 przy pełnej
  flocie (oba skrypty). Commit
  `feat(remote): per-stub bounded readiness check (stub:<provider> on /readyz)`.
- **(d)** [opus], effort high.

## Step 7 — limity połączeń player-QUIC [opus]

- **(a) co:** `core/edge/src/player.rs` (+`player_tests.rs`), `core/app/src/lib.rs`
  (Config + threading), `cmd/gateway-svc/src/main.rs` i `cmd/server` bez zmian
  (defaults w `core/app`).
- **(b) why now:** ostatnia zmiana kodu — izolowana, nie wpływa na wcześniejsze kroki.
- **(c) how:** `PLAYER_MAX_CONNS` (default 1024) i `PLAYER_MAX_CONNS_PER_IP`
  (default 32) czytane w `Config::from_env` (wzorzec parsowania grace-knobów,
  lib.rs:183-188), pola na `Config`, przekazane do `PlayerServer` builderem
  `with_conn_limits(global, per_ip)` PRZED `listen` (env-blind core/edge). W accept
  loop (:136-152): `incoming.remote_address()` PRZED `incoming.await`; stan
  `Arc<Mutex<HashMap<IpAddr, usize>>>` + globalny `AtomicUsize`; przekroczenie ⇒
  `incoming.refuse()` (bez handshaku) + `tracing::warn!` (rate-limited logging nie
  jest wymagane w tym kroku); RAII-guard (wzorzec `InFlightGuard`,
  `core/edge/src/server.rs:144-207`) dekrementuje przy zamknięciu połączenia; wpis
  mapy usuwany przy 0 (bez osobnego evictora — dekrement domyka cykl życia).
  Test w `player_tests.rs`: listen z `with_conn_limits(2, 2)` na `127.0.0.1:0`,
  3 równoległe dialy → trzeci odrzucony; potem zamknięcie jednego → czwarty dial
  przechodzi (zwolnienie guardu). Commit
  `feat(edge): player-QUIC connection caps (PLAYER_MAX_CONNS[_PER_IP])`.
- **(d)** [opus], effort high.

## Step 8 — finalna weryfikacja + memory [inline]

- **(a/c):** po kolei, JEDEN run naraz (pre-check `Get-Process cargo|rustc`):
  1. `cargo run -p archcheck` + `cargo run -p topiccheck` (nowa zależność
     remote→httpmw, brak zmian w grafie subskrypcji poza… żadnych — report_id nie
     zmienia topiców),
  2. pełny `./verify.ps1` (build, clippy -D warnings, test, audit, fortress,
     split-proof — z nowymi [MT6]/[GW-RDY]); public-api już re-blessed w kroku 5,
  3. audyt trailerów: `git log --format="%h %B" | grep Co-Authored` — lane↔model,
  4. `scripts/memory-sync.ps1 push` jeśli memory się zmieni.
- **(b):** verify.sh JEST CI tego repo; split-proof dowodzi obu topologii (never
  monolith-only).
- **(d)** [inline].

## Verification (całość)

- Idempotencja match: [MT6] w split-proof (duplikat ReportId → wins bez zmian) +
  test jednostkowy „1 wiersz, 1 event".
- Epic: 2 nowe testy e2e handlera (kolizja → error + identities nietknięte;
  re-link → linked).
- Formularze: testy „mixed submit ⇒ Err + store nietknięty" w obu modułach.
- Invalidation: 3 testy (sibling nie blokowany, start fail-loud na timeout,
  stop bounded).
- Readiness: [GW-RDY] + test dial-unreachable/dial-live.
- QUIC caps: test 3-go odrzuconego + zwolnienie po zamknięciu.
- Docs: kontrolny grep zero-STALE po README/CLAUDE/AGENTS.

## Poza zakresem (świadomie)

- #6 recenzji (wspólne CA / tożsamość floty) — checklist Hetzner, osobna inicjatywa.
- Strukturalne per-field errors w `adminapi::SubmitFn` — dwufazowa walidacja załatwia
  atomowość bez zmiany kontraktu; per-field UX to osobny temat.
- Przenoszenie Quarkus-docs z `docs/reference/` — flagged AMBIGUOUS, nie blokuje.
