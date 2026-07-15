# B1: stub module→module re-dial po restarcie peera — plan

Cel: zamknąć bug B1 z [2026-07-15-1120-weles-m0-pre-m1-backlog-status.md](../status/2026-07-15-1120-weles-m0-pre-m1-backlog-status.md)
(punkt 1 backlogu przed-M1). Po restarcie characters-svc na tym samym porcie
sync-owy call inventory→characters (`Ownership::owner_of`) po wewnętrznym QUIC
edge pada trwale (obserwowane jako permanentne 404 na `GET /inventory/{cid}`),
podczas gdy ścieżka gateway→svc się odzyskuje.

## Context — synteza researchu (3 subagenty, 2026-07-15)

Metody: 3 równoległe subagenty read-only (edge client lifecycle / stub wiring /
splitproof proof surface), każdy z targeted-rg-anchor → end-to-end read; synteza
+ inline weryfikacja kluczowych plików w głównym modelu. Grep-only nigdzie nie
był jedyną metodą.

### Jak trzyma połączenie strona konsumencka (ścieżka, która pada)

- `cmd/inventory-svc/src/lib.rs:17-21` tworzy `remote::Stub::new("characters",
  addr, charactersrpc::remote_factories())`.
- `Stub` posiada **jedno** połączenie na cały proces:
  `conn: Arc<Reconnecting<EdgeDialer>>` (`core/remote/src/lib.rs:448`, konstrukcja
  `:475-477`). `Stub::register` (`:564-591`) wręcza TEN SAM `Arc<dyn Caller>`
  każdej fabryce z `<name>rpc`; wygenerowany `Client` (rpc-macro
  `tools/rpc-macro/src/lib.rs:420-429`) tylko go przechowuje.
- Cache połączenia: `Reconnecting.cur: Mutex<Option<Arc<dyn Conn>>>`
  (`core/remote/src/lib.rs:175-179`); `get()` (`:190-198`) zwraca cache bez
  żadnej sondy zdrowia, dial tylko przy `None`.
- **Gate resetu — autorytet decyzji** (`core/remote/src/lib.rs:231-264`):
  reset + ewentualny redial/replay dzieje się WYŁĄCZNIE przy
  `FailureProvenance::ConnectionFatal`. Klasyfikacja w `map_edge_call_failure`
  (`:293-305`): `edge::Error::Connection` → ConnectionFatal;
  `Remote|UnknownMethod` → PeerAnswer; **wszystko inne (w tym `Stream`, `Io`,
  `Codec`, `Connect`) → StreamLocal — i StreamLocal NIGDY nie resetuje**.
  Martwe połączenie sklasyfikowane stream-owo zostaje przypięte w `cur` na
  zawsze. Testy kodujące obecną (błędną) intencję:
  `core/remote/src/tests.rs:213` (`stream_local_failures_do_not_reset_or_replay`)
  + tabela provenance `tests.rs:7-43`.

### Dlaczego gateway się odzyskuje (wzorzec, nie kod współdzielony)

Gateway NIE używa `Reconnecting` — trzyma własny cache surowych `edge::Client`
w `RouteTable.remotes` (`modules/gateway/src/lib.rs:629`, dial `:823-851`)
i **evictuje po każdym błędzie, który nie jest definitywną odpowiedzią**:
`dispatch` (`:784-800`) → `!e.status.is_definitive_answer()`, gdzie definitywne
jest tylko `Status::NotFound` (`core/opsapi/src/lib.rs:189-192`). Kolejny
request re-dialuje świeżego klienta. To jest asymetria z B1: dwa niezależne
mechanizmy samonaprawy o różnych partycjach błędów.

### Ścieżka „cichego 404" — co wiemy, czego nie wiemy (UWAGA: to jest headline)

