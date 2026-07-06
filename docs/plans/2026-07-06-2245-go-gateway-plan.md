# Plan: Gateway front-door dla Go (parytet z jvm-quarkus-sketch)

> Plan zatwierdzony i zrealizowany na branchu `feat/go-gateway` (Steps 1–5,
> commity 8983050 → 23cd255). Zachowany w repo per reguła "Plans & Status Docs — MANDATORY".

## Context

Porównanie 12-agentowe (6 delta + 6 research) wykazało, że **jedyną realną luką
funkcjonalną Go względem Quarkusa jest cała warstwa gateway/edge-routing** — reszta
jest na korzyść Go (match/rating/leaderboard, webui, prawie cały accounts, in‑process
`bus`, generyczny `outbox.Relay`). Konkretnie w Go brakuje:

1. **Prefix-routingu na `edge.Server`** — dziś `dispatch` robi wyłącznie exact-match
   `s.handlers[req.Method]` (`edge/server.go:171`). Brak tabeli prefixów / forwardu po
   rodzinie metod.
2. **Raw byte-relay na `edge.Client`** — `Call` zawsze enkoduje typowany request i
   dekoduje typowaną odpowiedź (`edge/client.go:34-76`); nie da się przekazać
   gotowych bajtów bez podwójnego enkodowania.
3. **Procesu-gatewaya** — brak `cmd/gateway-svc`, brak `httputil.ReverseProxy`
   gdziekolwiek. Split Go kończy się na 2 procesach (A=characters-svc, B=inventory-svc),
   klient musi wiedzieć który serwis odpytać. Quarkus ma 3. proces (QUIC router + HTTP
   reverse proxy) jako pojedynczy front door — sprawdzony live (`docs/reference/gateway.md`).
4. **Czego router miałby routować** — inventory nie hostuje edge servera w ogóle
   (brak pola `Edge *edge.Server`), a characters wystawia na edge tylko `characters.ownerOf`.
   Żeby udowodnić routing do **dwóch** backendów trzeba dodać po jednej player-facing
   metodzie: `characters.list` i `inventory.list` (jak w Quarkusie).

**Zakres (zatwierdzony):** tylko gateway-rdzeń. **Poza zakresem:** osobny moduł-kontrakt
`characters-api`, Stork service-discovery, docker-compose, arch-rules gatujące build,
ensure-cert (Go mintuje self-signed in-memory per proces, `edge/tls.go`, więc go nie
potrzebuje).

**Cel końcowy:** `run.ps1 microservices` startuje 3 procesy; „player client" dzwoni
wyłącznie do gatewaya (QUIC :9100 + HTTP :8082), a gateway routuje `characters.*`/
`inventory.*` do właściwego backendu i reverse-proxy'uje `/admin`,`/characters`,`/inventory`.
Padnięcie backendu = czyste `ok=false` w ograniczonym czasie, nie zwis, nie crash gatewaya.

### Spec przeniesiony 1:1 z Quarkusa (źródło prawdy: `docs/reference/gateway.md`, `EdgeRouter.kt`, `RoutedBackend.kt`, `GatewayHttpProxy.kt`)

- Router: w Quarkusie 3 tiery (**exact → payload-prefix → method-aware-prefix**),
  longest-prefix-wins. **Port Go realizuje tylko 2 z nich:** exact (już jest) +
  **method-aware-prefix-forward** (nowy). Payload-only prefix tier **pomijamy świadomie** —
  gateway potrzebuje wyłącznie forwardu po rodzinie metod; nie odtwarzamy nieużywanego tieru.
  Longest-prefix-wins w tierze forwardu przez skan liniowy. Catch-all: każdy błąd
  handlera/forwardu → `ok=false` z tekstem błędu; miss → istniejące `ok=false, "unknown method"`.
  (Zweryfikowane: `edge/server.go:184-187` już mapuje błąd handlera na `OK:false`, panika `:178-182`.)
- Raw-relay: osobna ścieżka bajty-in/bajty-out, **bez** enkodera obiektów (inaczej
  podwójne enkodowanie forwardowanego payloadu).
- `RoutedBackend` per backend: leniwy dial + jedno cache'owane połączenie, **dokładnie
  jeden** reconnect-and-retry na błąd, budżet timeoutu ~1s (krótszy niż timeout klienta),
  wszystkie tryby porażki (connect err / timeout / `ok=false` z backendu) → jeden czysty błąd.
- HTTP proxy: prefixy `/admin`,`/characters`,`/inventory` → trzy originy `host:port`,
  ścieżka przekazywana **verbatim**, brak rewrite. (`/admin` → inventory HTTP, bo admin
  żyje w inventory-svc.)
