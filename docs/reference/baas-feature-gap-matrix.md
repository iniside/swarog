> **ARCHIWALNE (Go-era, 2026-07-07)** — dokument opisuje port Go sprzed migracji do Rust; struktura (m.in. `outbox/`) jest nieaktualna. Stan bieżący: CLAUDE.md.

# BaaS Feature Gap Matrix — co mamy vs katalog PlayFab/AccelByte

**Data:** 2026-07-07 · **Cel:** uporządkować, które „prawdziwe" ficzery backendu gry
warto dodać, zanim cokolwiek zakodujemy. Trzy źródła: inwentarz naszych trzech seamów
+ modułów, audyt luk platform/infra, taksonomia katalogu BaaS (PlayFab, AccelByte,
Nakama, Unity Gaming Services, GameLift, Epic Online Services).

> **To jest dokument badawczy, nie plan.** Kolejność ficzerów w sekcji „Rekomendacja"
> to punkt startowy do rozmowy, nie zatwierdzona sekwencja. Plan pod konkretny ficzer
> powstaje osobno wg *Plan Writing Workflow*.

---

## 0. Sprostowanie: CLAUDE.md jest miejscami nieaktualny

Inwentaryzacja kodu wykazała rozjazdy między CLAUDE.md a repo — do naprawienia osobno,
ale trzeba je znać czytając ten dokument:

- Pakiet **`core` nie istnieje** jako katalog — rola rozbita na `lifecycle`, `registry`,
  `bus`, `contrib`. Interfejs to `lifecycle.Module`.
- **`DependsOn` → `Requires()`** i **nie ma topo-sortu** — moduły startują w kolejności
  rejestracji; dwufazowy `Build` (Register wszystkich → Init wszystkich) gwarantuje, że
  `Provide` poprzedza `Require`. Spójność zestawu pilnuje `validateRequires`
  (`internal/app/app.go:193`).
- **leaderboard/inventory NIE są już „szkieletami"** — obie to pełne implementacje;
  ich „szkieletowość" to *wąski zakres funkcjonalny*, nie brak kodu.
- Moduł **`config`** (DB-backed, LISTEN/NOTIFY, live-reload) jest już **zmergeowany**;
  gałąź `feat/config-split` nie istnieje.

---

## 1. Co już mamy (capability × dojrzałość × seam)

Trzy seamy w kodzie: **module registry** (`lifecycle`), **service registry**
(`registry.Provide/Require`, + `TryRequire`), **event bus** (`bus.Define/Emit/On`),
plus slot **`contrib`** (`Contribute/Contributions`). Transport split-deploy: `edge/`
(QUIC RPC), `gateway/` (reverse proxy), `outbox/` (transakcyjny relay).

| Capability | Dojrzałość | Moduł / seam | Realna luka funkcjonalna |
|---|---|---|---|
| Identity: dev/password + Epic OIDC + OAuth web-flow | **Pełne** | `accounts`, Provide `"accounts"` | brak refresh/logout/MFA/rate-limit — patrz §3A |
| Sesje (opaque, DB-backed) | **Pełne** | `accounts`, edge `verifySession` | stały 30-dniowy TTL, brak revoke |
| Characters (N/gracza, outbox) | **Pełne** | `characters`, events Created/Deleted | wzorzec integralności bez FK — referencja |
| Inventory (owner-scoped, event-driven) | **Pełne** | `inventory`, Provide `"inventory"` | jeden typ itemu, brak stack/limit/ekonomii/trade |
| Match report → event | **Pełne (demo)** | `match`, emit `match.finished` | `MatchID` nigdy nie ustawiony; brak matchmakingu |
| Rating / MMR | **Celowo in-memory** | `rating`, Provide `"rating"` | zero persystencji, brak mutexa — nie skaluje się |
| Leaderboard (top-100 wins) | **Pełne (wąskie)** | `leaderboard`, Postgres | jedna globalna tabela; brak wielu tablic/sezonów/paginacji |
| Config / operational knobs | **Pełne** | `config`, Provide `"config"` | adopcja tylko w `inventory`; brak segmentacji/A-B/wersji |
| Admin portal (GameOps) | **Pełne (wąskie)** | `admin`, slot `admin.item` | brak RBAC ról/audytu akcji/multi-tenancy |
| WebUI demo SPA | **Celowo trywialne** | `webui` | — |
| Transport split (edge/gateway/outbox) | **Pełne, przetestowane** | pakiety infra | `remote.Stub` hardcoded 2 nazwy (`default: panic`) |

