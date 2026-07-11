# Plan naprawczy — findings architektoniczne 1–5 + mniejsze

## Context

Plan zamyka findings 1–5 oraz sekcję „Mniejsze, lecz realne” z audytu 2026-07-11.
Research: sześć read-only subagentów na niepokrywających się kątach (retry/RPC,
event plane, admin security, player QUIC, verification/wipe, patterns/scope),
targeted full-file reads, Cargo metadata i historia Git. `rg` był wyłącznie
oznaczonym lower-bound locatorem. Datowane plany/reviews/statusy są archiwum zmian,
nie specyfikacją HEAD.

### Korekta findingu #1

Pierwotny opis był zbyt szeroki. Bieżący split HTTP/player dispatch w
`modules/gateway/src/backend.rs` używa `edge::Client` i po błędzie resetuje cache,
ale nie powtarza tego samego requestu. `characters.create/delete` i
`inventory.grant` nie są obecnie dublowane przez `remote::Reconnecting`.
`Reconnecting` obsługuje wygenerowane registry-capability clients, których aktualne
użycia są odczytami. Realnym problemem jest przyszłościowa, fail-open semantyka:
każda nowa metoda capability byłaby automatycznie retry'owana. Naprawa należy do
seamu, bez dokładania request-id do kontraktów domenowych.

### Overlap i „why not extend/depend on X”

- Retry korzysta z istniejących `opsapi::Caller`, `opsapi::Operation`, generatora
  `#[rpc]` i `remote::Reconnecting`; nie powstaje nowy transport ani moduł.
- Fairness i bounded stop rozszerzają app-owned `core/asyncevents`; nie powstaje
  scheduler/broker ani moduł messaging.
- Login hardening pozostaje wewnątrz fortecy `admin` i wykorzystuje istniejące
  `admin.login_attempts`, sesje, `httpmw` oraz transakcyjne advisory locki; nie
  powstaje capability ani osobny security module.
- Player admission pozostaje w `core/edge`/`core/app`, bo tylko edge zna źródłowy IP
  i lifecycle połączenia; gateway nadal odpowiada za API-key/session/policy.
- Match używa istniejących `report_id`, `winner`, `loser`; nie zmienia kontraktu
  `matchapi` ani eventu `match.finished`.

### Twarda decyzja bazodanowa

Ten projekt nie dostaje migracji danych. Plan wymaga **zero `ALTER`, backfilli,
dual-write, compatibility bridges i wersjonowanej migracji danych**. Wszystkie
zaplanowane zmiany poza adminem działają na obecnych schematach. Admin dostaje indeks
retencji w docelowym `AUTH_DDL`, dlatego rollout jawnie wykonuje
`DROP SCHEMA IF EXISTS admin CASCADE`, świeży boot oraz reseed przez
`cargo run -p adminctl -- create-user`. To wipe, nie migracja: żadnego
retrofitowania starej tabeli, `ALTER`, backfillu ani zachowywania dev sessions.

## Verified facts

- `core/remote/src/lib.rs::Reconnecting::call` retry'uje każdy `Caller` error raz.
- `tools/rpc-macro/src/lib.rs::gen_client_method` generuje wywołania `Caller::call`;
  ten sam model generuje `opsapi::Operation` używany przez gateway.
- `core/asyncevents/src/worker.rs::drain_pass` drenuje jedną subskrypcję do pustki;
  `core/asyncevents/src/lib.rs::WORKERS` ma wartość 2.
- `Plane::start` przechowuje supervisor handles, a faktyczny worker jest
  zagnieżdżonym taskiem; abort samego supervisora odłączyłby workera.
- `modules/admin/src/lib.rs::login_submit` rozdziela lock check, synchroniczny
  Argon2 i failure update; `password.rs` używa 64 MiB Argon2.
- `admin.login_attempts` ma `subject`, `fails`, `locked_until`, `updated_at`; obecny
  docelowy DDL dostanie indeks `updated_at`, a stary schemat zostanie skasowany.
- `modules/match/src/lib.rs::Service::report` odpytuje rating przed rozpoznaniem
  duplikatu, a konflikt `report_id` nie porównuje payloadu.
