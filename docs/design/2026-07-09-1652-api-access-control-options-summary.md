# API access control — opcje (przed planowaniem)

Stan: research zrobiony (3 równoległe subagenty: gateway/edge enforcement, model
tożsamości w accounts, wzorce config/admin/sloty). Decyzja użytkownika: TBD —
planowanie ruszy dopiero na wyraźne hasło.

## Wymaganie

Konfigurowalny dostęp do API per klasa wołającego: untrusted clients (gra u
gracza) dostają ograniczony zestaw API, trusted servers pełny dostęp. Docelowo
konfigurowalne w runtime, nie tylko w kodzie.

## Stan obecny (fakty z kodu)

- `opsapi::Identity` = `Option<player_id>` (`core/opsapi/src/lib.rs:59`) — jedyna
  informacja o wołającym w całym stacku.
- `AuthReq` na operacji ma 2 stany: `None` | `Player` (`core/opsapi/src/lib.rs:224`).
  Brak pojęcia klasy klienta / roli / scope'a gdziekolwiek.
- Jeden punkt egzekwowania: gateway `dispatch_matched_op` (HTTP,
  `modules/gateway/src/lib.rs:572`) i `handle_player` (player-QUIC, `:280`) — oba
  mają w ręku pełną `Operation` + zweryfikowaną `Identity`.
- HTTP i player-QUIC eksponują identyczny zbiór opów (RouteTable budowany z tych
  samych slotów `opsapi::SLOT`/`BINDING_SLOT`/`LOCAL_SLOT`/`PEER_SLOT`); nie da
  się wystawić opa tylko na jednym planie.
- `accountsapi::Sessions::verify_session` zwraca goły `Option<String>`; tabela
  `accounts.sessions` to token/player/timestamps — sesje z dev-auth i Epic są
  nierozróżnialne (provider ginie przy `issue_session`).