---

## 2. Taksonomia katalogu BaaS (skrót)

Pełne tabele + źródła w transkryptach researchu. Legenda: PF=PlayFab, AB=AccelByte,
NK=Nakama, UGS=Unity, GL=GameLift, EOS=Epic.

- **A. Platform/infra:** fleet hosting + autoscaling (GL flagowo), rate-limit/DDoS
  (infra-level), observability/analytics (PF PlayStream, UGS), multi-region, auth-at-scale
  (guest/platform identity, linking, refresh, MFA, ban/entitlement), CDN (UGS CCD).
- **B. LiveOps/gameplay:** matchmaking skill/latency (GL FlexMatch, AB, NK), leaderboards
  &amp; stats, player data / cloud save (UGS, EOS), inventory &amp; entities, quests/achievements,
  party/lobby/friends/social (NK flagowo), cloud scripting (PF CloudScript, UGS Cloud Code,
  NK runtime), player profiles.
- **C. Trust &amp; safety/ops:** anti-cheat (EOS/EAC flagowo), ban/mute/moderation, chat
  moderation, audit logs, feature flags / remote config / title data (UGS, PF), A/B testing
  (PF flagowo), segmentation (PF), scheduled tasks/cron (PF), notifications/push.
- **D. Monetyzacja/commerce:** catalog/store, virtual currency &amp; economy, entitlements/DLC,
  receipt validation (Steam/Epic/mobile), battle/season pass (AB flagowo), promotions/coupons.

**Table-stakes** (commodity, wszyscy mają, różni tylko API): auth+linking, leaderboards,
player data/cloud save, inventory+ekonomia, cloud scripting, matchmaking skill-based,
podstawowy store + purchase validation.

**Premium differentiator** (za to się płaci): natywny anti-cheat (tylko EOS), managed
fleet+autoscaling (GL), **A/B + segmentacja + scheduled tasks napędzane segmentami** (PF —
to jest LiveOps tooling, nie mechanika), gotowy Season Pass (AB), in-process server runtime
(NK), chat/social jako first-class model (NK), CDN (UGS).

---

## 3. Gap matrix per obszar — werdykt + jak byśmy to dodali

Kluczowe: dla każdej luki podajemy **którym z trzech seamów** to się dokłada (bo repo jest
Open/Closed — ficzer = nowy `lifecycle.Module`, nie edycja istniejących).

### A. Platform / infra

| Ficzer | Stan u nas | Table-stakes? | Jak dodać (seam) |
|---|---|---|---|
| Rate-limiting / throttling | **BRAK** (0 w repo) | tak (podstawa) | HTTP middleware w `internal/app` lub `gateway/` — **nie moduł**, to warstwa transportu |
| Metrics (Prometheus/OTel) | **BRAK** | tak | nowy `observability` moduł mountujący `/metrics` + middleware; lub cross-cutting w `internal/app` |
| Tracing (OTel) | **BRAK** | pół-premium | j.w. + propagacja trace-id w edge/bus |
| Structured logging | **Pełne** (`slog` wszędzie) | — | dodać request/trace-id do korelacji |
| Health / readiness | **STUB** (tylko `/healthz`, niespójny między procesami) | tak | rozdzielić `/readyz` (bus/outbox/edge) od `/healthz` |
| Multi-region: rating in-memory | **STUB — bloker** | — | przenieść na Postgres (jak leaderboard) — edycja `rating` |
| Multi-region: epic OAuth state in-memory | **STUB — bloker LB** | — | state store w DB/Redis zamiast `map` per-proces |
| Multi-region: cross-proces event bus | **STUB** (bus lokalny-w-procesie) | premium | outbox już istnieje jako most; brak brokera |
| Leader election / distributed locks | **BRAK** | premium | `pg_advisory_lock` gdy pojawi się „jeden worker" |
| Auth-at-scale | patrz niżej | tak | — |