- **Uproszczenie Go vs Kotlin:** Go edge = 1 QUIC stream per request (korelacja = stream,
  brak cid). Cała maszyneria cid/pending-map/CompletableFuture z Kotlina **odpada** —
  `CallRaw` otwiera świeży stream, quic-go obsługuje współbieżne streamy, więc Go nie ma
  kotlinowego head-of-line per-connection. Nie odtwarzamy cid.

### Przydział portów (minimalizuje churn od obecnych :8080/:8081/:9000)

| Proces | HTTP | QUIC edge | zmiana |
|---|---|---|---|
| characters-svc (A) | :8080 | :9000 | bez zmian |
| inventory-svc (B) | :8081 | **:9001** | NOWY edge server |
| gateway-svc (C) | **:8082** | **:9100** (player-facing) | NOWY proces |

Nowe env-vary (konwencja jak `<NAME>_EDGE_ADDR` z `cmd/inventory-svc/main.go:26-31`):
`GATEWAY_EDGE_ADDR=:9100`, `CHARACTERS_EDGE_ADDR=localhost:9000`,
`INVENTORY_EDGE_ADDR=localhost:9001`, `CHARACTERS_HTTP_ADDR=localhost:8080`,
`INVENTORY_HTTP_ADDR=localhost:8081`. Inventory-svc dostaje `EDGE_ADDR=:9001`
(już czytane przez `app.ConfigFromEnv`, `internal/app/app.go:48`).

---

## Step 1 — `edge`: prefix-routing + raw relay  `[opus]`

**(a) Co:** `edge/server.go`, `edge/client.go` (ew. nowy `edge/router.go`).
**(b) Dlaczego teraz:** to seam, na którym stoi cały gateway — Steps 3/5 go wołają.
Musi powstać pierwszy. Zmiany są zamknięte w pakiecie `edge` (mapa `handlers`, typy
`request`/`response`, pola `Client.conn/codec` są nieeksportowane — port MUSI być tu).
**(c) Jak:**
- Nowy typ `type ForwardHandler func(method string, payload []byte) ([]byte, error)`
  (jak `Handler`, ale dostaje nazwę metody — to pozwala jednej rejestracji `"characters."`
  obsłużyć całą rodzinę pod oryginalnymi nazwami).
- `Server`: dodaj pole `prefixes []prefixEntry` (`{prefix string; fwd ForwardHandler}`)
  + metodę `HandlePrefix(prefix string, fwd ForwardHandler)`. Rejestracja przed `ListenAddr`
  (jak `Handle`, nie thread-safe względem Serve — zgodne z istniejącym kontacktem `server.go:48`).
- `dispatch` (`server.go:165-189`): po chybieniu exact-matcha (`handlers[req.Method]`),
  skan `prefixes` filtrujący `strings.HasPrefix(req.Method, e.prefix)`, wybór **najdłuższego**
  prefixu; wywołaj `fwd(req.Method, req.Payload)`; wynik/err mapowany tą samą ścieżką co
  dziś handler exact (err → `response{OK:false, Error:...}`). Zachowaj recover z paniki.
  Miss w obu → istniejące `response{OK:false, Error:"edge: unknown method %q"}`.
- `Client.CallRaw(ctx context.Context, method string, payload []byte) ([]byte, error)`:
  buduje `request{Method: method, Payload: payload}` **bez** `codec.Encode(req)` na payloadzie
  (payload to już surowy JSON `json.RawMessage`), otwiera stream (`OpenStreamSync`),
  `writeFrame`, `readFrame`, dekoduje kopertę `response`; `!OK` → `errors.New(env.Error)`;
  zwraca `env.Payload` **verbatim** (bez `Decode`). Wzoruj na `Call` (`client.go:34-76`),
  pomijając encode requestu i decode odpowiedzi.
**Testy (w tym samym kroku):** `edge/router_test.go` — round-trip: rejestr prefixu
forwardującego do drugiego `edge.Server`, sprawdź longest-prefix-wins, exact wygrywa nad
prefixem, `CallRaw` nie podwaja enkodowania (bajty na wyjściu == bajty wejściowe handlera).
**Zweryfikowane założenia:** `request.Payload`/`response.Payload` to `json.RawMessage`
(`edge/wire.go:9,17`), codec = `json.Marshal/Unmarshal` (`edge/codec.go:16-18`) → relay
naprawdę bez podwójnego enkodowania. Współbieżność: `OpenStreamSync` na współdzielonym
`*quic.Conn` jest bezpieczne — jeden cache'owany `edge.Client` bity przez wiele wejściowych
goroutine gatewaya nie head-of-line-blokuje (dlatego cid zbędny).

