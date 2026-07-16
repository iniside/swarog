# Weles M1 — plaster pierwszy: gateway w trybie zarządzanym

Cienki pionowy plaster przez cały kształt M1: agent serwuje `resolve`, gateway-svc
pyta go zamiast czytać env, standalone zostaje bit-identyczny. Reszta M1 (minting,
SQLite, rollback, re-resolve, LB) jest **poza tym planem**.

Design: [weles-design.md](../reference/weles-design.md). Baza: 4 subagenty Explore
(gateway-svc + blast radius env / `core/remote` / weles + punkt wpięcia / bramka
parity), exact file:line.

> **Rev 1 (po grumpy review, opus/think-hard):** trzy blokery i sześć High
> naniesione. Rozstrzygnięcia PRZED implementacją: (1) verb dostaje wymiar
> **rodzaju adresu** — `resolve(provider, kind)`, bo gateway potrzebuje 8 adresów
> dwóch klas, a `accounts` obu naraz; (2) **kolejność 1→2a→2b→3→4→6→5** — dowód
> płaci za erozję bramki, a nie odwrotnie; (3) **dowód musi być bramką**, co wymaga
> nowego kroku: `weles::lock` uczy się pożyczać lease; (4) `ORCHESTRATOR_URL`
> dostaje port w manifeście (to jedyne miejsce, gdzie porty wolno pisać); (5) mapa
> resolve jest **topology-aware**; (6) Step 2 rozbity na 2a/2b. Zmiany w Changelogu.

## Dlaczego to jest plaster, a nie ficzer

