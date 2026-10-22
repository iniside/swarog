# Eksploracja: hostowany Lore jako moduł Swaroga

Data: 2026-10-22. Etap: wyłącznie eksploracja, bez implementacji. Źródła Lore:
`G:\tmp\lore-latest` (HEAD `cb5b536`). Ścieżki Lore poniżej są względne do tego
katalogu; ścieżki Swaroga są względne do korzenia repo.

Metoda: pięć równoległych subagentów read-only (kontrakt auth Lore; runtime/locki/hooki
Lore; reuse accounts/apikeys/groups; model procesów + admin Swaroga; inwentarz
`player_id`), synteza i weryfikacja kluczowych twierdzeń w głównym kontekście
(targeted read). rust-analyzer nie był dostępny dla `.rs` w tej sesji, więc
wszystkie liczniki i stwierdzenia „brak X" są **dolną granicą z Grep**, nie dowodem.

## Najważniejsze ustalenia

1. **Serwer Lore już dziś przyjmuje tokeny zewnętrznego wystawcy, klient — nie.**
   Strona serwerowa LEP OIDC (status `Approved`,
   `docs/proposals/2026-08-20-oidc-oauth2-authentication.md:6`) jest w dużej mierze
   zaimplementowana: discovery `jwks_uri`, tolerancja claimów, kontrola `typ`,
   `GlobalGrantsAuthorizer` (Tier 1), `ResourceGrantsAuthorizer` (Tier 2),
   `RepositoryCatalog`, `[server.auth.oidc]`. Klient (`lore-transport`) ma tylko
   `UcsAuthentication`, mówiący gRPC `UrcAuthApi`, zarejestrowany pod schematami
   `ucs-auth`/`https`/`http` (`lore-transport/src/auth/ucs_auth.rs:25`). Device grant,
   client credentials, refresh i discovery po stronie klienta (fazy 3–4 LEP) nie
   istnieją w drzewie.
2. **Dopóki klient potrzebuje `auth_url`, serwer nie użyje Tier 1.**
   `select_repository_authorizer` (`lore-server/src/authnz/repository_authorizer.rs:479-504`)
   wybiera `AuthClient` zawsze, gdy ustawiony jest `auth_url`. To oznacza
   **online `CheckUserPermission` na każde sprawdzenie dostępu**. Zatem faza
   adaptera to nie „Tier 1 + adapter logowania”, tylko pełny tryb `UrcAuthApi`:
   Swaróg musi obsłużyć logowanie, wymianę na token partycji, sprawdzanie uprawnień,
   katalog repozytoriów i `RebacApi`. Tier 1/Tier 2 z claimów włącza się dopiero
   po usunięciu `auth_url`, czyli gdy klient Lore dostanie fazę 3.
3. **Swaróg nie implementuje żadnego traitu `authnz` w procesie.** Stockowy
   `loreserver` nie ma rejestru wtyczek dla autoryzacji. Plugin registry obejmuje
   tylko store/topology/lock/notification (`lore-server/src/plugins/traits.rs:42-244`).
   Swaróg dostarcza **przewodowe odpowiedniki**, wymienione w §2.4.
4. **Stockowy `loreserver` nie ma hooków.** `ServerConfig::default()` ma pusty
   rejestr wtyczek i zero callbacków hooków (`lore-server/src/server_config.rs:36-58`).
   Hooki to domknięcia Rust wkompilowane w pochodną binarkę. Bez forka jedynym
   źródłem zdarzeń jest strumień gRPC `NotificationService.Subscribe` (per
   repozytorium, w pamięci, gubi zdarzenia przy lagu, bez `RepositoryCreate`/`Obliterate`).
   Decyzja dla właściciela: **P2**.
5. **Locki Lore żyją w RAM** (`LocalLockStore`, `DashMap`,
   `lore-server/src/lock/store.rs:26-29`) i giną przy restarcie. Odczyt i zdejmowanie
   są możliwe bez forka przez `urc.lock.LockService` (`Query`/`Unlock`) z tokenem
   niosącym `owner`/`admin`.
6. **Lore nie raportuje zajętości per repozytorium.** Fragmenty są
   content-addressed i deduplikowane. Billing TB wymaga modelu „instancja per
   studio” albo własnego pomiaru. Decyzja: **P4**.
7. **Refaktor `player_id → user_id` w kształcie z briefu łamie dwie reguły
   MANDATORY/hard constraint.** „Nowe migracje” i „serde alias w eventach” kolidują
   z **No Dual-Write, No Topology Branch, Wipe** oraz z Hard constraint #6. Decyzja: **P1**.

## 1. Pytania do właściciela (blokują kształt planu, nie eksplorację)

Każde to jedno pytanie. Raport nie przyjmuje po cichu żadnej z opcji.