## Step 2 — inventory dostaje edge server; `characters.list` + `inventory.list`  `[sonnet]`

**(a) Co:** `modules/inventory/inventory.go`, `cmd/inventory-svc/main.go`,
`modules/characters/characters.go`.
**(b) Dlaczego teraz:** router z Step 1 potrzebuje realnych metod na **dwóch** backendach,
zanim Step 3 je zrejestruje jako prefixy. Bez tego nie ma czego routować/testować.
**(c) Jak — dokładny mirror wzorca `characters.ownerOf` (`characters.go:117-120,143-168`):**
- **inventory** (dziś BEZ edge): dodaj eksportowane pole `Edge *edge.Server` do struktury
  (`inventory.go:37-42`) + import `gamebackend/edge`. W `Register` zatrzymaj `m.svc *service`
  zamiast anonimowego literału (`inventory.go:87-91`: `m.store=...; m.svc=&service{store:m.store};
  registry.Provide(ctx.Registry,"inventory",m.svc)`). W `Init` dodaj
  `if m.Edge != nil { m.Edge.Handle("inventory.list", inventoryListEdgeHandler(m.svc)) }`.
  Handler dekoduje `listReq{PlayerID string}`, woła **przez serwis** (wierny mirror
  `ownerOfEdgeHandler`, który idzie przez `svc.OwnerOf`, nie przez store)
  `m.svc.List(ctx, Owner{Type:"player", ID: req.PlayerID})` (`inventory.go:320-322`),
  marshaluje `listResp{Items []Holding}`.
- **cmd/inventory-svc/main.go** (`:47-67`): `srv := edge.NewServer()`,
  `im := &inventory.Module{Edge: srv}`, przekaż `srv` jako 3. arg `app.Run(cfg, mods, srv)`
  (dziś `nil`). EDGE_ADDR ustawia run-script na `:9001`.
- **characters**: obok `ownerOf` w `Init` dodaj
  `m.Edge.Handle("characters.list", charactersListEdgeHandler(m.svc))`; handler dekoduje
  `listReq{PlayerID}`, woła **przez serwis** `m.svc.ListByPlayer(ctx, pid)` (istnieje,
  `characters.go:312-314`; ten sam co używa `handleList`), marshaluje `listResp`.
- **`.go-arch-lint.yml` (C1 — obowiązkowe, inaczej Step 5 `go-arch-lint check` czerwony):**
  dziś `inventory: { mayDependOn: [ lifecycle, contracts ] }` (`:93`) — dodaj `edge`
  (jak `characters:` `:92`); w `cmdInventorySvc.mayDependOn` (`:134-140`) dodaj `edge`
  (jak `cmdCharactersSvc:` `:127-133`). Bez tego oba nowe importy `gamebackend/edge` to
  twarde naruszenia architektury.
- DTO-y wire mirror'owane po stronie klienta gatewaya **nie są potrzebne** — gateway
  forwarduje surowe bajty (`CallRaw`), nie zna kształtu. To zaleta raw-relay.
**Uwaga (świadome uproszczenie sketcha):** edge `*.list` ufają `player_id` z payloadu
(brak weryfikacji sesji na edge) — dokładnie jak `ownerOf` ufa `id` i jak player-facing
metody w Quarkusie. Odnotowane, nie regresja.

## Step 3 — pakiet `gateway/` + `cmd/gateway-svc`  `[opus]`

