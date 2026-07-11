# Plan naprawczy — wszystkie findings z audytu 2026-07-11 (rundy 1+2)

> **Lokalizacja docelowa:** po akceptacji skopiować ten plik do repo jako
> `docs/plans/2026-07-11-<HHMM>-all-findings-remediation-plan.md` (Plans & Status
> Docs — MANDATORY: plany żyją w repo, nie na ścieżce `~/.claude`). Plan-mode
> wymusza edycję tylko tego pliku, stąd tymczasowa lokalizacja.

## Context

Dwie rundy review (audyt bazowy `ec58f83` + review remediacji `af26dc5`) zostawiły
**6 otwartych starych findings** (plan `2026-07-11-1319` celowo obejmował tylko 1–5)
i **5 nowych** z kodu remediacyjnego, plus jeden proceduralny. `af26dc5` domknął
poprawnie najcięższe (wyciek transakcji workera → CRITICAL, retry seam fail-closed,
idempotencja match, player-QUIC connection caps), więc ten plan to **wyłącznie
domknięcie ogona** — brak zmian architektury, brak nowych modułów, wszystko mieści
się w `core/*` + istniejących fortecach. Cel: proces gotowy do publicznego deployu
(Hetzner) bez znanych wektorów DoS i bez plan-vs-kod rozjazdów.

Research: 5 read-only Explore subagentów potwierdziło kształt każdego fixa przeciw
realnemu kodowi (w tym quinn 0.11.11 ze źródeł w cache). Każdy fix ma
zweryfikowaną lokalizację file:line i wzorzec do reuse. **Twarda decyzja bazowa
(CLAUDE.md):** żadnych migracji danych — zmiana SQL to idempotentny `CREATE OR
REPLACE` w DDL, nie backfill.

### Korekta względem audytu (uczciwe zawężenie)

- **Stary finding #5 (wewnętrzny edge „pinuje połączenie na zawsze") był za szeroki.**
  Domyślny `quinn::TransportConfig` ma już `max_idle_timeout = 30s`, więc *w pełni
  cichy* peer JEST reapowany. Realny residual bug jest węższy: klient wewnętrzny
  wysyła keepalive co 5s (`client.rs:51`), więc peer trzymający transport żywym, ale
  wiszący na poziomie aplikacji (`send.stopped().await` w `server.rs:348`), pinuje
  `InFlightGuard` i jeden z 100 slotów strumienia w nieskończoność — 30s idle nigdy
  nie odpali, bo keepalive go resetuje. Fix to **per-stream timeout**, nie idle
  timeout. Step 4 jest o tym.
- **Stary finding #2 (xmin pinning)** pozostaje realny, ale fix to DB-side belt
  (`idle_in_transaction_session_timeout` na prywatnych sesjach workera), nie
  przebudowa modelu — złożony w Step 2.

### Overlap / „why not extend X" (Research before planning — MANDATORY)

Wszystkie fixy rozszerzają istniejące seamy, żaden nie tworzy nowego:
- 503-under-shed dla kluczy API **reużywa wzorzec `verifier.rs::VerifyUnavailable`**
  (już podpięty do 503 na obu planach, `lib.rs:816`/`410`) — nie powstaje nowy typ
  transportu ani status.
- Typed unknown-method reużywa istniejące `opsapi::Status::NotFound` i `edge::Error`
  (adminrpc już zależy od `edge`+`opsapi`, Cargo.toml:23-24) — bez nowej zależności,
  bez tripu archcheck.
- Readyz-liveness rozszerza istniejący `Liveness` + readyz closure w `core/app`
  (ten sam wzorzec co `dead()`), nie dokłada planu ani gauge’a.
- Anti-spoof QUIC reużywa `Incoming::retry()`/`remote_address_validated()` z quinn —
  ścieżka anty-amplifikacji jest w API, nie piszemy własnej.
- Argon2 permit, event-plane fairness/stop — modyfikacja kodu w miejscu, ten sam
  `Semaphore`/`Step`/`Liveness`.

## Legenda dispatch tagów

Sesja = **Fable 5**. `[fable]` = subagent Fable 5 (correctness-critical: współbieżność,
seam, lifecycle). `[sonnet]` = subagent Sonnet 4.6 (mechaniczne: SQL one-liner,
skrypty, komentarze, testy z wzorca). Każdy code-writing Agent dostaje: explicit
`model:`, effort embedded, nav-guidance (gopls/targeted read; grep = lower bound),
trailer swojej linii, oraz **verbatim regułę „One test rollout at a time"**. Każdy
krok = osobna granica review; commit po każdym, diff sprawdzany przed następnym.

---