- **P1 — `player_id → user_id` a zasada wipe.** Brief każe zmienić kolumny „przez
  nowe migracje” i dać „serde alias w eventach”. Reguły repo mówią: `migrate` to
  wyłącznie idempotentne `CREATE … IF NOT EXISTS`, zmiana schematu to edycja DDL
  w miejscu + DROP + świeży boot, a „kept for compatibility” pola są zakazane
  (`.agents/shared/core-rules.md` **No Dual-Write…**). Zmiana kształtu payloadu
  eventu to nowa wersja kontraktu i nowe id subskrypcji
  (`.agents/shared/gamebackend.md` Hard constraint #6). `#[serde(alias)]` nie
  występuje nigdzie w repo (Grep, dolna granica). Pytanie: czy refaktor ma iść
  w kształcie wipe? Czyli DDL w miejscu + wipe, a payloady jako `define(topic, 2, …)`
  z nowymi `*.v2` id albo w miejscu z re-bless baseline'ów, bez aliasu. Czy brief
  świadomie uchyla tu regułę wipe?
- **P2 — źródło zdarzeń Lore.** Do wyboru jest stockowy `loreserver` +
  konsument `NotificationService.Subscribe`: stratny, per repo, bez create/obliterate.
  Alternatywą jest pochodna binarka (`server_main(ServerConfig{ hook_registration_callbacks, … })`)
  z hookiem `post_handler` wysyłającym do Swaroga. Ta ma pełne hook pointy, ale nie
  jest już „stockowa” i wymaga budowania Lore. Weles nigdy nie buduje, więc artefakt
  pochodzi z osobnego builda.
- **P3 — model tenanta „studio”.** W `modules/` nie ma pojęcia organizacji, tenanta
  ani studia (Grep `tenant|organization|org_id|studio|workspace_id|project_id`: 0
  trafień). `groups` to płaskie grupy społecznościowe. Do wyboru: studio jako
  encja w nowym module `lore`, studio jako grupa w `groups` z rolą „admin” albo
  osobny moduł `studios`.
- **P4 — instancja per studio czy współdzielona.** Instancja per studio daje izolację
  locków i store'u, a TB to rozmiar katalogu store danej instancji. Współdzielony
  serwer z partycjami nie ma pomiaru TB i współdzieli locki w RAM.
- **P5 — logowanie członka studia.** `accounts` ma dziś `dev` (argon2, tylko za
  `ACCOUNTS_DEV_AUTH`), `epic`, `google`, `guest`. Nie ma produkcyjnego
  hasła/e-maila. Jakim dostawcą loguje się człowiek na stronie zatwierdzania
  logowania (device approval)? Google federated jest najbliżej gotowego.

### Decyzje właściciela (2026-10-22)

- **P1 — wipe.** DDL edytowany w miejscu, zmiany payloadów i id subskrypcji w miejscu
  z re-bless baseline'ów i golden. Bez nowych migracji, bez `serde(alias)` i bez
  tematów v2: reguła wipe obejmuje wprost także zmianę kontraktu eventu. Na maszynie
  roboczej nie ma jeszcze Postgresa, więc nie ma danych do wipe'u.
- **P2 — wariant A.** Stockowy `loreserver`. Moduł `lore` subskrybuje
  `NotificationService.Subscribe` per repo i przenosi zdarzenia na trwałe tematy
  `lore.*`. Pochodna binarka z hookami czeka, aż pojawi się potrzeba weta w Tier 1
  albo zdarzenia obliteracji.
- **P3/P4 — studio to cały deployment, zero-sharing.** Nowe studio dostaje pełny,
  osobny deployment Swaroga z jednym `loreserver`, bez żadnego powiązania z innymi
  studiami. To nie jest SaaS z wieloma tenantami w jednej instalacji. Konsekwencje:
  - w kodzie **nie ma pojęcia tenanta ani modułu `studios`**: użytkownicy studia to
    konta w `accounts` tego deploymentu, a role nadaje moduł `lore`;
  - jeden deployment ma dokładnie jeden `loreserver`, więc Tier 1 (akcje globalne
    w tokenie) jest naturalnym modelem, a `aud` wskazuje tę jedną instancję;
  - billing liczy się per deployment: seaty to użytkownicy z dostępem do Lore, a TB to
    rozmiar store'u tej instancji; moduł `lore` jedynie raportuje te liczby;
  - zakładanie nowego studia, czyli provisioning całego deploymentu (osobna baza
    danych, osobny root Welesa, porty, TLS), dzieje się poza deploymentem i jest
    osobnym, późniejszym tematem.
- **Uprawnienia Lore — moduł `lore`, per repo, edytuje operator.**
  - Moduł `lore` jest właścicielem ról, przypisań użytkowników, wyjątków per repo i
    listy chronionych branchy. Osobny moduł `access` powstaje dopiero wtedy, gdy
    pojawi się drugi system, który z nich korzysta.
  - Uprawnienia są **per repo**, więc docelowym modelem jest **Tier 2** (token per
    partycja z claimem `resources`), a nie Tier 1. W fazie adaptera uprawnienia per
    repo działają od razu przez online `CheckUserPermission`.
  - Edycja to zakładka w obecnym portalu admin (operator deploymentu, konta
    `admin.users`). Panelu dla leada studia na zwykłym koncie nie budujemy: uprawnienia
    w portalu wymagałyby projektu od zera.
- **P5 — dostawca niezależny.** Strona zatwierdzania logowania (device approval)
  używa tego dostawcy, którego skonfigurowano w `accounts`. Do zbudowania są
  produkcyjna samorejestracja (e-mail i hasło; dziś `dev` działa tylko za
  `ACCOUNTS_DEV_AUTH`) oraz konto zakładane przez admina. To kroki, a nie blokery.

## 2. Kontrakt claimów, którego oczekuje `loreserver`

### 2.1 Co serwer weryfikuje dziś (zaimplementowane)

- Struktura `AuthorizationToken` (`lore-server/src/auth/jwt.rs:126-155`):
  - wymagane `sub`, `iss`, `iat`, `exp`, `aud` (string albo tablica);
  - opcjonalne `name`, `preferred_username`, `client_id`, `env`, `idp`, `groups`,
    `resources`, `is_service_account`;
  - reszta trafia do spłaszczonego `extra`, czytanego ścieżką kropkową (`claim_at`, `:169`).
- Weryfikacja (`jwt.rs:315-389`): algorytm brany z JWK, a nie z nagłówka.
  Sprawdzane są `iss` ∈ `jwt_issuer`, `aud` ∩ `jwt_audience` oraz `exp`. `typ` jest
  sprawdzany tylko gdy ustawiono `jwt_typ` (`:352-367`). Claim tożsamości
  (`identity_claim`, domyślnie `sub`) musi być niepustym stringiem.
- JWKS (`lore-server/src/auth/jwk.rs`):
  - cache po `kid`, odświeżanie nie częściej niż co 10 s;
  - limit odpowiedzi 1 MiB, timeouty 5 s i 10 s;
  - discovery `<jwt_issuer>/.well-known/openid-configuration` bez redirectów,
    z warunkiem `document.issuer == issuer`; `jwks_uri` musi być https, gdy issuer jest https;
  - klucz nie-RSA **musi mieć `alg`**, inaczej jest odrzucany (`:183,216-226`).
    **Wymóg dla Swaroga: JWKS ES256 publikuje `alg: "ES256"` i `kid`.**
- gRPC: brak `Bearer` → `unauthenticated`; błąd weryfikacji → `permission_denied`
  (`jwt_interceptor.rs:25-85`). Bez `[server.auth]` wszystko przechodzi.

### 2.2 Tier 1 (docelowy, po fazie 3 klienta Lore)

Konfiguracja: `[server.auth]` bez `auth_url` i bez `resource_claim`, czyli
`GlobalGrantsAuthorizer`. Token access, który wystawia Swaróg:

| Claim / nagłówek | Wartość |
|---|---|
| nagłówek `alg`, `kid`, `typ` | `ES256`, id klucza, `at+jwt` (wtedy w Lore `jwt_typ = "at+jwt"`) |
| `iss` | URL wystawcy, identyczny z `jwt_issuer` i z `issuer` w discovery |
| `aud` | zawiera wartość z `jwt_audience`. Ze względu na D5 klienta (send-check) **powinno też zawierać URL/domenę serwera Lore** albo osobny claim `root_domains` / `https://lore.org/claims/root_domains` (LEP:708-751). Inaczej klient nie wyśle tokenu do serwera. |
| `sub` | stabilne id użytkownika Swaroga (dziś `accounts.players.id`, uuid). Lore zapisuje je w historii jako `created_by`. |
| `iat`, `exp` | standardowe; `jti` i `client_id` opcjonalne |
| claim akcji wskazany przez `permission_claim` | tablica akcji **globalnych**, np. `["owner","admin","migrate","obliterate","push-protected","presign"]` (słownik: LEP:700-703, 1026-1027) |
| `name`, `preferred_username` | opcjonalne, do wyświetlania |

Tier 1 jest globalny per akcja: każdy uwierzytelniony podmiot sięga do każdej
partycji serwera. Przy modelu „instancja per studio” (P4) to dokładnie granica
tenanta, a role z `groups` mapują się na zbiór akcji.

### 2.3 Tier 2 (opcjonalny, per partycja)

`resource_claim` ustawiony daje `ResourceGrantsAuthorizer`. Claim to lista
`{resource_id, permission}`: `resource_id` renderowany przez
`resource_id_template` (domyślnie `urc-{id}`), wildcard `resource_wildcard`
(`urc-*`). Klient wymienia token przez RFC 8693 + RFC 8707 pod
`token_exchange_issuer`. Wymaga dodatkowo endpointu token-exchange w Swarogu.

### 2.4 Faza adaptera `UrcAuthApi` (jedyna droga dla dzisiejszego klienta)

Konfiguracja Lore: `[environment.endpoint] auth_url = "https://<swarog>/…"` plus
obowiązkowe `[server.auth] jwt_issuer/jwt_audience` (sam `auth_url` bez
`[server.auth]` to błąd startu, `repository_authorizer.rs:483-490`). Serwer weryfikuje
JWT lokalnie przez JWKS Swaroga **oraz** pyta Swaroga online.

Co Swaróg musi serwować (gRPC, pakiet `epic_urc`, `lore-proto/proto/auth_api.proto:5-36`,
plus `ucs.auth.RebacApi`):

| Wołający | RPC | Rola w Lore |
|---|---|---|
| klient | `StartAuthSession` → `{session_code, login_url}`; `GetAuthSession` (polling) | login, w praktyce device-flow. Klient polluje co 5 s × 30 (LEP:604-607). |
| klient | `ExchangeUserTokenForMultiresourceToken(resource_id[])` | token partycji z `resources: [{resource_id:"urc-{repo}", permission}]` |
| klient | `ExchangeExternalTokenForUserToken`, `ExchangeAPIKeyForUserToken`, `RefreshAuthSession` | login tokenem lub kluczem API (CI); refresh |
| klient | `GetUserInfo`, `GetUserId` | nazwy autorów i właścicieli locków (fallback: same id) |
| serwer (`AuthClientAuthorizer`) | `CheckUserPermission(resource_id[])` z przekazanym `Bearer` | **każde** sprawdzenie dostępu (`repository_authorizer.rs:249-281`) |
| serwer (`AuthClientRepositoryCatalog`) | `LookupUserPermissions` | `lore repo list` |
| serwer (repo create/delete) | `RebacApi.CreateResource/DeleteResource` | Swaróg dowiaduje się o nowych repozytoriach, co przy okazji daje inwentarz dla modułu `lore` |

`UserToken` to `{user_token, expires_at, user_id, user_name}` (`auth_api.proto:93-98`).
Nowe API katalogu użytkowników `lore.user.v1.UserService`
(`lore-proto/proto/lore/user/v1/user.proto`: `UserGet`, `UserFind`,
`PartitionList` stream) istnieje w protach. Nie zweryfikowano, czy dzisiejszy
klient już go woła.

**Odpowiedź na „które traity authnz implementuje Swaróg”:** żadnego w procesie.
Swaróg jest serwerem dla:

- `AuthClientAuthorizer` (przez `CheckUserPermission`) oraz `AuthClientRepositoryCatalog`
  (przez `LookupUserPermissions`) — w fazie adaptera;
- `GlobalGrantsAuthorizer` / `ResourceGrantsAuthorizer` — tylko emituje claimy, które
  te autoryzatory czytają; faza docelowa;
- klienckiego `UserService` (przez `GetUserInfo/GetUserId` albo `lore.user.v1`) oraz
  `RebacApiClient`.

## 3. Co z `accounts`, `apikeys`, `groups` da się wykorzystać

| Potrzeba | Werdykt | Kotwica |
|---|---|---|
| Rotacja refresh tokenów, rodziny, wykrywanie reuse, grace 30 s | reuse; dopisać kolumny `client_id`/`scope` dla grantów OAuth | `modules/accounts/src/store.rs:214,239,737,760,814` |
| Weryfikator JWKS (konsument) | reuse wzorca, ale nie podpisuje | `modules/accounts/src/oidc.rs:165-229` |
| Klucze ES256, tabela kluczy, rotacja, `kid`, publikacja JWKS | **dopisać** | brak `EncodingKey` w `modules/`; jedyny w repo to testowy IdP `tools/splitproof/src/idp.rs:39` (RS256, wzór kształtu `/jwks`) |
| Discovery, `/jwks`, `/token`, `/device_authorization`, strona `/device` | **dopisać** przez `ctx.mount` | precedens `epic_oauth::router`, `modules/accounts/src/lib.rs:1342` |
| Stan device grant (`device_code`, `user_code`, interval) | **dopisać**; wzorzec jednorazowego `DELETE … RETURNING` | `accounts.oauth_states`, `lib.rs:216` |
| Ludzkie logowanie na stronie zatwierdzania | rozszerzyć (P5) | Google: `modules/accounts/src/providers.rs:185` |
| `client_credentials` przez apikeys | rozszerzyć: `keys.name` jako `client_id`, nowa metoda lookup po name+secret, kolumny scope/owner | `modules/apikeys/src/lib.rs:67-93`, `api/apikeys/api/src/lib.rs:49` |
| Role z członkostwa w grupach | rozszerzyć: brak serwerowego „lista moich grup z rolami”; `role_of` wymaga znanego `group_id`; brak powiązania grupa ↔ repo | `api/groups/api/src/lib.rs:208` |
| Tenant/studio | **dopisać** (P3) | — |
| `sub`, nazwa, handle | reuse (`Directory::players_by_id`) | `api/accounts/api/src/lib.rs:137` |
| Unieważnienie przy usunięciu konta | reuse (`player.deleted` + kaskada sesji) | `api/accounts/events/src/lib.rs:84` |
| Zależności krypto | `jsonwebtoken 9` (ring, ES256 `EncodingKey::from_ec_der`) jest już w workspace; generowanie klucza przez `rcgen 0.13` albo nowy `p256`; **`aws-lc-rs` zakazany** | `Cargo.toml:244,293,277` |
| gRPC dla adaptera | **dopisać**: w workspace nie ma `tonic`/`prost` (jest tylko `axum 0.7`) | `Cargo.toml:228` |

Zależności modułów, bez cyklu: `groups` wymaga `accounts`
(`modules/groups/src/lib.rs:134`), więc `accounts` nie może wymagać `groups`.
Proponowany kształt: claimy ról składa **nowy moduł `lore`**, który `requires`
`accounts` i `groups`. `accounts` wystawia nową synchroniczną zdolność podpisywania,
np. `accountsapi::TokenIssuer::mint_access(sub, aud, claims)`, wire-only.
Klucze zostają w `accounts`, wiedza o repozytoriach i rolach w `lore`.

## 4. `loreserver` w modelu procesów i nadzoru Swaroga

Fakty o `loreserver`:

- **Binarka i konfiguracja.** Jedna binarka. CLI to tylko `--config <DIR>` i `--env`,
  oba mają odpowiedniki env: `LORE_CONFIG_PATH`, `LORE_ENV` (`lore-server/src/server.rs:149-164`).
  Konfiguracja to warstwy TOML: wbudowane `default.toml` → `<env>.toml` → `local.toml` →
  `LORE__A__B` (tylko skalary; tablice jak `jwt_audience` muszą być w pliku).
- **Porty.** QUIC UDP 41337, gRPC TCP 41337, HTTP 41339.
- **Health.** Jedynie `GET /health_check` na porcie HTTP; brak `/readyz`.
- **Zamykanie.** Unix: SIGINT/SIGTERM. Windows: tylko `ctrl_c`, więc `Child.kill()`
  to twarde zabicie (`server.rs:296-325`).
- **Storage.** Domyślnie `local` (pliki), bez bazy danych. S3/DynamoDB/Consul to
  wtyczki spoza stockowej binarki.
- **Repozytoria.** Tworzone w runtime przez gRPC `RepositoryService.RepositoryCreate`;
  w konfiguracji nie ma listy repozytoriów.

Dopasowanie do Swaroga:

- **Nadzór należy do Welesa, nie do modułu.** Moduł nie może spawnować procesów, bo
  moduły są topology-blind, a Weles to supervisor. Weles uruchomi prebuilt binarkę
  ze `deploy/<gen>/<pkg>`; nigdy nie buduje.
- **`processctl`/`devctl`/`splitproof` nie powinny hostować `loreserver`.**
  Wpis spoza `cmd/*-svc` w `FleetSpec` daje `DiskDrift`
  (`tools/processctl/src/fleet.rs:370,581-624`), a do tego dochodzi zakres
  **Dev tooling scope**. Moduł `lore` dowodzi split-proof względem fake'a gRPC Lore,
  tak jak `mail` wobec dostawcy SMTP.
- **Luki w Welesie,** do zapisania w `docs/reference/weles-design.md`, bo schemat
  jest `deny_unknown_fields`:
  - **health**: probe jest na sztywno `GET /readyz` na `http_port`
    (`weles/src/health.rs:28-56`). Potrzebne pole `health_path` (Lore: `/health_check`)
    albo shim;
  - **porty**: Weles wstrzykuje `PORT=":N"`, którego Lore nie czyta. Port HTTP Lore
    trzeba ustawić przez `LORE__SERVER__HTTP__PORT` w `env` usługi albo w pliku.
    Nazwa klucza nie zweryfikowana;
  - **argv**: brak pola `args`, ale `LORE_CONFIG_PATH` w `env` usługi wystarcza,
    więc **argv nie jest potrzebne**;
  - **zatrzymanie na Windows**: Lore nie ma drenażu przez wire. Do sprawdzenia, czym
    Weles zatrzymuje proces.
- **Konfigurację generuje moduł `lore`, a nie Weles.** Weles nie zna treści
  konfiguracji (zero-sharing). Do wyboru:
  - **(a) na teraz:** binarka `lorectl render-config --out <dir>` czyta stan modułu
    z DB i jest uruchamiana jako `[[prepare]]` Welesa przed bootem floty
    (`weles/master/src/fleet_toml.rs:84-101`);
  - **(b) docelowo:** moduł woła management API Welesa (zaplanowane w erracie
    2026-07-31, `weles-design.md:142-199`, niezbudowane) z artefaktem konfiguracji.
- **Ekspozycja.** Klienci Lore łączą się z `loreserver` bezpośrednio (QUIC/gRPC +
  własny TLS). Passthrough gateway to tylko HTTP/1, bez upgrade (`modules/gateway/src/proxy.rs:41-51`),
  więc nie przeniesie gRPC/QUIC. To samo dotyczy gRPC adaptera `UrcAuthApi` w Swarogu.
  Wymaga to decyzji transportowej w planie, np. tonic jako router axum na porcie HTTP
  modułu z HTTP/2. Do zweryfikowania, czy obecny serwer HTTP w `core/app` obsługuje h2.
  Nie wolno przy tym obchodzić gateway „na skróty” bez decyzji, bo gateway jest
  jedynym frontem.
- **Adres `loreserver` w module.** Precedens dla zewnętrznych systemów to
  `MailConfig::from_env` (`modules/mail/src/config.rs:97-109`): konfiguracja dostawcy,
  nie gałąź topologii.

## 5. Zdarzenia, locki, ochrona branchy, billing

- **Zdarzenia → bus.** Opcja stockowa (P2):
  - zadanie w `start()` modułu `lore` subskrybuje `lore.notification.NotificationService.Subscribe`
    per repo (`lore-server/src/grpc/notification_service.rs:60-112`);
  - zdarzenia to `BranchPushed`, `BranchCreated`, `BranchDeleted`, `ResourceLocked`,
    `ResourceUnlocked` (`lore-proto/proto/notification.proto:47-62`);
  - moduł zamienia je na trwałe tematy `lore.*` przez `emit_tx`, a `audit` i
    `notifications` dodają subskrypcje `audit.lore-*.v1` i `notifications.lore-*.v1`;
  - źródło jest at-most-once: broadcast w pamięci, lag gubi, restart serwera gubi.
    Bus za modułem pozostaje durable.
  - Hook pointy pochodnej binarki to `BranchPush`, `BranchCreate`, `BranchDelete`,
    `RepositoryCreate`, `Obliterate`. `pre_handler` ma synchroniczne weto z budżetem
    200 ms, `post_handler` jest fire-and-forget bez retry (`lore-server/src/hooks/dispatch.rs:86-175`).
- **Locki.** `urc.lock.LockService` (`lore-proto/proto/lock.proto`):
  - `Query` działa per repozytorium (repo z metadanych żądania); nie ma listy
    przekrojowej, więc widok „wszystkie locki studia” to pętla po repozytoriach;
  - `Unlock` pomija kontrolę właściciela dla `owner`/`admin` (`lore-server/src/grpc/lock_service.rs:170-179`).
    Swaróg mintuje sobie token serwisowy z tymi akcjami;
  - w trybie adaptera Swaróg sam odpowiada na `CheckUserPermission`, więc sam
    autoryzuje swoje wołania.
- **Ochrona branchy.**
  - Natywna flaga `PROTECT` w metadanych brancha, ustawiana przez RPC
    `BranchProtect/BranchUnprotect` (`lore-server/src/grpc/handlers/branch_protect.rs`).
  - Push na chroniony branch kończy się `permission_denied("protected")`
    (`branch_push.rs:298-312`). Obejście ma dziś tylko `is_service_account`, a po LEP
    będzie nim akcja `push-protected`.
  - Brak wzorców per ref. Nie prześledzono, kto może wołać `BranchProtect`
    (`grpc/tower/partition_access.rs`).
- **Billing.** W Swarogu brak kodu billing/metering (Grep, dolna granica; `wallet` to
  waluta wirtualna).
  - Seaty liczy moduł `lore` z własnych danych (członkowie studia).
  - TB: Lore ma brak API zajętości per repo, a monitor dysku jest per wolumen
    (`docs/reference/lore-server-config.md:107-118`). Przy instancji per studio TB to
    rozmiar `immutable_store.local.path` tej instancji, mierzony przez moduł lub agenta
    Welesa. Przy instancji współdzielonej pomiar wymaga własnej pracy (P4).

## 6. Portal admin

- **Wkład zakładek.** Nowe zakładki to `adminapi::Item` w `adminapi::SLOT`
  (`api/admin/api/src/lib.rs:29,389-417`):
  - zdalnie edytowalne w splicie przez `AdminSubmit` (`:83-87`);
  - menu wierszy przez `ExtensionPoint` (np. „Repozytoria” w menu gracza/studia).
- **Brak mockupu.** W `UILayout/GameOps Admin.dc.html` nie ma mockupu VCS, repozytoriów,
  locków ani billingu, a AGENTS.md kieruje UI przez skill `mockup-implementation`.
  **Krok do zbudowania:** najpierw mockup zakładek Lore, potem implementacja.

## 7. Inwentarz refaktoru `player_id → user_id`

Liczby to dolna granica (Grep + odczyty), bo typu `PlayerId` w Rust nie ma. Tożsamość
to wszędzie `String`/`&str`, a kolumny to `uuid`. Łącznie `player_id|PlayerId|playerId`
poza `experiments/` daje ≥1863 trafień w 157 plikach, ~60% w `docs/plans` i testach.
`experiments/` (≥183 w 61 plikach) zostaje nietknięte.

- **`core/`**:
  - `core/opsapi/src/lib.rs` (`Identity::player`, `Identity::player_id`, `AuthReq::Player`);
  - `core/opsapi/src/tests.rs`;
  - `core/edge/src/{client,lib,server}.rs`.
- **Makro i generatory**:
  - `tools/rpc-macro/src/lib.rs` (emituje `identity.player_id()` i
    `Identity::player`; `auth = "player"` → `AuthReq::Player`);
  - fixture'y `tools/rpc-macro/tests/compile_fail/*.stderr`;
  - `tools/rpc-macro-tests`.
- **Kontrakty `api/*`**:
  - pola `player_id` są w `accountsevents` (`PlayerRegistered/Promoted/Deleted`,
    tematy `player.registered|promoted|deleted`), `accountsapi` (sesja, guest, receipt,
    `me`, `PlayerSummary`, `Directory::players_by_id`, `PLAYERS_ROW_MENU`),
    `charactersevents`/`api`, `walletevents`/`api`, `groupsevents`/`api` (`role_of`),
    `partyapi`, `friendsapi`, `notificationsapi`, `ratingapi` (`mmr`);
  - osobna oś nazewnicza: `player_rpc` i makra `*_player_meta!`.
- **Baseline'y i golden**, wszystkie do re-bless:
  - `docs/reference/public-api-baseline/*.txt`;
  - `docs/reference/contract-golden/contracts.txt` (13 linii `player_id`, linie `auth=Player`);
  - `tools/conformance/input-fields.golden.tsv` (8);
  - `tools/csharp-client-gen/testdata/{manifest.golden.json,Dtos.golden.cs,Client.golden.cs}`.
- **DDL modułów** (edycja `CREATE` w miejscu + wipe, jeśli P1 = wipe):
  - `accounts`: tabela `players` oraz `player_id` w `handles`, `delete_tickets`,
    `deletion_receipts`, `identities`, `sessions`, `refresh_tokens`, plus indeksy
    `*_player_idx`;
  - `purged_players` w friends, wallet, characters, storage, inventory, groups,
    notifications;
  - `wallet.balances`/`ledger`, `characters.characters`, członkostwa `groups`, inbox
    `notifications`, `party.members`, `party.player_revisions`.
- **Kod modułów**: store/service/projection/admin/tests we wszystkich powyższych, plus
  `gateway` (`verifier.rs`, `push_ws.rs`), `match`, `rating`.
- **Id subskrypcji z „player”**:
  - `wallet.player-{registered,promoted,deleted}.v1`;
  - `{inventory,storage,groups,party,friends,characters,notifications}.player-deleted.v1`;
  - `notifications.player-promoted.v1`;
  - `audit.player-{registered,promoted,deleted}.v1`.
- **Narzędzia**:
  - `tools/splitproof/src/{main,party_checks,storage_checks,pushws}.rs` (asercje
    kluczy payloadu i id subskrypcji);
  - `tools/conformance/src/policy.rs`;
  - `tools/topiccheck`;
  - `tools/opscatalog-gen`;
  - `tools/rpc-contract-model`.
- **Klient C#**: generator `tools/csharp-client-gen` (`emit.rs:341,350`: `player_id` →
  `PlayerId`/`playerId`), wynik `clients/csharp/Generated/{Dtos,Client}.cs`, ręczny
  `clients/csharp/Program.cs:139,157`. Regeneracja:
  `cargo run -p csharp-client-gen -- --out clients/csharp/Generated`, pilnowana przez
  etap `codegen-freshness`.