- Machine credentials nie istnieją: zero API keys/service tokens; wewnętrzny mTLS
  edge to zaufanie binarne (wszystkie certy mają CN `gamebackend-edge-leaf`, każdy
  peer może wywołać każdą metodę edge'a).
- `config.settings` to skalarne stringi (ns,key)->value z LISTEN/NOTIFY
  live-reload; macierz uprawnień na tym nie jeździ wygodnie, ale wzorzec
  (live-reload + durable `*.changed` + `CachedX` dla splitu) jest do skopiowania.
- Precedens gatowania opów: `ACCOUNTS_DEV_AUTH` / `INVENTORY_DEV_GRANT` — env-gate
  w `init` decydujący czy op w ogóle jest skontrybuowany; statyczne, per-proces,
  nie per-caller.
- **Gap znaleziony przy okazji:** `POST /events` (durable plane,
  `core/asyncevents/src/lib.rs:322`) montowany na zwykłym HTTP routerze bez
  żadnej autoryzacji — polega czysto na topologii sieci. Do domknięcia niezależnie
  od wybranej opcji.

## Wspólny fundament (potrzebny w każdej opcji)

1. Poszerzenie `Identity`: `Anonymous | Player(id) | Service(name)` (addytywnie —
   nic poza `player_id()` dziś nie konsumuje `Identity`, churn mały).
2. Poszerzenie `AuthReq` ponad `None|Player` (co najmniej `Service`; ew. tiery).
3. Wzbogacenie zwrotki weryfikatora (`SessionVerifier::verify` /
   `accountsapi::Sessions`) tak, by niosła klasę wołającego, nie tylko player_id.
4. Decyzja: jak uwierzytelnia się „trusted server” (patrz niżej).

## Opcje — gdzie mieszka polityka

### Opcja 1 — Deklaratywna audiencja (statyczna)

Każdy op deklaruje w `#[http(...)]` minimalną klasę wołającego
(`Public`/`Player`/`Service`); pole płynie przez `Operation` (nowe pole obok
`auth`) do gatewaya, który egzekwuje w dispatchu — automatycznie na obu planach.

- Zakres: `core/opsapi` (pole), `tools/rpc-macro` (atrybut), gateway (check),
  adnotacje na istniejących opach.
- ✅ najtańsze; polityka wersjonowana z kodem; zero nowych modułów/DB.
- ❌ zmiana polityki = rekompilacja — nie spełnia „konfigurowalne” wprost.
  To fundament, nie całość ficzera.

### Opcja 2 — Nowy moduł `policy` (pełny runtime)

Nowa forteca `modules/policy` + `api/policy`: własna tabela (macierz op ×
klasa wołającego), live-reload wzorem configu (LISTEN/NOTIFY + durable
`policy.changed` + `CachedPolicy` w `policyrpc` dla gateway-svc), strona w
adminie do edycji, gateway `requires("policy")` i pyta `policyapi::Policy`
przy każdym dispatchu (z cache w pamięci — bez I/O per request).

- ✅ pełna konfigurowalność runtime + admin UI; czysty Open/Closed (nowy kod);
  działa w obu topologiach istniejącym wzorcem stub/CachedX.
- ❌ największy zakres; trzeba świadomie rozstrzygnąć fail-mode gdy wpisu brak
  (fail-open vs fail-closed) i bootstrap (pusta tabela = co?).

### Opcja 3 — Hybryda: deklaratywny default + runtime override (REKOMENDACJA)

Op deklaruje w kodzie bezpieczny default (Opcja 1 — potrzebna i tak, żeby nowy
op nie rodził się otwarty), a moduł `policy` (okrojona Opcja 2; admin UI może
przyjść w drugiej iteracji) trzyma tylko odstępstwa: „przymknij X dla
untrusted”, „otwórz Y dla service”. Brak wpisu = default z kodu → fail-mode
naturalnie bezpieczny, macierz w DB mała.

- ✅ konfigurowalne tam gdzie trzeba; bezpieczne defaulty w kodzie; dowożalne
  etapami (fundament → enforcement → policy module → admin page).
- ❌ dwa źródła prawdy przy debugowaniu (kod + DB) — mityguje strona w adminie
  pokazująca politykę *efektywną*.

### Odrzucone ścieżki (i dlaczego)

- **Rozszerzenie modułu `config`** — skalarne stringi; macierz wymagałaby
  syntetycznych kluczy bez walidacji/enumeracji. Kopiujemy wzorzec, nie dane.
- **`httpmw::LAYER_SLOT` (middleware)** — siedzi poza rozwiązaniem route→op
  (gateway dispatchuje przez fallback handler), nie widzi `Operation` ani
  `Identity`, i w ogóle nie dotyka player-QUIC.
- **Env-gate per op (wzór `INVENTORY_DEV_GRANT`)** — per-proces i statyczne,
  nie per-caller, nie runtime.

## Sub-decyzja — credential zaufanego serwera

- **(a) Service tokens / API keys** — nowa tabela `accounts.service_tokens`,
  zwykły bearer przez gateway, weryfikator rozpoznaje typ tokena i zwraca
  `Identity::Service(name)`. Wpasowuje się 1:1 w istniejący bearer-once-at-gateway;
  działa też dla serwerów spoza naszej infry. (Rekomendacja na start.)
- **(b) Per-peer mTLS identity** — rozróżnialne certy (SAN per serwis) na
  wewnętrznym edge'u; wymaga przebudowy `DevCA`; tylko dla peerów w naszej
  infrze; nie rozwiązuje „zewnętrzny trusted server woła publiczne API”.
- **(c) Oba, etapami** — tokeny w tym ficzerze; per-peer mTLS później, razem z
  domknięciem niezabezpieczonego `/events`.

## Decyzje użytkownika (2026-07-09, po dyskusji)

Model docelowy: **policy per API key** (à la Supabase anon/service key). Bez
RBAC per user — to na razie mija się z celem.

- KAŻDY klient dostaje klucz: gra u gracza ma wkompilowany klucz
  „untrusted-client" (wąska lista API), zaufany serwer klucz „full".
- Klucz identyfikuje KLASĘ KLIENTA i niesie politykę (lista dozwolonych
  metod albo `full`); klucz kliencki nie jest sekretem (wyciągalny z binarki)
  — i to OK, bo per-gracz dalej autoryzuje sesja.
- Request = API key (obowiązkowy, identyfikuje aplikację) + opcjonalnie
  session bearer (jak dziś, dla opów per-gracz). Gateway w dispatchu:
  (1) polityka klucza dopuszcza metodę, (2) jeśli op wymaga gracza — sesja.
- Polityka per klucz w DB → konfigurowalna bez deployu; docelowo admin page.
- Klucz serwerowy = prawdziwy sekret, woła wszystko bez sesji.

Do rozstrzygnięcia przy planowaniu: header na klucz (`X-Api-Key` obok
bearera?), pole na klucz w player-QUIC envelope (dziś tylko `token`),
seedowany klucz dev dla localhost/split-proof/playercli, gdzie mieszka
tabela kluczy (accounts vs osobny moduł), live-rewokacja w splicie
(per-request DB vs cache z invalidacją — napięcie jak w config).

## Otwarte pytania do decyzji użytkownika

1. Wybór opcji 1/2/3 (rekomendacja: 3).
2. Credential trusted-serverów: a/b/c (rekomendacja: a, ew. c).
3. Ile klas wołających? Minimalnie `Anonymous < Player < Service`; czy potrzebny
   dodatkowy tier „trusted client” po stronie graczy?
4. Czy per-plane visibility (op tylko na HTTP, nie na player-QUIC) wchodzi w
   zakres tego ficzera, czy osobno?
5. `/events` bez auth — domykać w tym ficzerze czy jako osobny task?