- Player QUIC ma connection/stream caps, ale nie request-rate limit; API-key cache
  czyści całą mapę po osiągnięciu limitu.

## Step 1 — fail-closed retry policy w seamie RPC [subagent-complex]

**(a) Co:**

- `core/opsapi/src/lib.rs`: dodać publiczne
  `RetryMode::{Never, OnceAfterReconnect}`, pole `retry_mode` do `Operation` oraz
  parametr `retry_mode: RetryMode` do `Caller::call`.
- `tools/rpc-macro/src/lib.rs`: oba etapy parsera prywatnego markera metod
  `#[retry_safe]` — pierwotne rozwinięcie `#[rpc]` oraz ponowny parse tokenów przez
  meta-callback `generate_glue!`; usunięcie markera z emitowanego traita/meta tokenów
  oraz propagacja `RetryMode` zarówno do generated client call, jak i `Operation`.
- Wszystkie `api/*/api/src/lib.rs`: oznaczyć `#[retry_safe]` wyłącznie czyste
  odczyty: session verification/me, admin data, API-key lookup, ownership/list,
  config snapshot, inventory list, leaderboard top i rating MMR. Nie oznaczać
  auth/session mutations, characters create/delete, inventory grant ani
  `match.report`; mutacja pozostaje `Never` nawet po jej idempotency hardening.
- `core/remote/src/lib.rs::Reconnecting::call`: przy pierwszym błędzie zawsze
  zresetować cached connection; dla `Never` zwrócić pierwszy błąd bez replay, dla
  `OnceAfterReconnect` redialować i wykonać dokładnie jedną próbę.
- Uaktualnić implementacje/fakes/literals w `core/edge/src/client.rs`,
  `core/opsapi/src/{lib.rs,tests.rs}`, `core/remote/src/{lib.rs,tests.rs}`,
  `modules/gateway/src/tests.rs`, `cmd/gateway-svc/tests/stub_swap.rs`,
  `tools/rpc-macro/src/lib.rs` i `tools/rpc-macro-tests/tests/roundtrip.rs`.
  Markery trafiają dokładnie do `api/{accounts,admin,apikeys,characters,config,
  inventory,leaderboard,rating}/api/src/lib.rs`; odpowiadające
  `api/*/rpc/src/lib.rs` muszą skompilować się bez ręcznych zmian glue.

**(b) Dlaczego teraz:** to fundament dla wszystkich kolejnych zmian i zamyka klasę
przyszłych lost-response dubli bez mutowania publicznych requestów domenowych.
Default `Never` sprawia, że brak adnotacji degraduje tylko dostępność, nigdy
poprawność.

**(c) Jak:** marker jest metadata codegenu, nie częścią wire format ani publicznej
sygnatury traita. Generator musi zasilić obie topologie tym samym `RetryMode`;
gatewayowy `edge::Client` może ignorować tryb, ponieważ sam nie replay'uje, ale
`Operation` zachowuje semantykę i checker/test może wykryć drift. Testy:
`core/remote/src/tests.rs` — safe read retry once, unsafe mutation dokładnie jedno
call + reset, kolejny niezależny request redialuje, persistent safe error kończy się
po jednym retry; macro/RPC tests — marker propagowany, brak markera = `Never`.
Domain contract baselines i C# wire goldeny mają pozostać bez zmian. `RetryMode`,
pole `Operation` i parametr `Caller` są świadomą zmianą publicznego foundation API
`opsapi`; dodać downstream compile tests dla wszystkich explicit `Operation`
literals/fakes. Compile test macro musi dowieść, że `#[retry_safe]` nie wycieka jako
nieznana adnotacja ani z trait expansion, ani z meta expansion. Żaden baseline diff
nie jest automatycznie blessowany.

## Step 2 — ścisła idempotencja `match.report` [subagent-complex]

**(a) Co:** `modules/match/src/lib.rs` oraz `modules/match/src/tests.rs`.

**(b) Dlaczego teraz:** po wspólnym Step 1 kroki 2–6 są niezależnymi granicami
review. Match idzie pierwszy, bo jest najmniejszą domenową poprawką i szybko domyka
semantykę `ReportId` przed cięższymi zmianami infrastruktury.