- **Front demo**: `demos/webui/src/index.html:172` (`me.player_id`).
- **Docs bieżące**: `.agents/shared/gamebackend.md`, `docs/reference/*`,
  `docs/roadmap/feature-tracker.md`. Historyczne `docs/plans/*` zostają bez zmian
  (archiwa).
- **Do rozstrzygnięcia przy P1**: czy zakres obejmuje też nazwy tematów (`player.*`),
  `auth = "player"`, traity `Player`/`player_rpc` oraz `PLAYER_EDGE_ADDR`. Ten ostatni
  to inne „player”: płaszczyzna QUIC, nie id.

Refaktor jest **niezależny od Lore**. Lore potrzebuje tylko stabilnego `sub`, a ten
istnieje już dziś. Dlatego w kolejności jest ostatni.

## 8. Proponowana kolejność kroków

Zaczyna od 1 i 3, ale w kształcie wynikającym z ustalenia 2: dzisiejszy klient Lore loguje
się wyłącznie przez `UrcAuthApi`. Z punktu 1 briefu na start potrzebne są tylko
klucze, JWKS i discovery. Device grant, client credentials, refresh OIDC i `at+jwt`
z rolami (Tier 1) przydadzą się dopiero, gdy klient Lore dostanie fazę 3 LEP.
Każdy krok to osobny plan wg **Plan Writing Workflow**. Kroki modułowe idą przez skill
`add-game-module`, z dowodem w splicie.