## Step 1 — Admin: permit Argon2 przeżywa anulowanie requestu `[fable]`

**(a) Co:** `modules/admin/src/lib.rs` (`authenticate_and_mint` + `login_submit`),
`modules/admin/src/tests.rs`.

**(b) Dlaczego teraz / kolejność:** najwyższy realny severity nowego kodu (MEDIUM,
RAM-DoS). Niezależny od reszty; idzie pierwszy jako najmniejsza korekta o najwyższym
wpływie na bezpieczeństwo.

**(c) Jak:** permit `argon_permits` (`Semaphore(2)`) jest dziś trzymany w frame’ie
handlera axum (`lib.rs:722`), a Argon2 liczy się w `spawn_blocking`
(`lib.rs:508-511`), którego drop JoinHandle **nie anuluje**. Klient rozłącza się →
future handlera dropnięty → permit zwolniony, a odłączone 64 MiB liczy dalej →
współbieżnych hashy rośnie ku sufitowi blocking-poola (512), nie 2. Fix pass-through
(5 edycji, potwierdzone przez agenta):
1. `lib.rs:73` — `use tokio::sync::{OwnedSemaphorePermit, Semaphore};`
2. `authenticate_and_mint` (~`:463-469`) — dodać param `argon: OwnedSemaphorePermit`.
3. `spawn_blocking` (`:508-511`) — **przenieść permit do closure**:
   `move || { let _permit = argon; verifier.verify(&hash, &candidate) }`.
4. `login_submit:722` — `_argon` → `argon` (zostawić else-arm 500).
5. `login_submit:728` — przekazać `argon` do wywołania.

Gotcha: permit MUSI wejść do `move` closure, nie do async-body — `let _argon =
argon;` na poziomie fn reintrodukuje bug. `OwnedSemaphorePermit` jest `Send`.
`login_slots`(32) i `IpLimiter` — **bez zmian** (to admission, nie kosztowna praca;
zwolnienie na cancel jest pożądane). **GC (`cleanup_login_attempts`, `:719-721`)
ZOSTAWIĆ za slotem** — reviewer słusznie: przeniesienie przed
`login_slots.try_acquire_owned()` (`:716`) zdejmuje jedyny bound współbieżności GC
(`login_limiter` jest per-IP, więc flood z wielu IP odpaliłby nieograniczone
równoległe DELETE). Trzymanie 1 z 32 slotów podczas 1/256 GC jest akceptowalne —
nie ruszamy. Docstring `render_login` (`:668`) — dopisać jedno zdanie o marginalnej
(sub-ms, nie-body) asymetrii locked-path (0 zapisów DB vs 1-2), skoro „no oracle"
docstring tego nie uznaje.

**Testy:** anulowanie mid-Argon2 (drop future) → permit zwolniony dopiero po
zakończeniu blocking-work (structural: licznik żywych permitów albo bariera na
verifierze); istniejący `verifier_runs_exactly_once_for_every_denial_shape` zostaje.

**Weryfikacja:** unit-only (split-proof nie ma instrumentacji RAM). `cargo test -p
admin`.

---

## Step 2 — Event plane: hardening dostawy i stopu `[fable]`

**(a) Co:** `core/asyncevents/src/{lib.rs,worker.rs}`, `core/app/src/lib.rs` (kolejność
start), `core/asyncevents/src/{worker_tests.rs,tests.rs}`.

**(b) Dlaczego teraz / kolejność:** najgęstszy klaster (6 pozycji), niezależna granica
review. Wewnątrz kroku kolejność: najpierw reorder startu (najbezpieczniejszy),
potem enum `Step::Poisoned` (poprzedza usunięcie martwego `Terminating`, żeby review
widział health-flow), potem DB-side belty.

**(c) Jak — 6 pozycji:**