**Auth-at-scale (rozbicie §4 audytu):** fundamenty kryptograficzne **pełne** (argon2id
OWASP, constant-time compare, OIDC z whitelistą algorytmów, anti-CSRF state). Brakuje
warstwy „at scale": **refresh tokens (BRAK), logout/revoke (BRAK), MFA (BRAK), rate-limit
na login (BRAK), konfigurowalny TTL sesji** (stały 30 dni, `store.go:16`). Wszystko to =
rozbudowa modułu `accounts` (edycja, nie nowy moduł — to jego domena).

### B. LiveOps / gameplay services

| Ficzer | Stan u nas | Table-stakes? | Jak dodać |
|---|---|---|---|
| Matchmaking (skill-based) | **BRAK** (mamy tylko report wyniku) | tak | nowy `matchmaking` moduł; Require `"rating"` (MMR już jest!) |
| Leaderboards wielo-tablicowe / sezonowe | **STUB** (jedna tabela wins) | tak | rozbudowa `leaderboard` lub nowy `stats` |
| Player data / cloud save | **BRAK** | tak | nowy `playerdata` moduł (owns schema, klucz=player_id) |
| Quests / achievements | **BRAK** | tak | nowy `achievements` moduł; subskrybuje `match.finished`, `characters.*` |
| Party / lobby / friends / social | **BRAK** | zależy od gry | nowy `social` moduł (największy) |
| Cloud scripting | **BRAK** | premium | prawdopodobnie poza zakresem demo |

### C. Trust &amp; safety / ops — **największa luka względem katalogu**

| Ficzer | Stan u nas | Za to się płaci? | Jak dodać |
|---|---|---|---|
| Feature flags / remote config / title data | **Częściowo** (config mechanizm pełny, ale bez targetingu/segmentacji) | tak (PF flagowo) | rozbudowa `config` o scoping/segmenty |
| Segmentation | **BRAK** | **tak (PF flagowo)** | nowy `segments` moduł czytający player properties |
| A/B testing | **BRAK** | **tak (PF flagowo)** | na bazie segmentacji |
| Scheduled tasks / cron | **BRAK** | tak | nowy `scheduler` moduł (Starter z tickerem + `pg_advisory_lock`) |
| Audit logs | **BRAK** | tak | nowy `audit` moduł subskrybujący bus (nasłuch cross-cutting) |
| Ban / mute / moderation | **BRAK** | tak | rozbudowa `accounts` (ban) + nowy `moderation` |
| Notifications / push | **BRAK** | tak | nowy `notifications` moduł |
| Anti-cheat | **BRAK** | premium (tylko EOS ma natywnie) | integracja, nie własny silnik |

### D. Monetyzacja / commerce

| Ficzer | Stan u nas | Table-stakes? | Jak dodać |
|---|---|---|---|
| Catalog / store | **BRAK** | tak | nowy `store` moduł; Require `"inventory"` do grantu |
| Virtual currency / economy | **Częściowo** (inventory ma itemy, brak walut) | tak | rozbudowa `inventory` lub nowy `economy` |
| Entitlements / DLC | **BRAK** | tak | nowy `entitlements`; Require `"accounts"` |
| Receipt validation (Steam/Epic/mobile) | **BRAK** | tak | nowy `payments` moduł (weryfikatory jak OIDC w accounts) |
| Battle / season pass | **BRAK** | premium (AB flagowo) | nowy `seasonpass`; subskrybuje eventy postępu |
| Promotions / coupons | **BRAK** | pół | część `store` |

---