1. **Weles: uruchamianie obcej binarki.** Pole health path, port przez `env`,
   decyzja o zatrzymaniu na Windows. Zapis w `weles-design.md`. Następnie wpis
   `loreserver` w `fleet.toml` z ręcznym katalogiem konfiguracji, bez auth: smoke
   push lokalnie przez Welesa.
2. **`accounts`: wystawca tokenów, minimum.** Tabela kluczy ES256 (`kid`, status,
   rotacja), `/.well-known/openid-configuration`, JWKS z `alg`, wire-only
   `TokenIssuer::mint_access` w `accountsapi`.
3. **Moduł `lore`, szkielet.** Forteca z `api/lore/*` i `cmd/lore-svc`. Obejmuje
   jedną instancję `loreserver` deploymentu, repozytoria, mapowanie grup na role i akcje, render konfiguracji
   (`lorectl render-config` jako `[[prepare]]`), fake Lore w split-proof.
4. **Adapter `UrcAuthApi` + `RebacApi` w module `lore`.** Nowa zależność `tonic`/`prost`
   i decyzja o transporcie gRPC (§4). Login w stylu device przez ludzkie logowanie
   (P5), token partycji z `resources: urc-{id}`, `CheckUserPermission`,
   `LookupUserPermissions`, `Create/DeleteResource`, `GetUserInfo/GetUserId`.