**(c) Jak:** dodać store lookup istniejącego `(winner, loser)` po `report_id`.
`Service::report` po walidacji robi lookup **przed** `rating.mmr`: identyczny payload
zwraca `Ok(())` bez rating call; inny payload zwraca stabilny
`Error::conflict("ReportId already used for a different match")`. Dla race dwóch
requestów zachować `INSERT ... ON CONFLICT DO NOTHING`; gdy insert zwróci `None`,
odczytać zwycięski wiersz w tej samej transakcji i zastosować identyczne porównanie.
Nowy rekord nadal wykonuje row + `emit_tx` w jednym tx. Testy: same ID/same payload =
1 row/1 event; same ID/different payload = 409 i brak zmiany; replay przez Service z
failing/counting `MmrReader` = sukces i zero MMR calls; równoległa kolizja akceptuje
dokładnie jeden payload. Obecne kolumny wystarczają: bez DDL i wipe.

## Step 3 — fairness, fail-loud timeout i bounded stop event plane [subagent-complex]

**(a) Co:** `core/asyncevents/src/worker.rs`, `core/asyncevents/src/worker_tests.rs`,
`core/asyncevents/src/lib.rs` i plane-level tests w osobnym `src/tests.rs`.

**(b) Dlaczego teraz:** to niezależna granica review po Step 1. Wewnątrz tego kroku
refaktor task/connection ownership musi poprzedzić timeout w `stop`, inaczej abort
odłączy zagnieżdżonego workera lub odda manualny tx do poola.

**(c) Jak:**

1. Wprowadzić stały dodatni quantum `DELIVERIES_PER_SUB_PASS = 64` i po maksymalnie
   64 `Step::Delivered` przejść do następnej subskrypcji. `Empty`, `Skipped`,
   `Faulted` i error nadal kończą bieżący quantum. `deliver_one`, row lock,
   handler+checkpoint tx i XID order pozostają nietknięte.
2. Zmienić parser na `handler_timeout_from_env() -> anyhow::Result<Duration>`:
   unset = 10 s; zaakceptować jawne `ms/s/m` i bare seconds; odrzucić zero, pusty,
   malformed suffix, non-Unicode i checked-arithmetic overflow. `Plane::start`
   przenosi parse przed `snapshot`/catalog reconcile/task spawn i failuje startup z
   nazwą zmiennej bez DB mutation.
3. Usunąć nested worker spawn. Każdy worker dostaje własne, bezpośrednie
   `PgConnection::connect(listen_dsn)` zamiast zwracanego do wspólnego poola
   `PoolConnection`; `deliver_one` pracuje na `&mut PgConnection`. Po poison/zerwanej
   sesji worker redialuje przed następnym eventem. Przerwany manualny tx nigdy nie
   wraca do poola modułów. Przechowywać rzeczywiste worker handles; wrapper z guardem
   ustawia liveness dead tylko przy nieoczekiwanym końcu przed stopping.
4. Dodać `DEFAULT_PLANE_STOP_GRACE_MS = 5000` i test-only `with_stop_grace`.
   `ActiveDeliveries` mapuje `worker_id -> {generation,pid,state:
   Active|Terminating}`. Worker pobiera PID prywatnej sesji i rejestruje generację
   **przed** ręcznym `BEGIN`; completion/poison usuwa wpis pod tym samym mutexem.
   Po grace `Plane::stop` atomowo claimuje `Active` jako `Terminating`; normal
   completion widzący ten stan zamyka sesję zamiast jej ponownie użyć. Dla każdego
   claimu stop otwiera control `PgConnection::connect(listen_dsn)` i wykonuje
   `pg_terminate_backend(pid)` pod **pozostałym wspólnym deadline'em** — connect/query
   nie dostają nowych timeoutów. Po potwierdzeniu braku tej generacji/PID abortuje i
   awaituje odpowiadający worker, gdy handler jest kooperatywnym async future (sleep,
   I/O, lock wait); jego prywatna sesja jest już martwa. Gdy control
   connect/terminate wyczerpie deadline,
   stop abortuje kooperatywny task, co dropi jego prywatny socket, loguje forced-close
   i kończy bounded; żaden shared-pool connection nie jest skażony. Nie wolno zakładać
   rollback-on-drop dla manualnego `BEGIN` ani abortować samego supervisora. Jawna
   granica: Tokio nie potrafi zabić future wykonującego nieskończony CPU loop bez
   `.await`; `JoinHandle::abort` jest kooperatywny. Rollout gwarantuje bounded stop dla
   DB/network stalls i poprawnych async handlers, nie dla procesu z CPU-spin bugiem —
   taki proces wymaga zewnętrznego hard kill.