## 4. Rekomendacja — kandydaci na kolejne ficzery (do rozmowy)

Uszeregowane wg **stosunku wartości-demonstracyjnej do kosztu** i tego, jak dobrze pokazują
seamy Open/Closed. Każdy to *kandydat*, nie zatwierdzony plan.

**Tier 1 — najlepiej pokazują architekturę, niski koszt, wypełniają realną lukę:**

1. **Rate-limiting** (transport middleware, nie moduł) — user wprost wskazał; „table-stakes",
   0 w repo, izolowany do `internal/app`/`gateway`. Pokazuje warstwę cross-cutting bez modułu.
   *Why not extend:* nic do rozszerzenia — to nowa warstwa transportu.
2. **Observability: `/metrics` + `/readyz`** — user wskazał; nowy `observability` moduł +
   middleware. Pokazuje moduł czysto-infrastrukturalny (jak `webui`/`admin` — bez schematu).
   *Why not extend `admin`:* admin to UI portal, nie zbiór metryk Prometheus.
3. **Scheduled tasks / cron** (`scheduler` moduł) — odblokowuje leaderboard resets, XP boosty,
   season rollover. Pokazuje `Starter` + `pg_advisory_lock` (rozwiązuje przy okazji brak
   leader-election). *Why not extend:* żaden moduł nie ma pętli czasowej poza `config` LISTEN.
4. **Audit log** (`audit` moduł) — subskrybuje bus, zero zależności od modułów, pojawia się
   w `/admin` przez slot. **Modelowy przykład** cross-cutting listenera + slotu.
   *Why not extend `admin`:* admin renderuje, nie przechowuje historii zdarzeń.

**Tier 2 — większa wartość, większy koszt, rdzeń „prawdziwego" backendu gry:**

5. **Matchmaking** (`matchmaking` moduł) — Require `"rating"` (MMR już mamy!), emituje event
   dla `match`. Domyka pętlę gameplay: matchmake → play → report → rating → leaderboard.
   *Why not extend `match`:* match to sink wyniku; matchmaking to osobna domena (kolejka, MMR).
6. **Auth-at-scale w `accounts`** (refresh + logout/revoke + login rate-limit + config TTL) —
   rozbudowa istniejącego modułu; MFA osobno. User wskazał wprost. *Rozszerzenie, nie nowy moduł.*
7. **Player data / cloud save** (`playerdata` moduł) — czyste table-stakes, prosty schemat
   (player_id → blob/kv), pokazuje kolejny moduł z własnym schematem i edge RPC.

**Tier 3 — premium differentiator, duży zakres (osobna rozmowa):**

8. **Segmentation + feature-flag targeting** (rozbudowa `config` + `segments`) — to za to płaci
   się w PlayFab. Duży, ale najbardziej „premium".
9. **Store / economy / entitlements** — cała pod-domena commerce; wiele modułów.
10. **Social / party / lobby** — największy zakres; sensowny tylko jeśli gra tego wymaga.

**Poza radarem (integracja, nie własny kod):** anti-cheat (EAC), managed fleet (GameLift),
CDN — to produkty infrastrukturalne, nie ficzery aplikacyjne.

---

## 5. Wnioski jednym akapitem

Baza jest solidna: trzy seamy + slot + transport split działają i są przetestowane, a
`accounts`/`characters`/`inventory` pokrywają table-stakes na poziomie „istnieje". Największa
luka względem PlayFab/AccelByte to **obszar C (Trust &amp; safety / LiveOps ops)** — scheduled
tasks, audit, segmentacja, feature-flag targeting — czyli dokładnie „to, za co się płaci".
Drugorzędnie: pętla gameplay jest niedomknięta (brak matchmakingu) i auth nie ma warstwy
„at scale" (refresh/logout/MFA/rate-limit). Najlepszy pierwszy krok to Tier 1 (rate-limit,
observability, scheduler, audit) — niski koszt, wysoka wartość demonstracyjna, i każdy
pokazuje inny wariant użycia seamów.