- `modules/inventory/src/service.rs:61-77`: transportowy `Err(_)` z `owner_of`
  → **503** Unavailable; `Ok(None)` → **404**. Wygenerowany klient propaguje
  błędy `Caller::call` przez `?` (rpc-macro `:562-575`), a `null` w kopercie
  jest wiernie zachowywane jako `None` (rpc-macro `:501-515`). **Wniosek z
  kodu: przypięte MARTWE połączenie nie umie wyprodukować 404 — tylko 503.
  Obserwowane trwałe 404 wymaga UDANEGO RPC, w którym characters-svc
  odpowiedział `Ok(None)`** („nie znam takiej postaci") — co wskazywałoby na
  warstwę danych/środowiska (np. env respawnu w chaosie Welesa), nie transport.
- Klient edge ma keepalive 5s / idle 30s (`core/edge/src/client.rs:20,27`),
  bind na porcie efemerycznym (`tls.rs:384-390` — brak kolizji przy redialu),
  dial z deadlinem 5s. Nawet po błędzie sklasyfikowanym StreamLocal następny
  call na martwym połączeniu dostaje `open_bi` → `ConnectionLost` →
  ConnectionFatal → reset — więc czysty StreamLocal-pinning jest co najwyżej
  PRZEJŚCIOWY, nie permanentny. **Dokładny mechanizm trwałej awarii nie jest
  ustalony z kodu — dlatego Step 1 to test repro przypinający failing branch,
  a plan ma jawną tabelę decyzyjną (niżej); gałęzie (C)/(D) są na dziś
  najbardziej prawdopodobnym wynikiem.** Gate w `Reconnecting` pozostaje
  realną, udokumentowaną poniżej luką semantyczną względem gatewaya — ale plan
  NIE twierdzi, że to on sam wyjaśnia obserwację z chaosu.

### Rodzeństwo tej samej klasy (sweep — rule 6)

Ten sam `Reconnecting` obsługuje: admin remote fan-out
(`api/admin/rpc/src/lib.rs:72-80`) i zdalny `CachedConfig` (configrpc przez
`Stub`). Fix w gate naprawia je automatycznie. Gateway (`RouteTable`) już jest
fail-safe — bez zmian. Innych cache'y połączeń nie znaleziono.

### Dlaczego nie „wyciągnąć wspólnego mechanizmu z gatewayem"

Gateway evictuje po ZMAPOWANYM `opsapi::Status`; `Reconnecting` klasyfikuje
na surowym `edge::Error` ZANIM mapowanie zatrze rozróżnienie (komentarz w
`core/remote/src/lib.rs:315-317`) — to precyzyjniejszy autorytet (odróżnia
PeerAnswer-Remote od martwego transportu, czego status po mapowaniu nie umie).
Ujednolicamy więc SEMANTYKĘ (fail-safe: resetuj wszystko poza odpowiedzią
peera), nie kod. Konsolidacja obu mechanizmów to osobna, większa refaktoryzacja
— poza minimalnym domknięciem B1 (rule 3).

## Tabela decyzyjna (gałęzie w pełni wyspecyfikowane, wybór po Step 1)

| Wynik repro (Step 1) | Autorytet fixu |
|---|---|
| (A) Po restarcie peera calle padają trwale z klasyfikacją StreamLocal (np. `Error::Stream` na stale conn) | Gate `Reconnecting::call` — Step 2 jak napisany. |
| (B) Padają trwale na redialu (`Error::Connect`/`Tls` z `EdgeDialer`) | Autorytet w `EdgeDialer::dial` / `edge::Client::dial_with_config` (`core/remote/src/lib.rs:274-291`, `core/edge/src/client.rs:58-90`) — fix tam. Step 2 (gate) wchodzi WYŁĄCZNIE jako świadome wyrównanie semantyki do gatewaya (evict-on-non-answer), z deklarowanym domknięciem „parity", NIE „fix B1" — dowodem są deterministyczne unit testy na fake-transporcie, nie split. |
| (C) Warstwa połączenia SAMA się odzyskuje (test zielony na obecnym kodzie) | Pinning — jeśli istnieje — żyje wyżej. STOP implementacji, raport do Lukasza z diagnozą i opcjami (doktryna „report, don't fix") — bez zgadywania kolejnej warstwy. |
| (D) Repro zielone ORAZ dowody wskazują na zdekodowane `Ok(None)` (patrz headline wyżej: 404 nie może pochodzić z martwego transportu) | To nie jest bug transportu: diagnoza środowiska chaosu Welesa / ścieżki danych characters (env respawnu, DB, timing create-vs-restart). STOP + raport, jak (C). |

## Kroki

### Step 1 — repro na warstwie połączenia (test przypinający failing branch)
- **(a) Co:** nowy plik testowy `core/remote/src/redial_tests.rs` (deklaracja
  `#[cfg(test)] #[path = "redial_tests.rs"] mod redial_tests;` w
  `core/remote/src/lib.rs`, konwencja testów-w-osobnych-plikach). Test
  integracyjny na realnym edge: boot `edge::Server` z jedną echo-metodą na
  porcie loopback P (wzorzec z istniejących testów `core/edge`), call przez
  `Reconnecting<EdgeDialer>` (typy prywatne — test w tym samym crate) →
  sukces; zamknięcie serwera A (drop endpointu); boot serwera B na TYM SAMYM
  porcie P; pętla calli (`RetryMode::Never`) z krótkim sleepem, bounded
  deadline z zapasem ≥ 2× `CLIENT_IDLE_TIMEOUT_MS` (60s hang-guard);
  asercja: call ostatecznie przechodzi. Drugi wariant testu: metoda
  `RetryMode::OnceAfterReconnect`. Komunikat faila wypisuje sekwencję
  zaobserwowanych wariantów `edge::Error`/provenance (diagnoza wbudowana w
  test, nie ad-hoc printfy).
- **(b) Dlaczego teraz:** wynik wybiera gałąź tabeli decyzyjnej; bez tego fix
  byłby zgadywanką (dokładnie klasa błędu „patch symptomu przy złym
  autorytecie").
- **(c) Jak:** doktryna timing-sensitive: `--test-threads` działa per binarka,
  nie per plik — więc serializacja przez statyczny mutex serialny wokół obu
  wariantów (wzorzec: jeden `static REDIAL_SERIAL: Mutex<()>`), asercja
  „eventually recovers w bounded deadline", nigdy asercje latencji; real
  sockets ⇒ realny zegar z hang-guardem 90s (3× `CLIENT_IDLE_TIMEOUT_MS` —
  2× jest za cienkie pod pełną równoległością workspace). Windows: rebind tego
  samego portu UDP zaraz po dropie endpointu A może przejściowo paść — boot
  serwera B w krótkiej pętli bind-retry. Odnotować w docs/status koszt
  wall-time w blocking stage `test`. Test na obecnym kodzie MA prawo być
  czerwony — commit dopiero razem ze Step 2 (nie zostawiamy czerwonego
  mastera).
- **(d) Dispatch:** `[opus]` — `core-implementer`, effort: high.

### Step 2 — fix autorytetu: gate resetu w `Reconnecting::call`
- **(a) Co:** `core/remote/src/lib.rs` — semantyka trzystopniowa zamiast
  binarnej:
  - `ConnectionFatal` → jak dziś: `reset` (close + zdjęcie z cache) + tor
    `RetryMode`;
  - `StreamLocal` → **evict-without-close**: zdjęcie z cache (identity-guarded
    jak `reset`, `lib.rs:203-211`) BEZ `c.close()` — in-flight calle
    współbieżnych wołających kończą się na swoich `Arc`ach, następny call
    re-dialuje. To jest faktyczny parytet z gatewayem (`RouteTable` tylko
    usuwa wpis z mapy, `modules/gateway/src/lib.rs:792-797`); `close()` na
    współdzielonym połączeniu przy per-payload błędzie lokalnym
    (`Codec`/`FrameTooLarge` są StreamLocal) ubijałby zdrowe współbieżne
    strumienie — nowy failure mode, zakazany. Bez replay (StreamLocal nie
    dowodzi, że request nie dotarł — replay tylko na torze
    `OnceAfterReconnect` po ConnectionFatal, bez zmian);
  - `PeerAnswer` → nic (odpowiedź żywego peera).
  Zaakceptowany koszt: powtarzalny per-payload błąd Codec powoduje churn
  redialu (evict zdrowego połączenia per błąd) — odnotować w doc-commencie.
  Aktualizacja doc-commentów (`:171-174`, `:224-230`) i testów:
  `stream_local_failures_do_not_reset_or_replay` (`tests.rs:213-227`) →
  „stream-local evictuje (następny call dostaje świeży conn), NIE replayuje,
  NIE zamyka"; `stream_local_failure_preserves_concurrent_call_and_cached_connection`
  (`tests.rs:392-444`) → przepisany: współbieżny call NADAL przeżywa
  (własność zachowana przez evict-without-close), ale cache po evikcie jest
  pusty; tabela provenance `tests.rs:7-46` przepisana pod nową semantykę;
  `peer_answers_do_not_reset_or_replay` (`:188`) zostaje. Jeśli Step 1 wskazał
  gałąź (B): dodatkowo fix w `EdgeDialer::dial`/`dial_with_config` wg
  przypiętego wariantu, z własnym testem failing-branch.
- **(b) Dlaczego teraz:** to jedyny punkt decyzyjny „czy to martwe połączenie"
  na padającej ścieżce konsumenckiej; przy gałęzi (A) bez tego Step 3 nie
  zzielenieje; przy (B)/(C)/(D) wchodzi jako deklarowane wyrównanie semantyki
  (patrz tabela).
- **(c) Jak:** świadoma zmiana udokumentowanej decyzji („only a proven
  connection-fatal error drops that connection") — odnotowana w commit message
  i w erracie tego planu (rule 4). Fail-closed dla mutacji zachowany: evict ≠
  replay. Znany, przedistniejący koszt rozszerzony przez ten fix: `get()`
  trzyma mutex przez cały `dial()` (`lib.rs:191-196`, bounded 5s) — częstszy
  evict = częstsze okna serializacji wołających za 5-sekundowym dialem;
  reviewer atakuje tę klasę (resource-owned-by-wrong-scope) w Step 5.
- **(d) Dispatch:** `[opus]` — `core-implementer`, effort: high. Commit razem
  ze Step 1 (`fix(remote): ...`).

### Step 3 — committed splitproof assertion `[B1-REDIAL]`
- **(a) Co:** `tools/splitproof/src/main.rs` — nowa
  `async fn stub_redial(ctx, fleet, p)` wywoływana w `run()` po `rdy_dead()`
  (`main.rs:614`), przed `[LV2]`. Sekwencja (kolejność jest treścią asercji):
  1. **PRIMING martwego połączenia:** `register_login` gracza A,
     `create_character` (postać pre-kill), `inventory_of` → MUSI dać 200 —
     to wymusza żywe, CACHE'OWANE połączenie stuba inventory→characters
     (Reconnecting dialuje leniwie — bez tego kroku kill zastaje pusty cache
     i pierwszy post-respawn call trywialnie dialuje świeżo, nie wykonując
     ryzykownej gałęzi);
  2. kill+respawn characters-svc skopiowane z `rdy_dead` (`main.rs:1954-2023`):
     `position(name == "characters-svc")` → `try_wait` guard →
     `fleet.remove(idx)` → sleep 800ms → `ensure_no_stale_listener` →
     `ctx.spawn(ctx.service("characters-svc"))` → `ctx.wait_healthy` →
     `fleet.insert(idx, running)`;
  3. `create_character` **PO** respawnie, w pętli poll odpornej na 5xx —
     `create_character` (`main.rs:2113-2137`) retryuje tylko 429, a pierwszy
     post-respawn create idzie po martwym cache'owanym kliencie edge GATEWAYA
     (jeden 503 przed evict-and-heal, `modules/gateway/src/lib.rs:791-798`) —
     bez tej pętli asercja flake'uje na POPRAWIONYM kodzie;
  4. poll `inventory_of` (`main.rs:2161`) do statusu 200, bounded deadline 60s
     (okno detekcji martwego conn ≤30s idle + zapas);
     `p.check("[B1-REDIAL] inventory->characters ownership after peer restart -> 200", ...)`.
- **(b) Dlaczego teraz:** obowiązek z backlogu („committed splitproof
  assertion"). **Uczciwość zakresu (za precedensem `[RDY-DEAD]`,
  `main.rs:1908-1921`, i pamięcią amplification-proof-belongs-in-unit-tests):
  ta asercja dowodzi RECOVERY na splicie, NIE dyskryminuje fixed/unfixed** —
  na niepoprawionym HEAD martwy conn i tak może się odzyskać przez
  `open_bi`→ConnectionFatal w oknie polla. Dyskryminujący dowód failing branch
  żyje w Step 1 (deterministyczne testy jednostkowe + repro). Napisać tę notę
  wprost w doc-commencie asercji. `[3]`/`[5b]`/`[P2]` nigdy nie łączą owner_of
  z restartem peera — luka realna niezależnie od dyskryminacji.
- **(c) Jak:** poll, nie single-shot (wzorzec `i_gate` `main.rs:1873-1899`);
  asercja to „osiąga 200 w deadline", nie „nigdy nie błądzi". Asertowana
  postać (krok 3) tworzona wyłącznie po respawnie — pre-kill postać z kroku 1
  służy tylko primingowi. `ServiceSpec` tylko z `ctx.service(...)` (kanoniczny,
  objęty preflightem driftu floty).
- **(d) Dispatch:** `[opus]` — `core-implementer`, effort: high. Osobny commit
  (`test(splitproof): ...`).

### Step 4 — weryfikacja (protokół jednego rolloutu)
- **(a) Co:** sekwencyjnie: check `cargo`/`rustc` idle → `cargo test -p remote`
  → check idle → `cargo run -p verifyctl -- --fast` (obejmuje blocking
  split-proof z `[B1-REDIAL]`).
- **(b) Dlaczego teraz:** po kodzie, przed reviewem diffu — reviewerzy dostają
  zielony stan; znany wyjątek: B2 (flake devctl w pełnej równoległości
  workspace) może dać 1 niepowiązany FAIL w stage `test` — odnotować, nie
  „naprawiać przy okazji".
- **(c) Jak:** skill `safe-verification`; testy asyncevents nie są dotykane,
  ale pełny `verifyctl` i tak serializuje przez `run/rollout.lock`.
- **(d) Dispatch:** `[inline]` (odpalanie komend + odczyt tabeli PASS/FAIL).

### Step 5 — adversarial review diffu + dokumentacja + pamięć
- **(a) Co:** (1) jeden pass `core-reviewer` (diff core/remote + splitproof,
  metoda inna niż implementerów, model ≥ opus (tier implementerów); klasy do
  ataku: patrz (c));
  (2) `proof-auditor` na Step 1+3 — diff dotyka blocking-verify surface
  (splitproof); pytania wprost: „czy test Step 1 byłby czerwony na unfixed
  HEAD?", „czy `[B1-REDIAL]` primuje cache przed killem?", „czy nota
  o niedyskryminowaniu fixed/unfixed jest w doc-commencie?"; (3) punch listy
  wracają do lane'ów, nie są wchłaniane; (4) nowy doc statusowy
  `docs/status/2026-07-15-HHMM-b1-stub-redial-fix-status.md` (stary status
  z 1120 NIE jest przepisywany — archiwum). Pamięć
  `edge-stub-no-reconnect-after-peer-restart.md` aktualizowana dopiero PO
  Step 6 (akceptacja na żywo) — do tego czasu co najwyżej dopisek „gate fixed,
  chaos re-run pending"; po zmianie pamięci `scripts/memory-sync.ps1 push`.
- **(b) Dlaczego teraz:** review po zielonej weryfikacji = reviewer atakuje
  działający diff, nie kompilację.
- **(c) Jak:** wg sekcji Adversarial Subagent Review (klasy z taksonomii dla
  core/remote: error-class-folded-into-success, resource-owned-by-wrong-scope,
  constant-shadowing-config; dla splitproof: proof-nie-wykonuje-gałęzi).
- **(d) Dispatch:** review-agenci wg definicji; dokumentacja/pamięć `[inline]`.

### Step 6 — akceptacja na żywo: powtórka chaosu Welesa
- **(a) Co:** scenariusz z odkrycia B1: `weles deploy target/debug` → `weles up
  split` → kill characters-svc pod żywym ruchem → auto-restart → postać
  utworzona po restarcie → `GET /inventory/{cid}` przez gateway = 200.
- **(b) Dlaczego teraz:** to jest środowisko, które buga znalazło; splitproof
  dowodzi klasy, chaos Welesa dowodzi oryginalnej obserwacji.
- **(c) Jak:** faza akceptacji na żywo — jeśli COKOLWIEK padnie: stop, diagnoza,
  raport z opcjami (doktryna „report, don't fix"). Jeden rollout naraz.
- **(d) Dispatch:** `[inline]`, za zgodą Lukasza na uruchomienie floty.

## Review planu (przed zatwierdzeniem, 2026-07-15)

Jeden niezależny pass grumpy-senior (tier sesji, think hard). Punch lista:
2 BLOCKERy (Step 3 nie primował cache'u przed killem; overclaim „dowód failing
branch" dla asercji split — nie dyskryminuje fixed/unfixed), 4 MAJORy (404
niemożliwe z martwego transportu → gałąź (D) + headline; StreamLocal-pinning
co najwyżej przejściowy → domknięcie Step 2 przeskalowane na „parity";
`reset()` zamyka współdzielone połączenie → evict-without-close dla
StreamLocal; retry-past-5xx dla post-respawn create), 3 MINORy (serializacja
testu per-binarka nie per-plik + hang-guard 90s + bind-retry na Windows;
mutex-przez-dial odnotowany jako klasa do ataku; pamięć → resolved dopiero po
Step 6). Wszystkie naniesione powyżej przed pokazaniem planu Lukaszowi.
Punkty zaatakowane i uznane za zdrowe: identity-guard resetu, fail-closed
RetryMode, sibling sweep, rezygnacja z unifikacji z mechanizmem gatewaya.

## Errata

- **2026-07-15, po Step 1:** repro (`core/remote/src/redial_tests.rs`,
  niezacommitowany) wybrał **gałąź (C)** — warstwa połączenia odzyskuje się
  SAMA: po primingu i teardownie serwera pierwszy call pada jako
  `edge: connection: closed by peer …` → `Unavailable`/**ConnectionFatal** →
  reset → następny call (Never) lub replay (OnceAfterReconnect) trafia w nowy
  serwer. `test result: ok. 5 passed`. **Zastrzeżenie nazwane przez
  implementera:** repro pokrywa wyłącznie GRACEFUL close —
  zwolnienie portu do rebindu strukturalnie wymaga `RunningServer::close()`
  (accept-task trzyma klon endpointu, `core/edge/src/server.rs:202-232`),
  a `close()` wysyła jawny QUIC CONNECTION_CLOSE, stąd czysty ConnectionFatal.
  Twardy kill (taskkill/SIGKILL — to robi chaos Welesa) nie wysyła ramki
  zamknięcia; detekcja martwego połączenia idzie wtedy przez failed
  `open_bi`/write/read albo 30s idle — ścieżka, która MOŻE klasyfikować się
  jako `Error::Stream`/StreamLocal (pinning-concern) i pozostaje NIEZBADANA
  na warstwie połączenia (rebind-same-port nie umie jej odtworzyć w jednym
  procesie). Zgodnie z tabelą: STOP + raport do Lukasza przed jakąkolwiek
  decyzją o Step 2.