5. Testy: hot A z backlogiem > quantum nie blokuje B w jednym `drain_pass`; osobny
   production-shape test uruchamia dwa workery, dwa stale gorące pierwsze subs i
   późniejszą subskrypcję, która musi zrobić progress mimo `SKIP LOCKED`; porządek A
   pozostaje rosnący; parser table; invalid env nie robi reconcile ani task spawn;
   stuck `pg_sleep` i handler czekający na async synchronization kończą bounded stop,
   backend znika z `pg_stat_activity`, pool
   connection nie wraca z otwartym tx, effect/checkpoint nie commitują i żaden
   kooperatywny worker nie działa po stop; panic/premature exit ustawia readiness dead, kontrolowany stop
   nie daje false-positive.

Nie ma zmiany schematu ani koordynat checkpointów.

## Step 4 — resource-safe i transakcyjny admin login [subagent-complex]

**(a) Co:** `modules/admin/src/lib.rs::{AdminState,login_submit,AUTH_DDL}`,
`modules/admin/src/password.rs`, `modules/admin/src/tests.rs`,
`split-proof.sh` i `split-proof.ps1` assertions `[AD2b]`/`[AD2c]`.

**(b) Dlaczego teraz:** to niezależna granica review po Step 1 i łączy findings 3–5;
samo `spawn_blocking` bez serializacji, timing parity i limitu RAM byłoby niepełne.

**(c) Jak:**

- Dodać do `AdminState` `login_slots: Arc<Semaphore>` (32, `try_acquire_owned`,
  globalnie ogranicza queued requests), `argon_permits: Arc<Semaphore>` (2, permit
  oczekiwany dopiero po admission), istniejący `httpmw::IpLimiter::new(5.0, 20)` i
  licznik requestów. Po `resolve_ip`, przed DB, per-IP denial lub brak login-slot
  zwraca 429 + dokładny `Retry-After: 1`; co 256 requestów wołać
  `IpLimiter::evict_idle(Instant::now())`. Test constructors przyjmują jawne limity,
  więc concurrency-lockout test używa burst 64 i nie jest maskowany przez 429.
- Wydzielić `authenticate_and_mint(username, submitted, ip) -> LoginOutcome`.
  Otworzyć jeden tx; zdobyć `pg_advisory_xact_lock(hashtextextended(subject,
  stały_namespace))` dla posortowanych `user:<username>` i `ip:<addr>`; dopiero pod
  lockami ponownie sprawdzić lockout i pobrać hash/dummy hash. Exact SQL:
  `SELECT pg_advisory_xact_lock(hashtextextended($1, $2))` z `$2 =
  4702968888123215687_i64` (`ADMINLOG` namespace);
  subjects sortowane bytewise, hash collision wyłącznie nadmiarowo serializuje.
- `login_slots.try_acquire_owned()` jest przed oczekiwaniem na
  `argon_permits.acquire_owned()`, a oba są przed `pool.begin()`; maksymalnie 32
  bounded waiters czeka na dwa Argon permits. Closed semaphore daje 500. Oba permits
  są zwalniane na każdej ścieżce 401/429/500 po completion/rollback security tx.
  Przenieść owned hash/password do `tokio::task::spawn_blocking`; tx pozostaje pod
  advisory locks. Argon semaphore ogranicza koszt do dwóch trzymanych DB connections
  i ok. 128 MiB aktywnego Argon2 na proces.