1. **Reorder start (stary #6):** w `core/app/src/lib.rs` w fallible bloku `run()`
   zamienić kolejność: dziś `app.start()` (`:564`) → `plane.start()` (`:566-568`) →
   `invalidation.start()` (`:572-574`). Przenieść blok invalidation **nad** blok
   plane. Potwierdzone: `invalidation.start()` potrzebuje tylko `app.start()`
   (snapshot modułów), `plane.start()` nie zależy od invalidation. **`ordered_teardown`
   (`:799-824`) NIE lustrzeć** — stop celowo zatrzymuje plane PRZED invalidation
   (delivery halts before modules tear down, CLAUDE.md #8).

2. **Readyz przy trwale failującym workerze (stary #7):** `Liveness` (`lib.rs:94-104`)
   ma tylko `dead`/`stopping`. Dodać `AtomicU64` „last successful pass epoch" (sekundy
   monotoniczne); **zaseedować w `Plane::start`** (inaczej 0 → wiek „nieskończony" →
   readyz flapuje na not-ready tuż po starcie, bo HTTP serwuje zaraz po
   `invalidation.start`, a pierwszy pass może zalegać na zimnym DB); bumpować w
   `worker::run` po każdym udanym `drain_pass_on` (~`:443`); readyz closure
   (`core/app:503-506`) flip na `Err` gdy `wiek > 30s` (obok `dead()`) — 30s to ~30×
   floor pollingu 1s + margines na wolny pass, nie flapuje przy chwilowym Err.
   Reconnect-loop (Err→reconnect) nie dotyka dziś `Liveness` — to jedyny sygnał
   wewnątrz pętli.

3. **`Step::Poisoned` (nowy #3):** timeout handlera zabija własny trwały backend
   workera (`worker.rs:293-297`), ale zwraca `Faulted`→`healthy=true`, przez co
   następny op gwarantowanie failuje + spurious ERROR. Dodać wariant `Step::Poisoned`;
   arm timeoutu (`:280-311`) zwraca go zamiast `Faulted`; `drain_pass_on` (`:466`)
   mapuje na `(delivered, false)` → natychmiastowy reconnect; `drain_pass` (fresh-conn,
   `:395`) mapuje na `break`. **Nie** traktować całego `Faulted` jako unhealthy — arm
   błędu handlera (`Ok(Err)`, `:257-279`) legalnie utrzymuje zdrowe połączenie.

4. **Usunąć martwy `DeliveryState::Terminating` (nowy #2):** enum (`worker.rs:35-38`),
   pole `state` (`:45`), mutację w `claim_active` (zwracać wszystkie wpisy), oraz
   `_still_active` w `Plane::stop` (`lib.rs:280-282`). `terminate_claim` już fenced na
   `pid`+`backend_start`, `claim_active` woła się raz (`lib.rs:261`, poza pętlą) — stan
   `Terminating` strzeże double-claima, którego nie ma. Realnym guardem przed stale-
   removal jest sprawdzenie generacji w `ActiveGuard::drop` (`worker.rs:83-90`), które
   zostaje; regresja `stale_backend_identity_cannot_terminate_live_reused_pid` to
   właściwy test. Wariant (b) = delete (mniej kodu, ta sama gwarancja).

5. **`idle_in_transaction_session_timeout` (stary #2, xmin) — uczciwy zakres.** UWAGA:
   ustawienie tego na *własnych* sesjach workera **NIE** naprawia oryginalnego findingu
   (rogue/idle-in-tx sesja *gdziekolwiek* w klastrze przypina xmin) — to jest belt
   przeciw workerowi wyciekającemu WŁASNĄ tx (możliwe po refaktorze na direct-
   connection w af26dc5), nie przeciw obcej sesji. Wprowadzić helper `connect(dsn)` =
   `PgConnection::connect` + `SET idle_in_transaction_session_timeout = '<handler_timeout
   + margines, np. handler_timeout*2>'` (NIE literał 15s — `ASYNCEVENTS_HANDLER_TIMEOUT`
   jest env-konfigurowalny `worker.rs:111-120`; literał zabiłby legalny wolny handler
   przy większym timeoucie), przepuścić delivery sites (`worker.rs:161`, `:417`).
   **Co to łapie / czego nie:** `idle_in_transaction` odpala tylko gdy tx jest IDLE
   *między* statementami (dropnięty future) — NIE gdy backend jest wklinowany *wewnątrz*
   statementu (to stan `active`; ten przypadek pokrywa już arm timeoutu +
   `pg_terminate_backend`, a komplementarnym beltem byłby `statement_timeout`).
   **Oryginalny xmin-anywhere to koncern ops** (globalny `idle_in_transaction_session_
   timeout` w `postgresql.conf`/`ALTER SYSTEM` + istniejący gauge
   `asyncevents_safe_frontier_age_seconds` jako alert) — udokumentować w
   `docs/reference/`, nie udawać że kod core to egzekwuje.

6. **Drobne:** (i) zapytanie tożsamości `pg_stat_activity` (`worker.rs:430-440`) biegnie
   co pass mimo że pid/backend_start stałe per połączenie — scachować w bloku
   `if conn.is_none()` (`:416-428`), trzymać obok conn. (ii) `testing::deliver_all`
   (`lib.rs:381`) bierze DSN z env, nie z poola `TestTransport` — dodać `dsn` do
   `TestTransport`, żeby sesje workera trafiały w ten sam DB co `reconcile`.

**Testy:** startup-order (delivery nie rusza przed pierwszym refreshem invalidation —
observ. przez callback licznik); readyz flip po N failach; `Poisoned` → natychmiast
reconnect bez spurious-error (rozszerzyć istniejący timeout test); regresja że delete
`Terminating` nie łamie `stale_backend_identity_cannot_terminate_live_reused_pid`;
idle-in-tx timeout ustawiony (sprawdzić `current_setting` na sesji workera).

**Weryfikacja:** `cargo test -p asyncevents -p app` + `topiccheck --durability-strict`
(bez zmian topiców, ale fairness/stop dotyka worker). Split assertion patrz Step 9.

---

## Step 3 — Player-QUIC: admission dopiero po walidacji adresu (anti-spoof) `[fable]`

**(a) Co:** `core/edge/src/player.rs`, `core/edge/src/player_tests.rs`.

**(b) Dlaczego teraz / kolejność:** stary #4, realny wektor DoS przy publicznym
deployu (spoof source IP → wyczerpanie globalnego/per-IP budżetu slotów). Niezależny.

**(c) Jak:** dziś accept-loop czyta `incoming.remote_address().ip()` (`:234`) i woła
`try_admit(ip)` (`:235`) **przed** `incoming.await` (`:251`) — spoof rezerwuje i trzyma
slot przez nigdy niekończący się handshake. quinn 0.11.11 (potwierdzone w źródłach)
eksponuje `Incoming::remote_address_validated()` i `Incoming::retry()`. Przed `:234`:
```rust
if !incoming.remote_address_validated() {
    let _ = incoming.retry();   // may_retry() gwarantowane true; nie unwrap
    continue;                    // żaden slot nie zarezerwowany
}
```
Off-path spoofer nigdy nie dostaje pakietu Retry → nigdy nie produkuje drugiego
(zwalidowanego) `Incoming` → `try_admit` nieosiągalny dla spoofa. Slot rezerwowany
dopiero przy zwalidowanym powrocie. Wariant (b) (przenieść `try_admit` za
`incoming.await`) jest słabszy — plan gracza jest server-cert-only, więc realny peer
i tak tanio kończy handshake i wyczerpuje sloty; (a) to anty-amplifikacja, którą sam
komentarz autora próbował opisać. **Poprawić komentarze** `:65-66`, `:196-200`, `:233`
(twierdzą „costs nothing an attacker can inflate" — fałsz dla spoofa).

Gotcha: `retry()` dokłada 1 RTT do *pierwszego* diala gracza — akceptowalne w modelu
połączenia trwałego (raz na dial). Retry jest bezstanowy (token, `retry_token_lifetime`
domyślnie 15s). Plan wewnętrzny (`server.rs`) NIE ma `ConnLimiter` (peery mTLS) — fix
jest player-only.

**Testy:** niezwalidowany Incoming → `retry()` wołany, slot nie zajęty; zwalidowany →
admission normalne. (Unit w `player_tests.rs` z fake/loopback; live w Step 9.)

**Weryfikacja:** `cargo test -p edge`. Split assertion Step 9.

---

## Step 4 — Wewnętrzny edge: per-stream timeout + jawny TransportConfig `[fable]`

**(a) Co:** `core/edge/src/server.rs` (`listen` + `serve_stream`), `core/edge/src/lib.rs`
testy edge.

**(b) Dlaczego teraz / kolejność:** stary #5 (zawężony). Niezależny; po Step 3, bo oba
dotykają edge i chcemy osobne granice review.

**(c) Jak — dwie części:**
1. **Per-stream timeout (realny fix) — OBIE połowy strumienia.** `serve_stream` ma DWA
   nieograniczone `await` które peer z żywym keepalive (klient co 5s, `client.rs:51`)
   pinuje bez odpalenia 30s idle: (i) `read_frame(&mut recv).await` na WEJŚCIU
   (`server.rs:332`) — peer otwiera bidi stream i nigdy nie dosyła pełnej ramki; (ii)
   `send.stopped().await` na WYJŚCIU (`:348`) — peer nigdy nie drenuje odpowiedzi. Oba
   trzymają `InFlightGuard` + slot strumienia. Owinąć **każdy z dwóch waitów osobno** w
   `tokio::time::timeout(EDGE_STREAM_GRACE, …)` (np. 30s); środek — sam dispatch
   handlera — może legalnie trwać, więc go NIE obejmować jednym kopertowym timeoutem.
   Po przekroczeniu zamknąć stream i puścić guard. (Reviewer: pierwotna teza „to jedyna
   rzecz" była fałszywa — read half to ta sama patologia.) Wartość vs `EDGE_DRAIN_GRACE`
   (5s, tylko shutdown): ten timeout działa w steady-state, drain grace tylko przy stopie.
2. **Jawny `TransportConfig` (audytowalność, nie ratunek z nieskończoności):**
   `listen` (`server.rs:85-91`) buduje `ServerConfig::with_crypto` bez
   `.transport_config(...)`. Domyślny ma już `max_idle_timeout=30s`,
   `max_concurrent_bidi/uni=100` — więc to pinowanie *fully-silent* jest już OK.
   Dołożyć jawny `TransportConfig` (szablon z player.rs:207-213: `max_idle_timeout`
   30s, `max_concurrent_bidi_streams` 16, opcjonalnie uni cap) żeby pinować bound
   przeciw przyszłej zmianie defaultu quinn i zejść ze 100 slotów. **Idle 30s > 5s
   keepalive** (6 interwałów/okno) — keepalive realnie trzyma conn.

**Testy:** stream wiszący na `send.stopped()` z żywym keepalive → reap po grace
(fake/loopback + kontrola czasu); TransportConfig ustawiony (idle/stream caps).

**Weryfikacja:** `cargo test -p edge`. Split assertion (idle/stream reap cross-process)
Step 9.

---

## Step 5 — Gateway: key-verifier zwraca 503 przy load-shed, nie 401 `[fable]`

**(a) Co:** `modules/gateway/src/keys.rs`, `modules/gateway/src/tests.rs`.

**(b) Dlaczego teraz / kolejność:** nowy #5 (MEDIUM). Niezależny; mirroruje istniejący
`VerifyUnavailable`, więc czysta granica.

**(c) Jak:** trait `KeyVerifier::lookup -> Option<KeyRecord>` (`keys.rs:61-63`)
strukturalnie nie odróżnia „nieznany klucz" od „przeciążenie" — `check_api_key`
(`:109-111`) kolapsuje każde `None` do 401. Ważny-ale-niecache’owany klucz przy
distinct-key spamie dostaje „invalid". Mirror `verifier.rs::VerifyUnavailable`
(już podpięty do 503 na HTTP `lib.rs:816` i player `:410`):
1. `keys.rs:61-63`: `lookup -> Result<Option<KeyRecord>, LookupUnavailable>` + `pub
   struct LookupUnavailable;`.
2. `KeyDenial` (`:68-75`) dodać `Unavailable`; `message()` (`:79`) + `status()` (`:90`,
   → `Status::Unavailable`).
3. `check_api_key` (`:109-111`): `Ok(Some)→r`, `Ok(None)→Invalid`, `Err→Unavailable`.
4. Impl: `RealKeyVerifier` — cache hits→`Ok`; **flight-saturation (`:255`) i semafor
   shed (`:260`) → `Err(LookupUnavailable)`**; store-`Err` (`:268`) → `Err` (spójne z
   docstring `:9-14` „blip nie może zatruć ważnego klucza jako 401"). `KEY_MAX_BYTES`
   guard (`:249`) → `Ok(None)` (nadmierny klucz to definitywnie nie-klucz, nie outage).
   `AllowAllKeyVerifier` (`:149`) → `Ok(Some)`. `FakeKeyVerifier` (`tests.rs:25`).
5. **Zaktualizować docstring modułu `keys.rs:12-13`** — dziś mówi „per-request `Err →
   deny` collapse still applies"; po fixie `Err → 503`, nie deny-as-401. Bez tego doc
   przeczy kodowi.
6. Testy (`tests.rs` ~10 asercji `.lookup().await.{unwrap,is_none}`) → double-unwrap.
   **Sprawdzić że `[K1]-[K4]` split-asercje nadal przechodzą** — 401 dla brak/zły klucz,
   403 client-key na match.report, 202 server-key: żaden z nich nie przechodzi ścieżką
   shed/store-Err, więc semantyka 401/403 pozostaje; zmiana dotyka tylko ścieżki
   przeciążenia (nowy 503).

Gotcha: `Err(Unavailable)` zostaje **uncached** (już jest — cache tylko na `Ok`).
Oba `check_api_key` callery (`lib.rs:396`/`749`) bez zmian — już wołają
`denial.status()`/`.message()`, więc 503 płynie po dodaniu wariantu. **Public-api:**
`gateway` to moduł, nie contract-crate — bez wpływu na baseline; `apikeysapi::Keys`
niezmieniony.

**Weryfikacja:** `cargo test -p gateway`. Split assertion (503-under-shed cross-process)
Step 9.

---

## Step 6 — Config: NOTIFY payload bez `value` (usuń latentny abort >8 KB) `[sonnet]`

**(a) Co:** `modules/config/src/lib.rs` (funkcja triggera w `SCHEMA_DDL`),
`modules/config/src/tests.rs`.

**(b) Dlaczego teraz / kolejność:** stary #8 (MINOR). Mechaniczny, niezależny; tag
sonnet — jedna linia SQL + test.

**(c) Jak:** trigger buduje jeden `_payload` z `'value', _value` (`:98-104`) i używa go
i do `pg_notify` (`:107`, **zabójca >8000 bajtów — abortuje tx zapisu**) i do durable
`append_event` (`:111`). Callback invalidation (`:691-694`) to closure zero-arg → re-
czyta cały snapshot, **nigdy nie czyta payloadu NOTIFY**. Durable payload to
opublikowany kontrakt (`configevents::CHANGED`, public-api+topiccheck) — **zostaje
nietknięty**. Zmienić tylko `:107` na value-less inline:
```sql
PERFORM pg_notify('config_changed', jsonb_build_object(
    'namespace', _ns, 'key', _key, 'operation', _op, 'revision', _rev)::text);
```
`:98-104` i `:111` bez zmian. To realignuje kod do **już napisanego** docstringu
(`:54-56` opisuje NOTIFY jako value-less). `CREATE OR REPLACE` w DDL, idempotentny
`migrate()` (`:649`) — bez migracji danych.

**Testy:** zapis config z dużą wartością (~>8 KB) nie abortuje; revizja/refresh nadal
działają; durable `config.changed` nadal ma `value`.

**Weryfikacja:** `cargo test -p config`. Split assertion (large-value config reload)
Step 9. **`topiccheck`** — potwierdzić brak zmiany kształtu `config.changed`.

---

## Step 7 — Admin-rpc: typed unknown-method zamiast `contains("unknown method")` `[fable]`

**(a) Co:** `core/edge/src/{lib.rs,server.rs,client.rs,player.rs}`,
`api/admin/rpc/src/lib.rs`, testy edge.

**(b) Dlaczego teraz / kolejność:** stary #9 (MINOR, ale realny footgun — reword
komunikatu edge wypełnia cały sidebar `/admin` kartami „unavailable"). Po Step 4, bo
oba dotykają `core/edge`.

**(c) Jak (Option B — minimalny typed fix, potwierdzony):** dziś adminrpc
(`api/admin/rpc/src/lib.rs:78-80`) robi `e.to_string().contains("unknown method")`;
string powstaje w `server.rs:280`; wire (`wire.rs:29-36`) niesie tylko `error:
Option<String>`; `From<edge::Error> for opsapi::Error` (`lib.rs:74-78`) kolapsuje
wszystko do `Status::Unavailable`.
1. Dodać `edge::Error::UnknownMethod(String)` (`core/edge/src/lib.rs:42-67`).
2. W `client.rs:101` (**tylko** — NIE `player.rs`) wykryć sentinel przy dekodowaniu
   `ok:false` → `UnknownMethod` zamiast `Remote`. **String-check zostaje wewnątrz
   edge**, obok producenta, przez wspólny `const UNKNOWN_METHOD_PREFIX` dzielony przez
   `server.rs:280` i `client.rs` (producent/detektor nie mogą się rozjechać). Reviewer:
   plan gracza (`player.rs:504-518 dispatch`) NIE ma tablicy metod — nigdy nie produkuje
   sentinela `edge: unknown method`; detekcja tam dałaby tylko false-positive z
   relayowanych stringów frontu. Edycja player = wyłącznie internal-client.
3. `From<edge::Error>` (`lib.rs:74-78`): `UnknownMethod → opsapi::Error::not_found(…)`,
   reszta → `unavailable(…)`. Zaktualizować docstring `:70-73` (już nie „każdy błąd =
   Unavailable").
4. adminrpc (`:78-80`): zamiast string-match → `e.status == opsapi::Status::NotFound`
   (`e` to już `opsapi::Error`, `use opsapi::Error` jest). Bez nowej zależności
   (adminrpc już zależy od `edge`+`opsapi`), bez tripu archcheck.
5. Test `core/edge/src/lib.rs:166` (`Error::Remote(msg) if contains…`) → `UnknownMethod(_)`.

**BLAST RADIUS (reviewer, świadoma decyzja):** `From<edge::Error> for opsapi::Error`
(`lib.rs:74-78`) to JEDYNA konwersja używana przez KAŻDY generowany rpc-client i
gateway Remote dispatch — nie jest adminrpc-local. Więc `UnknownMethod → not_found`
zmienia mismatch metody gateway→svc (version skew, misdeploy) z **503 na 404 na
froncie, nieodróżnialne od domenowego not-found**. To jest celowy wybór (unknown-method
nie jest retryowalne), ale MUSI być nazwany: dodać test gateway-level potwierdzający
status i **decyzja do zatwierdzenia** — czy transport-level 404 aliasowany z domenowym
404 jest OK. Alternatywa gdyby nie: użyć dedykowanego `Status` (np. zostawić Unavailable
dla nie-admin konsumentów, a rozróżnienie zrobić tylko w adminrpc przez nowy wariant) —
ale to większy diff. Retry-impact minimalny (RPC default `RetryMode::Never`). Opcja C
(structured `code` w `wire.rs Response`) = pełna eliminacja stringów, większy blast
radius (obie planes) — **odrzucona jako over-engineering** dla MINOR.

**Testy:** unknown-method → `opsapi::Status::NotFound` → adminrpc drop (Absent), realny
peer-down → Unavailable → error card.

**Weryfikacja:** `cargo test -p edge -p admin` + istniejące admin fan-out testy.

---

## Step 8 — Proofy shutdown + drobne skryptowe/dokumentacyjne `[sonnet]`

**(a) Co:** `split-proof.ps1` (`Stop-Svc`), `split-proof.sh` (`stop_pid`),
`modules/gateway/src/keys.rs` (komentarz), `core/app/src/lib.rs` (jeśli trzeba doc).

**(b) Dlaczego teraz / kolejność:** nowy #4 + polish. Po krokach kodowych, bo asercje
muszą pasować do finalnego zachowania. Mechaniczne → sonnet.

**(c) Jak:**
1. **ExitCode w graceful proof (nowy #4):** `Stop-Svc` (`split-proof.ps1:238-249`) robi
   tylko `winctrl break` + `WaitForExit(10000)`, nigdy nie sprawdza `$Proc.ExitCode`
   ani nie liczy martwego-już procesu jako fail. Dodać:
   ```powershell
   if ($breakSent -and $Proc.WaitForExit(10000)) {
       if ($Proc.ExitCode -eq 0) { Note "gracefully stopped $Label"; return $true }
       Note "$Label drained but exited $($Proc.ExitCode)"; return $false
   }
   ```
   Branch already-exited (`:239`) → `$false` przy non-zero ExitCode. **.sh symetria:**
   `stop_pid` (`:217-231`) po wykryciu wyjścia: `wait "$pid"; ec=$?`, nowy flag
   `STOP_NONZERO` gdy `ec != 0`, symetryczna asercja `[W… clean-exit]`. In-flight-probe
   **odroczony** (brak endpointu z opóźnieniem we flocie — racy, low-value; udokumentować
   jako świadome odroczenie).
2. **Komentarz `keys.rs:36-38`** — mówi „CLEARED / O(1)", a `insert` (`:205-219`) robi
   teraz `retain(expired)` + `min_by_key` oldest (O(n)). Zaktualizować.

**Weryfikacja:** `./split-proof.ps1` (Windows) sam siebie testuje; sh na parytet nazw.

---

## Step 9a — Nowe named split-proof assertions (tylko realnie asertowalne) `[fable]`

**(a) Co:** `split-proof.sh` + `split-proof.ps1`, `tools/playercli` (jeśli trzeba tryb
burst).

**(b) Dlaczego teraz / kolejność:** asercje nie mogą wyprzedzić implementacji. `[fable]`
— projekt cross-process asercji jest subtelny (łatwo napisać asercję happy-path pod
etykietą bezpieczeństwa).

**(c) Jak — 2 asercje realnie obserwowalne w skrypcie + 2 zdegradowane do unit
(reviewer: skrypt NIE zaspoofuje source-IP off-path ani nie ma endpointu fault-
injection do zawieszenia strumienia):**
- **[ASERTOWALNE] Key-verifier 503-under-shed** (Step 5) — rozszerzyć `[K1]-[K4]`
  (`:644-682`): N równoległych curl z RÓŻNYMI kluczami przeciw żywemu apikeys-svc,
  obserwować 503 (nie 401) gdy semafor (64) się nasyci. Observable: kod HTTP 503.
- **[ASERTOWALNE] Config pg_notify large-value** (Step 6) — rozszerzyć `[C1]-[C3]`
  (`:776-836`): zapis wartości ~>8 KB przez `pg()`/admin, asercja że zapis NIE abortuje
  (revizja rośnie) i refresh przechodzi. Observable: wiersz w DB + revizja + reload.
- **[ZDEGRADOWANE do unit + udokumentowany deferral] Player-QUIC anti-spoof** (Step 3)
  — skrypt nie zaspoofuje UDP; jedyne co `playercli` pokaże to happy-path (zwalidowany
  dial nadal admittuje), co NIE dowodzi gałęzi spoof. Pokrycie: unit w `player_tests.rs`
  (niezwalidowany Incoming → `retry()`, slot nie zajęty). Player **rate-limit** (nie
  spoof) jest osobno asertowalny burstem przez `playercli` jeśli af26dc5 tego jeszcze
  nie ma — sprawdzić i ewentualnie dodać `[Rn]`. Deferral spoofa nazwać w skrypcie.
- **[ZDEGRADOWANE do unit + deferral] Internal-edge stream reap** (Step 4) — brak
  endpointu fault-injection wiszącego na `send.stopped()` we flocie (ten sam brak, przez
  który Step 8 odracza in-flight-probe — spójnie). Pokrycie: unit w edge (loopback/fake
  + kontrola czasu). Udokumentować deferral cross-process.

Helpery (agent potwierdził — **nie ma** `http()` wrappera): `pg()` (`:203-211`),
inline-curl `curl -s -w '\n%{http_code}'`, `wait_healthy()` (`:270-281`),
`new_uuid()` (`:174`). `fleet_preflight` (`:288-310`) bez zmian (brak nowego svc).

## Step 9b — Verify, docs, trailer, gates `[sonnet]`

**(a) Co:** symetria nazw `.sh`/`.ps1`, `CLAUDE.md`/`AGENTS.md` (jeśli fix zmienia
opisane zachowanie — np. player admission przez Retry, key-verifier 503, unknown-method
→ 404, idle-in-tx knob), `docs/reference/` (ops-note o globalnym idle-in-tx z Step 2.5),
finalny verify.

**(b) Dlaczego teraz / kolejność:** po 9a; czysto mechaniczne/dokumentacyjne.

**(c) Jak:** **Public-api:** żaden fix nie zmienia contract-crate (`apikeysapi::Keys`
bez zmian, `core/edge`/`gateway`/`modules/admin` impl poza baseline) → **bless
niepotrzebny**; potwierdzić że `public-api` advisory jest zielony. **archcheck/
topiccheck:** zero tripów (potwierdzone) — defensywnie sprawdzić rule 17 (gateway stub
coverage) po Step 5/7 i `topiccheck` po Step 6.

**Trailer (proceduralny finding):** `af26dc5` (overlapujący z tym planem) **nie ma
`Co-Authored-By`** mimo że to code-fix commit — CLAUDE.md „Commit Message Format —
MANDATORY" tego wymaga. Każdy subagent implementacyjny dostaje w prompt swój trailer
(`[fable]`→Claude Fable 5, `[sonnet]`→Claude Sonnet 4.6), a **po rolloucie uruchomić
audyt** `git log -N --format="%h %B" | grep "Co-Authored"` i potwierdzić zgodność linii
— przy `af26dc5` ten check ewidentnie pominięto.

**Finalna weryfikacja (verify-only, per „One test rollout at a time"):** przed KAŻDYM
runem `Get-Process | ? { $_.ProcessName -match '^cargo$|^rustc$' }` i czekać. Kolejno:
focused non-DB testy → focused DB-package testy → `cargo run -p archcheck` →
`cargo run -p topiccheck -- --durability-strict` → `cargo run -p requirecheck --
--strict` → na końcu **jedno** `./verify.ps1 -All -Strict` (nie odpalać osobnego
split-proof podczas verify).

---

## Proponowane commity (granice review)

Po każdym kroku osobny commit (Conventional Commits, scope = moduł), diff sprawdzany
przed następnym:
`fix(admin)` · `fix(asyncevents,app)` · `fix(edge)` [Step 3] · `fix(edge)` [Step 4] ·
`fix(gateway)` · `fix(config)` · `refactor(edge,admin)` [Step 7] ·
`test(split-proof),chore(gateway)` [Step 8] · `test(split-proof)` [Step 9a] ·
`docs,test(verify)` [Step 9b].

## Podsumowanie zakresu

| Step | Findings | Severity | Tag |
|------|----------|----------|-----|
| 1 | Argon2 permit + GC reorder + doc | MEDIUM | fable |
| 2 | plane-order, readyz, Poisoned, del Terminating, idle-in-tx, 2 nity | MAJOR×2 + reszta | fable |
| 3 | player-QUIC anti-spoof admission | MAJOR (DoS) | fable |
| 4 | internal edge per-stream timeout + TransportConfig | MAJOR (zawężony) | fable |
| 5 | key-verifier 503-under-shed | MEDIUM | fable |
| 6 | config NOTIFY value drop | MINOR | sonnet |
| 7 | typed unknown-method | MINOR | fable |
| 8 | shutdown proof ExitCode + komentarz | LOW | sonnet |
| 9a | split assertions (2 asertowalne, 2 → unit+deferral) | proces | fable |
| 9b | verify + docs + ops-note + trailer audyt | proces | sonnet |