**(a) Co:** **nowy** pakiet root-level `gateway/` (rodzeństwo `edge/`/`outbox/` — foundation,
NIE `core.Module`) z `gateway/routed_backend.go`, `gateway/httpproxy.go` + nowy
`cmd/gateway-svc/main.go`. **Uwaga:** root `gateway/` w repo Go **nie istnieje** (zweryfikowane
absolutną ścieżką — wcześniejsze „puste gateway/" było artefaktem dryfu cwd). Nazwa wolna;
Kotlinowy `gateway/` żyje pod `experiments/jvm-quarkus-sketch/`, nie koliduje.
**(b) Dlaczego teraz:** spina Step 1 (router/CallRaw) i Step 2 (metody backendów) w proces.
Musi być po nich.
**(c) Jak:**
- `gateway.RoutedBackend` — implementuje `edge.ForwardHandler`. Pola: `peerAddr string`,
  cache `*edge.Client` (mutex, leniwy `edge.Dial(ctx, addr, edge.ClientTLS())`).
  `Forward(method string, payload []byte) ([]byte, error)`: **budżet per-attempt** —
  `context.WithTimeout(context.Background(), 1s)` (`gateway.forwardBudget`) na **każdą**
  próbę, `CallRaw`; na błąd — invaliduj cache (re-dial) i **jeden** retry (świeży ctx/timeout);
  drugi błąd → zwróć błąd (dispatch zamieni na `ok=false`). Worst case ~2×1s → Step 5 bound
  „~≤3s" jest spójny. **Mirror CAŁEGO `edgeConn` (`modules/remote/remote.go:89-153`),
  nie tylko `call()`** — bezpieczeństwo współbieżne siedzi w identity-guarded `reset`
  (`:114-121`, `if e.client == failed`) + `get`/`close` (`:96-132`); mirror samego `call`
  (:138-153) zgubiłby guard i wróciłby close-race/thundering-herd re-dial przy jednym
  współdzielonym kliencie i wielu wejściowych połączeniach. Różnica vs `edgeConn`: `CallRaw`
  (bajty) zamiast typowanego `Call`.
- `gateway.NewHTTPProxy(routes map[string]string) http.Handler` — `http.ServeMux` z
  wzorcami `/admin/`,`/characters/`,`/inventory/`, każdy →
  `httputil.NewSingleHostReverseProxy(&url.URL{Scheme:"http", Host: origin})`
  (ścieżka zachowana verbatim, origin bez base-path). `/admin` → `INVENTORY_HTTP_ADDR`.
- `cmd/gateway-svc/main.go` — **NIE** używa `internal/app.Run` (gateway jest bezstanowy:
  brak DB — `app.Run` pinguje DB i wymagałby Postgresa; brak modułów, brak bus). Slim boot
  mirrorujący *kolejność* shutdown z `app.go:162-184`:
  1. `chars := &gateway.RoutedBackend{peerAddr: env CHARACTERS_EDGE_ADDR}`, `inv := ...INVENTORY_EDGE_ADDR`.
  2. `srv := edge.NewServer(); srv.HandlePrefix("characters.", chars.Forward); srv.HandlePrefix("inventory.", inv.Forward)`.
  3. `srv.ListenAddr(GATEWAY_EDGE_ADDR, tls)` — TLS: `edge.SelfSignedTLS()` (jak backendy).
  4. `mux := http.NewServeMux(); mux.HandleFunc("GET /healthz", ...200)`;
     mux montuje `gateway.NewHTTPProxy({...})` na trzech prefixach;
     `http.Server{Addr: PORT, Handler: mux, ReadHeaderTimeout: 10*time.Second}`
     — **`ReadHeaderTimeout` obowiązkowy** (gosec G112/Slowloris; `app.go:153` ma go z tego
     samego powodu, a Step 5 odpala `golangci-lint`).
  5. `signal.Notify SIGINT` → `httpSrv.Shutdown` → `srv.Close` → zamknij backendy.
- **`.go-arch-lint.yml` (M1 — obowiązkowe, DWA komponenty + deps):** dodaj do `components:`
  `gateway: { in: gateway }` oraz `cmdGatewaySvc: { in: cmd/gateway-svc }`; do `deps:`
  `gateway: { mayDependOn: [ edge ] }` oraz `cmdGatewaySvc: { mayDependOn: [ gateway, edge ] }`.
  (Komentarz przy `gateway` w stylu `edge`/`remote`: transport-level, nie impl modułu.)
**Uwagi altitude / świadome uproszczenia (nie regresje):**
- `RoutedBackend` to uogólniony `remote.edgeConn` na bajty; nie ruszamy prywatnego
  `remote.edgeConn` — mała, świadoma duplikacja wzorca retry (wspólny helper poza zakresem).
- `ForwardHandler` nie dostaje `ctx` (spójne z istniejącym `Handler`) → `Forward` fabrykuje
  własny `context.Background()`; rozłączenie playera w trakcie **nie** anuluje forwardu wyjściowego.
  Akceptowalne dla sketcha, odnotowane.
- `HandlePrefix("characters.", …)` routuje też wewnętrzne `characters.ownerOf` — nieszkodliwe
  (ten sam backend), ale prefix jest szerszy niż jedna metoda player-facing.

## Step 4 — run scripts: topologia 3-procesowa  `[sonnet]`

**(a) Co:** `run.ps1`, `run.sh` (bliźniacze).
**(b) Dlaczego teraz:** dopiero gdy `cmd/gateway-svc` istnieje (Step 3) można go zbudować/uruchomić.
**(c) Jak:** dodaj `bin/gateway-svc` do listy budowania. W gałęzi `microservices`
(`run.ps1:177-222` / `run.sh:136-173`): do env procesu B dodaj `EDGE_ADDR=:9001`;
**po** zdrowych A i B wystartuj **C=gateway** (sekwencyjnie, jak A→B) z env
`PORT=8082, GATEWAY_EDGE_ADDR=:9100, CHARACTERS_EDGE_ADDR=localhost:9000,
INVENTORY_EDGE_ADDR=localhost:9001, CHARACTERS_HTTP_ADDR=localhost:8080,
INVENTORY_HTTP_ADDR=localhost:8081`; health-check `http://localhost:8082/healthz`;
dopisz do `run/pids.json`/`pids` i logów (`run/gateway.out.log`). Teardown iteruje pliki
pid — bez zmian. Zaktualizuj banner (player front door: QUIC :9100 / HTTP :8082).

## Step 5 — live smoke + weryfikacja  `[opus]`

**(a) Co:** `gateway/live_smoke_test.go` (guard build-tag `//go:build live` lub env),
ew. `edge/router_test.go` już w Step 1.
**(b) Dlaczego teraz:** live 3-proces łapie to, czego loopback nie może (w Quarkusie
wyłapał 2 realne bugi integracyjne). Ostatni, bo wymaga całości.
**(c) Jak — mirror sprawdzonego scenariusza (`docs/reference/gateway.md:57-64`):**
- Start 3 procesów (lub in-proces: dwa `edge.Server` jako backendy + gateway `edge.Server`
  + `RoutedBackend` na nie). „Player client" (`edge.Dial` na :9100) woła
  `characters.list` → trafia do characters, `inventory.list` → do inventory: dowód
  routingu do **różnych** backendów.
- **Graceful degradation:** ubij characters backend → `characters.list` zwraca `ok=false`
  w czasie ograniczonym (2 próby × 1s budżet per-attempt, worst ~≤3s; martwy backend
  odrzuca dial szybciej), gateway żyje, `inventory.list` działa dalej.
- HTTP: `GET :8082/admin` → 200 przez reverse-proxy do inventory.

**Weryfikacja end-to-end:** `go build ./...`, `go test ./edge/... ./gateway/... ./cmd/gateway-svc/...`,
`go vet ./...`, `golangci-lint run ./...`, `go-arch-lint check` — **teraz zielony**, bo
edycje `.go-arch-lint.yml` z Step 2 (C1: `edge` dla inventory/cmdInventorySvc) i Step 3 (M1:
komponenty `gateway`+`cmdGatewaySvc`) są częścią planu. Następnie ręcznie: `./run.ps1 microservices`,
scenariusz smoke z player-clientem, potwierdzić degradację ubijając characters-svc.

---

## Dispatch (do zatwierdzenia z planem)

| Step | Lane | Uzasadnienie |
|---|---|---|
| 1 edge seam | `[opus]` | seam correctness-critical (routing dispatch, raw relay), nieeksportowane typy |
| 2 inventory edge + list | `[sonnet]` | wierny mirror wzorca `characters.ownerOf`, w pełni wyspecyfikowany |
| 3 gateway + cmd | `[opus]` | nowy design: routing, degradacja, budżety timeoutu, slim boot |
| 4 run scripts | `[sonnet]` | mechaniczna konfiguracja/env, bliźniacze skrypty |
| 5 live smoke | `[opus]` | subtelne asercje degradacji/bounded-time |

Trailery Co-Authored-By: `[opus]`→Claude Opus 4.8, `[sonnet]`→Claude Sonnet 4.6.
Commit po każdym Stepie (Conventional Commits, scope `edge`/`gateway`/`inventory,characters`/`chore`).

## Ryzyka / decyzje otwarte

- **Znany limit v1 (jak w Quarkusie, świadomie):** w Kotlinie forward blokuje per-connection.
  W Go stream-per-call daje więcej współbieżności za darmo — nie „naprawiamy" tego cicho,
  ale i nie dodajemy sztucznego limitu.
- **`go-arch-lint` (rozstrzygnięte, nie ryzyko):** wymaga edycji w Step 2 (C1) i Step 3 (M1),
  obie wyspecyfikowane. `gateway`/`cmdGatewaySvc` to komponenty transportowe (jak `edge`/`remote`),
  nie `core.Module`. `inventory`+`cmdInventorySvc` dostają `edge`. Pominięcie któregokolwiek =
  twarde naruszenie na `go-arch-lint check`.
- **Reverse proxy `/admin`:** w splicie admin żyje w inventory-svc → origin = `INVENTORY_HTTP_ADDR`.
  Gdyby admin się przeniósł, to jedna zmiana mapy w Step 3.