- W tym samym tx: znany błędny user inkrementuje user+IP; nieznany/invalid user
  wykonuje dummy Argon2, ale inkrementuje wyłącznie IP; sukces czyści subjects,
  tworzy session i emituje `login-succeeded`; przekroczenie progu ustawia lock i
  emituje dokładnie jeden `login-locked`. Request widzący user/IP lock nadal robi
  dokładnie jeden dummy verify, ale nie zwiększa licznika; w przeciwnym razie szósty
  request byłby timing oracle dla istniejącego konta. Stary `record_failure` nie
  otwiera własnego tx.
- Po trim username ma `1..=128` bajtów, password maks. 1024 bajty. Invalid/overlong
  input nie wchodzi do Argon closure: closure dostaje stały dummy hash i stałe krótkie
  dummy password, robi jeden verify, zwraca generic 401 i nie tworzy `user:*` row.
- `AUTH_DDL` dodaje `CREATE INDEX IF NOT EXISTS admin_login_attempts_updated_idx ON
  admin.login_attempts(updated_at)`. Co 256 dopuszczonych prób (`fetch_add(1) % 256
  == 255`), **przed** security tx,
  osobne krótkie maintenance connection wykonuje dokładny CTE:
  `WITH stale AS (SELECT ctid FROM admin.login_attempts WHERE updated_at < now() -
  interval '24 hours' AND (locked_until IS NULL OR locked_until <= now()) ORDER BY
  updated_at LIMIT 256 FOR UPDATE SKIP LOCKED) DELETE FROM admin.login_attempts a USING stale WHERE
  a.ctid = stale.ctid`. Rollout zaczyna się od `DROP SCHEMA admin CASCADE`, boot i
  `adminctl` reseed; wipe jest jawną ręczną czynnością rolloutową, nigdy częścią
  `Admin::migrate`. Dodać test dwóch kolejnych `Admin::migrate` po fresh schema.
  Licznik jest shared per process; redundantny cleanup między replikami jest
  bezpieczny dzięki `FOR UPDATE SKIP LOCKED`. Żadnego ALTER/backfill.
- Dodać prywatny `PasswordVerifier` w `AdminState`; production adapter wywołuje
  niezmienione `password::verify_password` (to samo co `adminctl`), a test fake
  liczy i barieruje calls. Tests: concurrent same user/IP zamyka threshold dokładnie na 5/20 i emituje jeden
  lock event; dwa IP/jeden user oraz dwóch userów/jeden IP nie deadlockują; ghosty
  nie tworzą user rows; input bounds; GC zachowuje aktywny lock; limiter i semaphore;
  heartbeat na current-thread runtime postępuje podczas Argon2; structural verifier
  counter potwierdza dokładnie jeden verify dla known-wrong, unknown, user-locked,
  IP-locked i invalid input bez kruchych ścisłych timing thresholds.
- Split proof: `[AD2b]` tworzy świeżego dedykowanego usera i trusted-proxy IP,
  wysyła 12 bounded parallel jobs (mieści się w burst 20), następnie SQL
  sprawdza `fails=5` dla `subject='user:<name>'`, aktywny lock i dokładnie jeden `admin.action` filtrowany po tym
  aktorze; osobny `[AD2c]` na innym IP używa produkcyjnych 5/20 i sprawdza dokładny
  429 + `Retry-After`, żeby limiter nie maskował transakcyjnego testu lockoutu.

## Step 5 — player-QUIC request admission i bounded API-key cache [subagent-complex]

**(a) Co:** `core/edge/src/player.rs`, `core/edge/src/player_tests.rs`,
`core/app/src/lib.rs`, `core/app/src/tests.rs`, `modules/gateway/src/keys.rs`,
`modules/gateway/src/tests.rs`, `tools/playercli` dla persistent multi-call proof,
`split-proof.sh` i `split-proof.ps1`.

**(b) Dlaczego teraz:** to niezależna granica review po Step 1; player limit pozostaje
w edge, gdzie dostępne są source IP i connection lifetime, i ląduje przed Windows
proofem wykorzystującym ten sam RunningServer.

**(c) Jak:**