M1 to milestone od udowodnienia kształtu (design doc, „M1 scope"). Przy statycznych
portach `resolve` zwraca dokładnie to, co dziś zwraca env. Nic się nie poprawia —
powstaje szew, póki stawka jest zerowa.

**Gateway jest naturalnie pierwszy:** woła peerów, a jego edge'a nie woła nikt, więc
może zacząć resolvować bez zmuszania kogokolwiek do ruchu. Jego własny port zostaje
statyczny (publiczne wejście). *(Uwaga: gateway bootuje 11. z 12 — ostatni jest
`admin-svc`, `manifest.rs:211-230`. Kolejność bootu nie jest tu argumentem.)*

## Ustalenie, które zmniejsza plan

`cmd/gateway-svc/src/main.rs:116-131` buduje `ProcessWiring` z ośmiu `env_addr(...)`
**przed** `modules()`. Wszystko poniżej jest agnostyczne wobec źródła adresu —
`ProcessWiring` (`core/lifecycle/src/wiring.rs:17-27`), `Stub::new`
(`core/remote/src/lib.rs:487`), `PEER_SLOT`, `RouteTable`. Podmieniamy **źródło tych
ośmiu stringów**, nic więcej.

**`core/remote::Stub` nie jest ruszany.** Re-resolve wymagałby przebicia trzech
zamrożonych kopii adresu (`Stub.peer_addr` `:462`, `EdgeDialer.peer` `:275`,
`probe_loop` snapshot `:405`) ORAZ `RouteTable.peers`
(`modules/gateway/src/lib.rs:623,681-693`, zamrożone przy `build`). Osobny plan.

## Osiem adresów, DWIE klasy — rdzeń kontraktu

| konsument | klucz | wartość | źródło |
|---|---|---|---|
| gateway | 6 × `*_EDGE_ADDR` | 9000/9001/9003/9006/9008/9009 | peer `edge_port` |
| gateway | `ADMIN_HTTP_ADDR` | `127.0.0.1:8085` | admin **`http_port`** (`manifest.rs:215`) |
| gateway | `ACCOUNTS_HTTP_ADDR` | `127.0.0.1:8084` | accounts **`http_port`** (`:86`) |

`accounts` występuje **w obu klasach naraz** (edge 9003 + http 8084). `admin` ma
`edge_port: None` — nie jest peerem, tylko originem passthrough. Verb keyed wyłącznie
na `provider` **strukturalnie nie umie** tego wyrazić.

**Semantyka blank-default jest niosąca:** `env_addr("ADMIN_HTTP_ADDR", "")`
(`main.rs:127`, komentarz `:110-111`) — pusty origin każe porzucić prefiks, trasa
zostaje 404. To zachowanie musi przeżyć.

## Inwarianty (każdy krok)

**Zero-sharing jest DWUKIERUNKOWE** (`weles-design.md:20-21`): weles nie importuje
crate'a workspace'u ORAZ workspace nie importuje welesa. Konsekwencja, która rządzi
dowodami: **`core/remote` nie może testować przeciw serwerowi welesa, a weles nie
może testować przeciw klientowi** — każda strona ma własną atrapę, a zgodność stron
pinuje WYŁĄCZNIE Step 6.

Dalej: standalone bit-identyczny (devctl/processctl/splitproof wołają
`game_backend_fleet_with_environment` — `tools/devctl/src/supervisor.rs:342`,
`tools/splitproof/src/main.rs:521`); moduły nie czytają env topologii; `platform/*`,
`lock.rs`, `state.rs`, `prep.rs` i czyste fn zostają sync; handler sygnału dotyka
tylko statyka; P6 (`drop(control)` po teardownie); `_lock` ostatni; **nic z wyspy
async nie produkuje `Observed::Exited`**.

## Odstępstwa od designu — zapisane, nie przemycone

1. **Brak scopingu per-konsument.** Design (`weles-design.md:93-97`) mówi: *resolve
   scoped per-consumer, never „give me the fleet map"*. M1 serwuje mapę bez
   tożsamości wołającego, bo **nie ma dziś mechanizmu tożsamości** na tym skoku
   (HTTP po localhoście; `SO_PEERCRED` byłby osobną robotą). Deviacja świadoma i
   wąska: skok jest lokalny, a trust domain to jedna maszyna. Zamknięcie — maszyna
   druga, gdy kontrakt przetnie granicę zaufania.
2. **Kształt odpowiedzi NIE odstępuje.** Design mówi *resolve returns all live
   instances* (`:98`), więc verb zwraca **listę** od pierwszego dnia, z dokładnie
   jednym elementem w M1. Zwracanie skalara zabetonowałoby kształt, który LB musi
   złamać — w milestonie, którego celem jest właśnie kształt.

---

## Step 1 — weles: jeden autorytet dla adresu peera `[opus]`

**(a) Co:** `weles/src/manifest.rs`. Dziś `edge_port: Some(9000)` (`:169-170`) i
`("CHARACTERS_EDGE_ADDR", "127.0.0.1:9000")` (`:185`) to ten sam fakt zapisany dwa
razy. Tak samo `:154`, `:174`, `:198-209`, `:219-229`.

Nowe pole obok `env_extra`:
`peers: &'static [(&'static str, &'static str, AddrKind)]` = (klucz env, provider,
rodzaj), gdzie `enum AddrKind { Edge, Http }`. `compose_env` rozwiązuje je przez
lookup po bootującej flocie. `env_extra` zostaje wyłącznie dla literałów
(`TLS_MODE`, dev-seedy).

**Rodzaj jest POLEM, nie zgadywany z sufiksu klucza** — inaczej nazwa env znów
staje się autorytetem, czyli dokładnie ta inwersja, którą ten krok zabija.

**(b) Dlaczego teraz / order:** PRZED serwerem. `resolve` odpowiada z
`edge_port`/`http_port`. Zostawienie literałów = dwa autorytety dla „gdzie jest
characters", a pierwszy dryf między nimi jest niewidoczny. Fix-the-Authority.

**(c) Jak:** wartości wychodzą **identyczne** — zweryfikowane przez reviewera na
pełnych danych: każdy adres to `127.0.0.1:<port peera>`, żadnego obcego hosta,
żadnego peera z `edge_port: None` wołanego jako edge. Dowód: istniejące goldeny
`manifest_tests.rs:48-244` muszą przejść **bez edycji**; `weles-fleet-parity` też
(porównuje wartości, nie sposób powstania). Lookup nieznanego providera albo
`AddrKind::Edge` na usłudze z `edge_port: None` = `panic!` z nazwą (błąd
programisty przy dodawaniu usługi — konwencja „duplicate registration PANICs").
**Prove-the-branch:** (i) test, że `AddrKind::Edge` na `admin-svc` (`edge_port:
None`, `:214`) failuje głośno; (ii) test, że zmiana `edge_port` peera **przenosi
się** na composed env konsumenta — dziś nie przenosi, to jest ta wcześniej-błędna
gałąź.

**(d) Dispatch:** `[opus]` core-implementer.

## Step 2a — weles: wyspa tokio, sam cykl życia `[opus]`

**(a) Co:** `weles/src/agentapi.rs` — runtime tokio na własnym wątku + serwer HTTP
na localhoście, **bez żadnych verbów** (jedna trasa `/healthz` → 200). Wpięcie w
`supervisor.rs`.

**(b) Dlaczego teraz / order:** **w repo NIE MA precedensu dla runtime'u na
osobnym wątku obok kodu sync** (`weles-design.md:437-440`). Cykl życia jest tu
całym ryzykiem i musi być udowodniony sam, zanim dołożymy do niego kontrakt.

**(c) Jak — konkrety:**
- **Bind: obok `ControlServer::bind`, `supervisor.rs:748-768`** — po `Reporter`
  (`:672`), **przed `boot()` (`:781`)**. Boot jest sekwencyjny i bramkowany readyz
  (`:863-919`, spawn `:875`, pętla `:885-918`), więc usługa potrzebująca `resolve`
  do dojścia do readyz **nie dojdzie**. *Precyzyjnie: to NIE jest zakleszczenie —
  `HEALTH_DEADLINE` bramkuje i `bail!`uje (`:914-915`). To ograniczona porażka
  startu.* Wniosek ten sam, powód poprawny.
- **Port: z manifestu.** Nowa stała `AGENT_PORT` w `manifest.rs` — to **jedyne**
  miejsce, gdzie w welesie wolno pisać port (`manifest.rs:5-7`). Objąć go
  `ensure_no_stale_listener` (dziś tylko `http_port`, `supervisor.rs:873`). To
  usuwa problem „URL jest runtime'owy, a `env_extra` jest `'static`".
- **Wzorzec: `ControlServer`** (`control.rs:75-148`) — skopiować uzgodnienie
  gotowości `sync_channel(1)` (`:97,116`) z `BIND_DEADLINE` i **wszystkimi trzema
  ramionami** (`:116-130`), oraz rozdział autorytetu stopu: prywatny `shutdown` +
  `dead`, **NIGDY `fleet_stop`** (`:67-74`).
- **Anulowanie:** `AtomicBool`+sleep NIE dociera do accept parkującego na `.await`.
  Drop sygnalizuje `Notify`/`oneshot` albo `Runtime::shutdown_timeout`, potem join.
  Kopiowanie `ControlServer::drop` (`:141-148`) dosłownie **zawiesi join**.
- **`Runtime::drop` blokuje** ⇒ runtime dropowany **na swoim wątku**, nigdy na
  wątku supervisora — inaczej stalluje `_lock` (`:831`).
- **Drop: PO teardownie** (obok `:819`, control-shaped), bo usługa w drenażu może
  jeszcze wołać. Zapisać przy komentarzu P6.
- **Tokio features: `rt-multi-thread`, `macros`, `net`, `io-util`. NIGDY `signal`,
  NIGDY `process`.** `process` instaluje handler SIGCHLD i reapuje dzieci spod
  `try_wait` (`supervisor.rs:988-998`) — psuje `Observed::Exited` w całej flocie.
- **Ten zakaz NIE może być komentarzem.** Resolver-2 unifikuje feature'y w całym
  grafie builda, więc `weles/Cargo.toml` **nie jest autorytetem** dla feature'ów
  tokio, które weles dostanie. Dowodem jest **test** wołający
  `cargo tree -e features -p weles` i failujący na obecności `process`/`signal`.
  (To repo ma świeżą lekcję, że komentarz nie jest strażnikiem —
  `weles-design.md:455-459`.)
- **Bez TLS = bez pytania o `ring`/`aws-lc-rs`:** `hyper`/`hyper-util` bez feature'a
  TLS nie dotykają rustls. `aws-lc-rs` nie ma w `Cargo.lock`. `hyper` 1.x jest.
- **Zasięg do nazwania w commicie:** dodanie `net` do pinu tokio (root
  `Cargo.toml:166`) sięga całego builda. Alternatywa: pin lokalny w weles.
**Prove-the-branch:** bind-fail → `Err` bez spawnu; drop joinuje w budżecie (test
z hang-guardem, nie sleepem); `/healthz` odpowiada w trakcie `boot`; runtime
dropnięty nie blokuje zwolnienia locka.

**(d) Dispatch:** `[opus]` core-implementer.

## Step 2b — weles: verby `resolve` i `hello` `[opus]`

**(a) Co:** `resolve(provider, kind) -> {addrs: [String]}` (jeden element w M1) i
`hello(service, pid) -> {}` (loguje; kontrakt bez mechanizmu).

**(b) Dlaczego teraz / order:** po 2a (żywy cykl życia) i po 1 (jest z czego
odpowiadać).

**(c) Jak:**
- **Mapa jest TOPOLOGY-AWARE.** `run_up(topology)` (`supervisor.rs:628`) uruchamia
  `monolith()` dla `Topology::Monolith` (`:731-734`). Mapa ze `split_fleet()`
  bezwarunkowo rozdawałaby adresy **dwunastu nieistniejących procesów**. Mapa
  powstaje z **defs bootującej topologii**; monolit ⇒ mapa pusta, każdy `resolve` →
  404. Zgodne z `weles-design.md:65` (monolit spełnia kontrakt trywialnie, bo nie
  ma peerów).
- **Dane:** mapa `(provider, kind) -> addr` wyliczona przed spawnem wątku i
  przeniesiona przez `move` — dane `'static`, bez locka. Wzorzec istnieje:
  `supervisor.rs:778` robi to dla `ports`.
- **`Reporter` nietykany** (`!Sync`: `Cell`/`RefCell`, `:533-534`). Serwer **nie
  dotyka `shared`** — omija konstrukcyjnie zatrucie mutexa (patrz „Znane ryzyko").
- Nieznany provider albo brak adresu danej klasy → **404**, nigdy zgadywanie.
**Prove-the-branch:** `resolve` zwraca to samo, co `compose_env` dla tej samej pary
(pinuje jeden autorytet ze Stepu 1); `resolve("admin", Edge)` → 404;
`resolve("accounts", Edge)` ≠ `resolve("accounts", Http)`; **monolit ⇒ 404 na
wszystko**.

**(d) Dispatch:** `[opus]` core-implementer.

## Step 3 — `core/remote`: klient resolve `[opus]`

**(a) Co:** `pub async fn resolve_peer(orchestrator_url, provider, kind) ->
Result<Vec<String>>`. HTTP+JSON, bounded timeout, bez retry.

**(b) Dlaczego teraz / order:** po serwerze, przed mainem.

**(c) Jak:**
- **Dom `core/remote`** — zgodnie z designem, żeby `Stub` mógł go użyć przy
  re-resolve. W M1 używa go wyłącznie main.
- **`core/remote` NIE jest bramkowany przez `public-api`** — `discover`
  (`tools/verifyctl/src/stages/public_api.rs:200-226`) chodzi tylko po
  `api/<domain>/{api,events}`.
- **Reguła „core nigdy nie czyta env"** (`core/remote/src/lib.rs:94-98`, dosłownie:
  *„thread it through `Stub::new` the way `peer_addr` already is, NOT env"*).
  `ORCHESTRATOR_URL` czyta **main**.
- **`reqwest` dozwolony w `core/*`:** archcheck rule 16
  (`tools/archcheck/src/main.rs:531-561`) zabrania tylko Module/Api/Events/Rpc/Demo.
  `FORBIDDEN_API_DEPS` (`:92-94`) dotyczy wyłącznie crate'ów `<name>api`.
- **Koszt do nazwania:** `core/remote` ma dziś `tokio = features=["sync"]`;
  `reqwest` wciąga hyper do **każdego** `cmd/*-svc`. Świadomy koszt.
- **DOWÓD TYLKO PRZECIW WŁASNEJ ATRAPIE.** Zero-sharing jest dwukierunkowe, więc
  `core/remote` nie może wziąć welesa jako dev-dep. Test stawia własny mini-serwer
  HTTP w dev-deps. **Zgodność stron pinuje wyłącznie Step 6** — zapisać to w
  doc-komentarzu klienta, bo inaczej ktoś uzna te testy za dowód interop.
**Prove-the-branch:** nieznany provider → typowany błąd; orchestrator nieosiągalny
→ błąd w budżecie czasu, nie zawieszenie; kształt JSON-a niezgodny → typowany błąd.

**(d) Dispatch:** `[opus]` core-implementer.

## Step 4 — `cmd/gateway-svc`: ścieżka bootu zarządzanego `[opus]`

**(a) Co:** `cmd/gateway-svc/src/main.rs:116-131`. `ORCHESTRATOR_URL` ustawiony ⇒
adresy z `resolve_peer`; nieustawiony ⇒ `env_addr` dokładnie jak dziś. Jedna
deterministyczna decyzja na starcie, bez warstwowania.

**(b) Dlaczego teraz / order:** zamyka pętlę.

**(c) Jak:**
- **Polityka porażki jest PER KLASA, nie globalna:**
  - **Edge peer** nieresolwowalny ⇒ **proces pada**. Cichy fallback na default
    (`127.0.0.1:9000`) byłby gorszy niż śmierć — to adres, pod którym nikogo nie ma.
  - **Passthrough origin** nieresolwowalny ⇒ **pusty origin**, czyli prefiks
    porzucony, trasa 404 — **dokładnie dzisiejsza semantyka blank-default**
    (`main.rs:110-111,127`). Fail-closed nie może jej skasować.
- `cmd/gateway-svc/src/lib.rs` **nietykany**: archcheck rule 17
  (`tools/archcheck/src/main.rs:735-770`) skanuje **tekstowo** literał
  `Stub::new("<domain>"` dla 6 domen; złamanie = FAIL blokującej `fortress`.
  Zapisać, żeby nikt tego kroku nie „uprościł".
- `PLAYER_EDGE_ADDR`, TLS, `CREDENTIAL_ADMISSION_TIMEOUT_MS` zostają z env w obu
  trybach — to konfiguracja własna procesu, nie adresy peerów.
- weles spawnuje gateway z `ORCHESTRATOR_URL` (z `AGENT_PORT`, Step 2a) zamiast
  ośmiu kluczy adresowych — zmiana w `manifest.rs` `env_extra`/`peers` gatewaya.
**Prove-the-branch — MIEJSCE MA ZNACZENIE:** `cmd/gateway-svc/tests/` to target
**integracyjny**: sięga tylko do libki, **nigdy do `main.rs`**. Dlatego dowód idzie
w `#[cfg(test)] mod` **wewnątrz main.rs** — istniejący wzorzec (`env_addr`,
`admission_budget_from_value` są tak testowane, `:29-30`, `:76-77`). Wydzielić
czystą fn zwracającą **własny typ** z ośmioma parami (klucz→adres); test porównuje
ten typ dla obu trybów. **Nie dotykamy `ProcessWiring`** (brak `PartialEq`, `peers`
prywatne, tylko `peer_or` — `core/lifecycle/src/wiring.rs:17-43`); porównanie
własnego typu nie wymaga zmiany w `core/lifecycle`.

**(d) Dispatch:** `[opus]` core-implementer.

## Step 5 — `weles::lock`: pożyczony lease `[opus]`

**(a) Co:** `weles/src/lock.rs` — ścieżka jednorazowego pożyczenia lease'a,
bit-kompatybilna z tym, co robi processctl, z walidacją tożsamości rodzica.

**(b) Dlaczego teraz / order:** **bez tego Step 6 nie może być bramką.** verifyctl
trzyma `OwnedLease` przez cały manifest i przekazuje dziecku pożyczkę
(`tools/verifyctl/src/runner.rs:57,252`, `spawn_borrower(spec, "splitproof")`).
`weles::lock` ma dziś **jedno** wejście — `acquire` (`:69`) — bez ścieżki
pożyczania, i nie może zaimportować `processctl::OwnedLease` (zero-sharing). Stage
welesa w verifyctl **zakleszczyłby się na `run/rollout.lock` przeciw własnemu
lease'owi verifyctl**.

**(c) Jak:** weles już deklaruje bit-kompatybilne uczestnictwo w locku (CLAUDE.md,
`weles-design.md:8-9`) — to jest rozszerzenie tej samej deklaracji, nie nowy
mechanizm. Kopiujemy kształt processctl (zero-sharing: kopia z notą proweniencji).
Pożyczkobiorca waliduje tożsamość/rolę rodzica i **fail-closed** przy niezgodności.
**Prove-the-branch:** pożyczka z niepoprawną tożsamością rodzica → odmowa;
pożyczka poprawna → brak ponownego brania locka; `acquire` bez pożyczki działa jak
dziś.

**(d) Dispatch:** `[opus]` core-implementer.

## Step 6 — dowód trybu zarządzanego jako BLOKUJĄCA stage `[opus]`

**(a) Co:** nowa stage `weles-managed-gateway` w `tools/verifyctl` + harness
bootujący welesa na realnej flocie.

**(b) Dlaczego teraz / order:** **PRZED Stepem 7**, bo design mówi wprost, że każde
wyjście usługi spod bramki jest **opłacone** żywym dowodem
(`weles-design.md:530-532`) — opłacone, czyli dowód ląduje pierwszy. Dziś **nic nie
bootuje welesa i nie asertuje na nim**: `weles/tests/platform.rs` spawnuje welesa
jako `__test-child` do testów kontenmentu, splitproof jest hard-wired do processctl
(`main.rs:521`).

**To jest JEDYNY dowód, że serwer welesa i klient `core/remote` zgadzają się co do
drutu** (zero-sharing blokuje test przekrojowy). Dlatego **nie może być
`#[ignore]`** — blokująca stage `test` woła `cargo test`, który testy `#[ignore]`
**pomija**; byłby to komentarz, który się kompiluje.

**(c) Jak:**
- Stage bootuje welesa (przez pożyczony lease, Step 5) na flocie ze `deploy/` i
  asertuje: gateway wstał, `/readyz` 200, **oraz JEDNA operacja przechodząca przez
  Remote do peera** — to dowodzi, że zresolvowany adres jest realnie użyty, a nie
  tylko pobrany. Plus jedna przez passthrough (dowodzi klasy `Http`).
- Rejestracja stage'y: `StageId` + `name()` (`tools/verifyctl/src/model.rs:4-47`),
  wpis w `BLOCKING` (`stages/mod.rs:30-91`), `pub mod`, **oraz aktualizacja
  `stage_manifest_is_frozen`** (`stages/mod.rs:255-286` — pinuje nazwy i `len()==17`).
  Wzorzec najmniejszej samodzielnej stage'y: `conformance.rs`.
- Wymaga `deploy/` i bazy ⇒ rollout, jedna flota naraz.
**Prove-the-branch:** stage FAILuje, gdy gateway dostanie `ORCHESTRATOR_URL`
wskazujący na martwy port (dowodzi, że asercja realnie zależy od resolve, a nie
przechodzi przypadkiem).

**(d) Dispatch:** `[opus]` core-implementer. **Review:** + proof-auditor.

## Step 7 — bramka parity: zapisać rozejście, nie osłabić bramki `[opus]`

**(a) Co:** `tools/verifyctl/src/stages/weles_fleet_parity.rs` +
`weles_fleet_parity_tests.rs` + `docs/reference/weles-fleet-parity.md` +
`weles/src/manifest_tests.rs`.

**(b) Dlaczego teraz / order:** **ostatni, po dowodzie.** Step 4 rozjeżdża
weles↔processctl celowo. Bez tego kroku bramka jest trwale czerwona.

**(c) Jak:**
- **Bramka będzie czerwona na DZIEWIĄTYM diffie, nie ośmiu:** poza ośmioma
  zniknięciami dochodzi `ORCHESTRATOR_URL` **present in weles, absent in
  processctl** (`weles_fleet_parity.rs:295-297`). Wykluczenie musi objąć obie
  strony.
- **Gdzie wykluczać:** `is_excluded` (`:64`) i `strip_excluded` (`:117`) są
  **key-only i service-blind**, wołane z `view_from_*` (`:140`, `:161`), które nie
  mają kontekstu diffu. `diff_env` (`:281`) **dostaje `label`** (= `pkg`, `:227`) —
  więc per-usługa/per-klucz jest wyrażalne **tam**. To znaczy: wyprowadzić
  wykluczenie ze `strip_excluded`, przepiąć `exclusion_policy()` (`:73-79`) i
  poprawić doc modułu (`:26-32`). Nie przepisanie, ale **nie jest to „mała
  zmiana"**, jak twierdziła rev 0.
- **Test do zmiany, którego rev 0 nie wymieniła:**
  `exclusion_predicate_is_the_allowlist` (`weles_fleet_parity_tests.rs:19-25`)
  asertuje, że `is_excluded` **jest** allowlistą.
- **Nie osłabiać globalnie.** Wykluczenie per-usługa i per-klucz, z powodem w
  kodzie. Pozostałe 11 usług porównywanych **w pełni**.
- Golden gatewaya (`manifest_tests.rs:174-190`): 8 kluczy znika, `ORCHESTRATOR_URL`
  dochodzi.
- `dependency_order_diffs` (`:339-360`) i set-diff nazw (`:318-323`) **bez zmian** —
  zweryfikowane przez reviewera: gateway zachowuje nazwę i pozycję.
- `docs/reference/weles-fleet-parity.md` twierdzi dziś: *„everything topology-shaped
  is compared"* i *„this stage is its ONLY parity gate"*. Oba przestają być prawdą.
  **Poprawić w tym samym rolloucie**, wskazując Step 6 jako zastępstwo.
**Prove-the-branch:** wykluczenie jest **wąskie** — dryf peer-env DOWOLNEJ innej
usługi dalej FAIL; dryf NIE-adresowego klucza gatewaya (`TLS_MODE`) dalej FAIL.

**(d) Dispatch:** `[opus]` core-implementer. **Review:** + proof-auditor.

---

## Weryfikacja

Po krokach bez DB: `cargo test -p weles`, `-p remote`, `-p gateway-svc`. Po Stepie
6 i 7: `cargo run -p verifyctl -- --fast` (rollout — sprawdzić cargo/rustc i flotę,
jedna komenda naraz).

## Poza zakresem (świadomie)

- **Re-resolve i minting** — wymagają `RouteTable.peers`
  (`modules/gateway/src/lib.rs:623,681-693`), trzech kopii adresu w `Stub` i
  snapshotu w `probe_loop` (`core/remote/src/lib.rs:405` — dziś sondowałby stary
  adres w nieskończoność). Osobny plan.
- **Round-robin LB** — pula per instancja, wybór, zdrowie instancji, `RetryMode`.
  Verb zwraca listę, więc kształt nie jest zabetonowany.
- **SQLite, rollback, M2, `DOWN_TIMEOUT`** — niezależne, osobno.
- **Master jako osobny proces** — M1 ma jeden proces; ten plan buduje stronę agenta.
- **Scoping per-konsument w `resolve`** — patrz „Odstępstwa", zamknięcie przy
  maszynie drugiej.

## Znane ryzyko, którego plan nie zamyka

`Reporter` jest `!Sync`, `shared` to `std::sync::Mutex` z `.expect("poisoned")` w 7
miejscach (`supervisor.rs:433,446,461-463,579,599,953`); `SPAWN_LOCK` broni się
przez `into_inner` (`platform/mod.rs:90-92`), mutex stanu **nie**. Step 2b omija to,
nie dotykając `shared`. **`hello`, który kiedyś będzie chciał tam zapisać, tę minę
uzbroi.** Zapisane, nierozwiązane.

## Changelog

- **Rev 1 (2026-07-16, po grumpy review opus/think-hard):**
  - **B1** verb `resolve(provider)` → **`resolve(provider, kind)`** i pole `peers`
    dostaje `AddrKind` (gateway potrzebuje 8 adresów 2 klas; `accounts` obu naraz;
    `admin` ma `edge_port: None`; zgadywanie z sufiksu klucza przywracało env jako
    autorytet). Semantyka blank-default passthrough zachowana → **polityka porażki
    per klasa**, nie globalny fail-closed.
  - **B2** zero-sharing jest **dwukierunkowe** ⇒ test klienta przeciw serwerowi
    welesa **niebudowalny**; każda strona ma atrapę, zgodność pinuje tylko Step 6.
  - **B3** Step 6 **nie może być `#[ignore]`** (blokująca stage `test` je pomija) ⇒
    musi być stage'ą ⇒ **nowy Step 5**: `weles::lock` uczy się pożyczać lease
    (inaczej deadlock na `rollout.lock` przeciw lease'owi verifyctl).
  - **H1** **kolejność 1→2a→2b→3→4→6→5(lock)→7(parity)** — dowód płaci za erozję
    bramki (design: *„each departure paid for by a live proof"*).
  - **H2** `ORCHESTRATOR_URL` dostaje `AGENT_PORT` w manifeście (jedyne miejsce,
    gdzie wolno pisać port) — usuwa konflikt `'static env_extra` vs runtime i
    problem, że `RuntimeInputs` powstaje (`:726-730`) przed bindem (`:748`).
  - **H3** mapa resolve **topology-aware** (mapa ze `split_fleet()` pod monolitem
    rozdawałaby 12 nieistniejących adresów).
  - **H4** bramka czerwona na **9** diffie (`ORCHESTRATOR_URL` present-in-weles);
    wykluczenie musi wyjść ze `strip_excluded` do `diff_env` (jedyne miejsce z
    `label`); test `exclusion_predicate_is_the_allowlist` do zmiany.
  - **H5** dowód Stepu 4 **nie może** żyć w `cmd/gateway-svc/tests/` (target
    integracyjny nie sięga `main.rs`) ⇒ `#[cfg(test)] mod` w main.rs + własny typ
    zamiast `ProcessWiring` (brak `PartialEq`, prywatne pola).
  - **H6** zakaz feature'ów tokio `process`/`signal` **mechanicznie**
    (`cargo tree -e features`), nie komentarzem — resolver-2 unifikuje feature'y
    w całym grafie, więc `weles/Cargo.toml` nie jest autorytetem.
  - **M1** Step 2 rozbity na **2a** (wyspa+cykl życia, brak precedensu w repo) i
    **2b** (verby).
  - **M2** odstępstwa od designu zapisane jawnie (brak scopingu per-konsument);
    verb zwraca **listę**, żeby nie zabetonować kształtu przed LB.
  - **L1** poprawione fakty: późny bind to **ograniczona porażka startu**, nie
    zakleszczenie (`HEALTH_DEADLINE` `:914-915`); **admin-svc bootuje ostatni**, nie
    gateway; `main.rs:117-128` to 6 peerów **+ 2 passthroughy**.
  - **L2** usunięta sprzeczność 6 vs 8 adresów (jest 8: 6 edge + 2 http).