5. **Kamień milowy: logowanie i push na żywym serwerze.** `lore login` → push →
   `lore repo list`, wszystko na serwerze nadzorowanym przez Welesa i z
   uprawnieniami Swaroga.
6. **Zdarzenia.** Konsument `Subscribe` albo hook pochodnej binarki (P2), trwałe tematy
   `lore.*`, subskrypcje `audit` i `notifications`.
7. **Locki.** Widok przez pętlę `Query` po repozytoriach, zdejmowanie przez `Unlock`
   tokenem serwisowym. Ochrona branchy przez `BranchProtect/Unprotect`.
8. **Admin.** Najpierw mockup, potem zakładki Lore przez `mockup-implementation`.
9. **Billing.** Seaty z modułu `lore`; TB wg P4.
10. **OIDC pełne (Tier 2).** Device grant, refresh, `client_credentials` przez
    `apikeys`, `at+jwt`, token exchange RFC 8693 + RFC 8707 z claimem `resources`
    per repo. Włączane, gdy klient Lore ma fazy 3–4.
    W tym samym kroku znika adapter `UrcAuthApi`, bo reguła „nigdy dwa tory” zabrania
    trzymania obu. Przejście po stronie floty steruje się w Lore przez
    `auth_mode`/`oidc_preferred` (LEP:516-531).
11. **`player_id → user_id`.** Po decyzji P1.

## 9. Niezweryfikowane / otwarte

- Czy cały Lore buduje się i działa natywnie na Windows (sprawdzono tylko obsługę sygnałów).
- Mapowanie metod gRPC na akcje w `grpc/tower/partition_access.rs`, czyli kto może
  `BranchProtect`, `RepositoryCreate`, `Query`.
- Pełne zachowanie `AdminLock` dla niepustych zasobów.
- Czy dzisiejszy klient woła `lore.user.v1.UserService` czy wyłącznie
  `GetUserInfo/GetUserId` z `UrcAuthApi`.
- Dokładna nazwa klucza portu HTTP Lore dla nadpisania przez `LORE__…`.
- Czy serwer HTTP w `core/app` obsługuje HTTP/2 (potrzebne dla gRPC adaptera).
- Zachowanie `jsonwebtoken` w Lore, gdy token nie ma `aud`.