- Dodać env-blind `PlayerRequestLimits` oraz
  `PlayerServer::with_request_limits(per_ip_rps, per_ip_burst, per_conn_rps,
  per_conn_burst)`. `RequestLimiter` ma jeden mutex zawierający per-IP mapę i
  per-connection buckets indeksowane wewnętrznym `connection_id`; stream tasks wołają
  jedną metodę, więc nie istnieje drugi mutex ani odwrotna kolejność locków.
  `allow(ip, connection_id)` refiluje oba i odejmuje tokeny atomowo tylko gdy oba
  pozwalają; rate==0 lub burst==0 wyłącza wyłącznie odpowiedni poziom. Admission następuje po accept stream, ale przed frame
  read; denial robi `recv.stop(0)`, wysyła dokładny framed
  `ok:false,error:"player request rate limit exceeded"`, kończy send i nie uruchamia
  JSON/API-key/session/RPC. Token jest pobierany za każdy przyjęty stream, także pusty
  lub malformed. Per-IP bucket **przeżywa reconnect**; mapa trzyma `last_seen`, usuwa
  idle >3 min. `request_count.fetch_add(1) % 256 == 255` wykonuje retain idle, więc
  normalna ścieżka jest O(1). Przy cap 65,536 insertion ponownie usuwa expired, potem
  deterministycznie evictuje najstarszy `(last_seen, IP)`. Per-connection bucket
  usuwa `ConnGuard`; nie ma detached cleanup taska.
- `core/app::Config` czyta `PLAYER_RATE_LIMIT_RPS/BURST` (default 20/40) i
  `PLAYER_CONN_RATE_LIMIT_RPS/BURST` (10/20); zero jawnie wyłącza dany limiter.
  Env pozostaje w app, nigdy w module.
- `RealKeyVerifier`: odrzucić API key >256 bajtów przed cache/capability; zastąpić
  clear-all-at-10k przez opportunistic expired eviction, potem oldest-entry eviction;
  dodać `KeyFlights { locks: HashMap<String, Weak<tokio::sync::Mutex<()>>> }` dla
  prawdziwego per-key single-flight oraz 64-permit globalny semaphore. Miss zdobywa
  keyed lock, ponownie sprawdza cache, potem robi
  `global.try_acquire_owned()`; saturation zwraca `None` bez capability call, a
  istniejący `check_api_key` mapuje to na `KeyDenial::Invalid`. Permit obejmuje jeden
  lookup; cancellation zwalnia keyed mutex. `FlightEntry { weak, sequence }` ma
  osobny cap 10,000; co 256 missów robi globalny retain martwych Weak, przy cap
  ponawia retain, a gdy wszystkie entries są żywe, nowy distinct key failuje `None`
  bez insertion. Infra `Err` pozostaje uncached, więc kolejny waiter próbuje sekwencyjnie,
  nigdy stampede. Cache entry dostaje monotonic `inserted_at` + sequence; pod jednym
  mutexem najpierw usuwa expired, potem najstarszy `(inserted_at, sequence)` i zawsze
  zachowuje `len <= 10_000`. Plaintext/revocation semantics bez zmian.
- Tests: exact burst/refill, per-IP sharing, per-connection isolation, denial nie
  wywołuje handlera; config defaults/override/zero; overlong key = zero lookups;
  capacity nie czyści hot cache; expired-first i deterministic oldest eviction;
  concurrent same-key miss daje jeden capability call; cancelled leader nie blokuje
  followera; global saturation nie zwiększa capability calls. Denied QUIC stream test
  wysyła max frame równolegle i dowodzi, że stop/response nie wisi.
- Rozszerzyć `playercli` o persistent multi-call mode; split/monolith named proof
  zużywa per-connection burst, widzi pinned rate-limit response, czeka na refill i
  ponownie wykonuje poprawny request w szerokim timeout window. Exact refill jest
  testowany fake clockiem w unit tests; live proof nie opiera się na ciasnym sleep.
  Wielokrotne procesy CLI nie mogą udawać testu per-connection.

## Step 6 — Windows graceful-shutdown proof [subagent-complex]

**(a) Co:** `Cargo.toml`, nowe `tools/winctrl/{Cargo.toml,src/main.rs}`,
`core/app/src/lib.rs::shutdown_signal`, `core/app/src/tests.rs`, `split-proof.ps1`;
bashowy `split-proof.sh` tylko dla symetrycznej nazwy/assertion, bez zmiany
sprawdzonego TERM→KILL cleanup.

**(b) Dlaczego teraz:** Step 3 i Step 5 zmieniają event-plane handles oraz player
RunningServer, więc proof kompletnego drainu musi być po nich.

**(c) Jak:** `winctrl spawn --pid-file --stdout --stderr -- <exe> <args...>` używa
`CreateProcessW(CREATE_NEW_PROCESS_GROUP | CREATE_UNICODE_ENVIRONMENT)` przez
`windows-sys` features `Win32_Foundation`, `Win32_System_Console`,
`Win32_System_Threading`, `Win32_Security` i zachowuje redirection/log capture;
group id = child PID. CreateProcess failure jest błędem, a PID file powstaje atomowo
przez temp+rename przed exit helpera. `winctrl break <pid>` robi
`FreeConsole`, `AttachConsole(pid)`, `SetConsoleCtrlHandler(NULL, TRUE)`,
`GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid)` i `FreeConsole`; sukces oznacza
udane Win32 API call, nie sleep ani nieobserwowalne „delivery”.
`core/app::shutdown_signal` dodaje `tokio::signal::windows::ctrl_break()` do select.
`split-proof.ps1` uruchamia proof target przez `winctrl spawn`, wysyła BREAK, czeka na
bounded clean exit, port release oraz zakończenie in-flight request; Force pozostaje
wyłącznie stale-process cleanup i fallback, a użycie fallbacku failuje named
assertion. Helper ma Windows-only compile/test; signal-independent choreography
pozostaje testowana w `core/app/src/tests.rs`.

## Step 7 — symetryczne proofy i jeden finalny rollout [subagent-mechanical]

**(a) Co:** `core/remote/src/tests.rs`, `tools/rpc-macro-tests/tests/roundtrip.rs`,
`core/asyncevents/src/{worker_tests.rs,tests.rs}`, `modules/admin/src/tests.rs`,
`modules/match/src/tests.rs`, `core/edge/src/player_tests.rs`,
`core/app/src/tests.rs`, `modules/gateway/src/tests.rs`,
`tools/playercli/src/main.rs`, `split-proof.sh`, `split-proof.ps1`, `CLAUDE.md` i
repozytoryjny `AGENTS.md`. W dwóch bieżących guidance files dopisać wyłącznie nowe
retry-safe default, event fairness/stop grace, admin login bounds/wipe-reseed oraz
cztery player rate-limit envy; nie edytować datowanych plans/reviews/statuses.

**(b) Dlaczego teraz:** wszystkie zachowania i knob surfaces są ustalone; mechaniczna
symetria skryptów i finalne gates nie mogą wyprzedzić implementacji.

**(c) Jak:** uzupełnić wyłącznie jawnie wymienione testy i named assertions z Steps
1–6; `tools/csharp-client-gen/testdata/{manifest.golden.json,Client.golden.cs}` oraz
`docs/reference/public-api-baseline/*.txt` są kontrolowane przez diff i oczekuje się
braku zmian. Przed **każdym** cargo test/verify/split-proof sprawdzić
`Get-Process | Where-Object { $_.ProcessName -match '^cargo$|^rustc$' }` i czekać,
nigdy nie uruchamiać drugiego runu równolegle. Kolejno: focused non-DB tests,
focused DB package tests, `cargo run -p archcheck`,
`cargo run -p topiccheck -- --durability-strict`, public-api/codegen diff, a na końcu
jedno `./verify.ps1 -All -Strict`; nie uruchamiać osobnego split-proof podczas
verify. Każdy nowy cross-process flow ma named assertion w obu skryptach. Public API
i C# output mają pozostać bez wire diff; nie blessować przypadkowej zmiany.

## Expected commits / implementation review boundaries

Każdy krok jest osobną granicą review; commit jest tylko proponowany i wymaga
autoryzacji implementacyjnej użytkownika. Proponowane
scopes: `refactor(opsapi,remote,rpc-macro)`, `fix(match)`, `fix(asyncevents)`,
`fix(admin)`, `feat(edge,app,gateway,playercli)`, `test(app,split-proof)`,
`test(verify,split-proof)`. Implementacja używa planowych tagów powyżej; zmiana tagu
w trakcie wymaga zgody użytkownika.
