# Plan: Fortress bez wyjątków (opcja B) + 3 poprawki jakości referencji inventory/characters

_Zatwierdzony 2026-07-14. Realizacja: Kroki 1–6 (Krok 0 = ten plik)._

## Context

Review modułów `inventory`/`characters` (oznaczanych jako *canonical reference*) dał 6 uwag.
Werdykt użytkownika: **fortress jest binarny — żadnych wyjątków**, w wariancie **B**: reguła obejmuje
zarówno import cudzego **impl crate** (`Kind::Module`) jak i cudzego **rpc glue** (`Kind::Rpc`), także
w `[dev-dependencies]`. Zakres uzgodniony: **#1, #2, #3+#4, #5** (pomijamy #6 skip-green — osobny
repo-wide temat).

Sedno #1 to **fix autorytetu, nie symptomu**: `archcheck` dziś pomija dev-deps
(`tools/archcheck/src/main.rs:242-244`), więc „bez wyjątków" jest komentarzem w Cargo.toml, nie
inwariantem. Autorytet musi zacząć tę klasę łapać.

### Errata do pierwotnego Contextu (mój błąd, nazwany)
Napisałem „dokładnie dwie krawędzie" — to było dolne oszacowanie z wadliwego sweepa (matchował
tylko deps o nazwie == katalog w `modules/`, więc z konstrukcji nie widział rpc-glue). Pełny,
poprawny census dev-depów module→(coś fortecowego):

| Krawędź | Cel | Klasa | Pod wariantem B |
|---|---|---|---|
| `inventory → config` | impl crate | `Kind::Module` | **usunąć** |
| `match → rating` | impl crate | `Kind::Module` | **usunąć** |
| `apikeys → adminrpc` | cudzy rpc glue | `Kind::Rpc` | **usunąć** |
| `audit → {characters,accounts,config,match,admin}events` | cudze events | `Kind::Events` | **legalne** (rule 3; events importowalne przez każdy moduł; archcheck nie ma ramienia Events — wpada w `_ => {}`) |

Trzy krawędzie do usunięcia, jedna klasa (`audit→*events`) świadomie legalna — to regresja, którą
Krok 4 musi chronić.

## Zależność kolejności (krytyczna)
Wszystkie trzy krawędzie muszą zniknąć (Kroki 1–3) **przed** zaostrzeniem reguły (Krok 4) — inaczej
blokujący fortress-stage (`cargo run -q -p archcheck`, `tools/verifyctl/src/stages/fortress.rs:35-49`)
zaświeci się na czerwono. Kroki 1/2/3 to samowystarczalne, atomowe edycje bez okna nie-budującego się
drzewa (dev-deps nie wpływają na build `server`/`*-svc` ani na requirecheck/topiccheck). Kroki 5–6
niezależne.

---

## Krok 1 — `match`: fake `MmrReader` + fake `Module`, usuń dev-dep `rating` `[opus]`

**(a) Co:** `modules/match/src/tests.rs` — helper `wired()` (61–76, wołany z 206/236/273/305) ORAZ
pozytywna noga `validate_requires` w `match_requires_rating_and_fails_validate_without_it` (linia
**491**, `Box::new(Rating::new())`); `modules/match/Cargo.toml:35-38` (usuń `rating`).

**(b) Dlaczego teraz:** jedna z trzech krawędzi, musi paść przed Krokiem 4.

**(c) Jak — dwie różne podmiany (dlatego `[opus]`, nie mechaniczne):**
1. `wired()` × 4: `CountingReader` (fake `impl MmrReader`, **już istnieje** `104-125`, wpinany
   przez `service_with_reader` `78-102`) zwracający `Ok(1000)` (parytet z domyślnym MMR nieznanego
   gracza). Zachować resztę `wired()` (durable plane/transport) — jedyna zmiana to źródło
   `dyn MmrReader`. Wartość MMR i tak nigdy nie jest asertowana (doc 199–202: dowód *seam wiring*).
2. Linia 491: `validate_requires` dopasowuje **po nazwie modułu** (`core/app/src/lib.rs:560`:
   `modules.map(|m| m.name())`), więc wystarczy lokalny **name-only stub** `impl lifecycle::Module`
   z `name()=="rating"` (wzorzec: `Fake::boxed("characters", &[])`, `core/app/src/tests.rs:199`;
   `CountingReader` to `Arc<dyn MmrReader>`, NIE `Box<dyn Module>` — nie zastąpi tej nogi).
3. Po usunięciu ostatniego `rating::` — skreślić dev-dep.

**(d) Dispatch:** `[opus]` — poprawność `validate_requires` + zrozumienie `wired()`.

---

## Krok 2 — `inventory`: fake `configapi::Config`, usuń dev-depy `config`+`invalidation` `[opus]`

**(a) Co:** `modules/inventory/src/tests.rs` — test
`grant_starter_reflects_config_after_invalidation_refresh` (fn 502–591; `config::` tylko w 519,
`invalidation::` tylko w 513); `modules/inventory/Cargo.toml:37-44` (usuń `config` **i**
`invalidation` — po refaktorze OBA bezużyteczne, potwierdzone; `asyncevents` zostaje, to core).

**(b) Dlaczego teraz:** druga krawędź, przed Krokiem 4.

**(c) Jak — osąd o pokryciu:** dzisiejszy test dowodzi pełnej ścieżki live-reload (raw SQL → trigger
`pg_notify` → `InvalidationPlane` → refresh → następny grant). Ta ścieżka cross-process jest **już**
asertowana w splitproof **[C1]–[C4b]** (`tools/splitproof/src/main.rs:1029-1058`:
`starter_sword`→`health_potion`→revert, rev-bump, >8KB, reset). Jedyne *inventory-local* pokrycie
tego unit-testu to: `init` rozwiązuje `dyn Config` przez `require::<dyn Config>()` i
`grant_starter`/`starter_spec` (`projection.rs:61-89`) z niego czyta — to fake zachowuje.
Przebudować na wzorzec `FakeConfig` (characters `tests.rs:11-31`: impl
`get_string/get_bool/get_int/get`): dostarczyć fake pod `key("config","reader")` przed `init`;
asertować `STARTER_ITEM` gdy fake zwraca default i skonfigurowany item gdy zwraca np.
`health_potion`. Usunąć maszynerię NOTIFY/invalidation. **Komentarz w teście:** live-reload e2e
asertowany w splitproof [C1]–[C4b] (żeby nie wyglądało na cichą utratę pokrycia — reguła „scope
claims").

**(d) Dispatch:** `[opus]` — decyzja co zachować/oddać.

---

## Krok 3 — `apikeys`: usuń dev-dep `adminrpc` (skutek wariantu B) `[opus]`

**(a) Co:** `modules/apikeys/src/admin_tests.rs` — test `edge_serves_admin_data` (**133–166**,
jedyny użytkownik `adminrpc`, linia 155); `modules/apikeys/Cargo.toml:30-33` (usuń `adminrpc`).

**(b) Dlaczego:** wariant B zakazuje `module → cudzy rpc glue` też w dev-depach. `edge_serves_admin_data`
dia‌luje własną admin-edge-face apikeys generowanym `adminrpc::admin_data_rpc::Client` — a ten glue
z definicji żyje w `adminrpc`, apikeys nie może go sam wygenerować, więc test musi odejść z crate'a.

**(c) Jak:** usunąć `edge_serves_admin_data`. Jego pokrycie (admin-face zarejestrowana + osiągalna po
edge, „guards the Step 6 regression where the admin face silently went unregistered") jest asertowane
**end-to-end cross-process** w splitproof **[AD3b]** (`main.rs:1315-1318`:
`GET /admin/api-keys` przez gateway → admin-svc → apikeys-svc po QUIC, „two hops", 200 + `dev-client`).
Pozostałe testy w pliku (`render_shows_rows_kpis_and_fields`, `submit_*`, atomowość) **zostają** —
wołają `admin_content_full`/`apply_edit` bezpośrednio, bez `adminrpc`; render danych admina jest
nadal unit-pokryty. Usunąć dev-dep.
**Świadomy koszt (zapis w planie + errata):** tracimy in-crate szybkopętlowy guard „admin-face
zarejestrowana po edge"; end-to-end pokrywa [AD3b]. To bezpośrednia cena wariantu B, nie ukryta.

**(d) Dispatch:** `[opus]` — decyzja o utracie pokrycia + zapis erraty.

---

## Krok 4 — Zaostrz `archcheck`: dev-dep module→(module|rpc) = FAIL `[opus / core-implementer]`

**(a) Co:** `tools/archcheck/src/main.rs` — pętla reguły 1/2 (234–256); `tools/archcheck/src/tests.rs`
— testy gałęzi.

**(b) Dlaczego teraz:** fix autorytetu; musi wejść **po** Krokach 1–3.

**(c) Jak:** skreślić `if dep["kind"].as_str() == Some("dev") { continue; }` (242–244) z **tej jednej**
pętli. Skutek: ramiona `Some(Kind::Module(other)) if other != dm` **oraz**
`Some(Kind::Rpc(domain)) if domain != dm` odpalają dla dowolnego rodzaju zależności (wariant B —
oba). Cele `Kind::Core`/`Kind::Api`/`Kind::Events` nadal wpadają w `_ => {}` (253), więc
`module→core` (np. `asyncevents`/`invalidation`/`app`) **i** `module→events` (audit→*events) dev-depy
zostają legalne — bez osobnej allow-listy. **Nie ruszać** innych dev-skipów w pliku (reguły
3/5/6/10/13/16 mają własne uzasadnienia — minimal-sufficient-closure). Poprawić komentarz 240–241
(dziś „Only normal/build deps carry the runtime import graph" — już nieprawda) i nagłówkowy opis
reguły fortress.
**Prove the failing branch** w `tests.rs` (wzorzec fikstur: `tests.rs:374`) — cztery testy:
(1) module→module DEV → **violation**; (2) module→foreign-rpc DEV → **violation**;
(3) module→core DEV → **pass** (regresja: `asyncevents`/`invalidation`); (4) module→events DEV →
**pass** (regresja: `audit→*events`).

**(d) Dispatch:** `[opus]` przez **core-implementer** — autorytet seamu fortress; agent wymusza
nazwanie autorytetu + dowód gałęzi.

---

## Krok 5 — Negatywny dowód atomowości emit_tx (characters), przez failing transport `[opus / core-implementer]`

**(a) Co:** `core/asyncevents` (moduł `testing`) — dodać minimalny **failing transport**;
`modules/characters/src/tests.rs` — nowy negatywny test obok
`create_persists_character_and_durable_event_atomically` (238–260).

**(b) Dlaczego:** #5 — istniejący „atomic emit proof" dowodzi tylko sukcesu. Reguła „Prove the failing
branch": bez wymuszenia awarii append'u i sprawdzenia rollbacku wiersza domenowego to pół dowodu.

**(c) Jak — NIE przez zatrucie współdzielonego kontraktu:** pierwotny pomysł (pre-seed konfliktowego
`asyncevents.history_contracts` na `character.created`) jest wadliwy — `create` używa **stałego,
współdzielonego** topicu, więc konfliktowy wiersz zatruwa równoległe sibling-testy (`store_tests.rs`
robi to bezpiecznie tylko dzięki `unique_topic`). Zamiast tego wstrzyknąć błąd **na poziomie
transportu**: dodać do `asyncevents::testing` handle, którego `enqueue_tx` zwraca `Err` (globalnie
albo dla wskazanego topicu) — zero dotykania współdzielonego stanu DB, reużywalne. Wstrzyknąć przez
`Context::with_db_and_transport` (jak `transport()` w `wired_with_cap`, `characters/tests.rs:172-173`).
Test: `svc.create(...)` → asertować `Err` **oraz** `char_count_by_player(&pool,&pid) == 0` (wiersz
domenowy wycofany z padłym append'em). Ścieżka rollbacku: `create`
(`modules/characters/src/lib.rs:290-365`) na błędzie `emit_tx` (362) robi early-return, `tx` drop bez
`commit()` → implicit ROLLBACK cofa INSERT z 350. Istniejące helpery: `unique_player` (185–192),
`cleanup` (194–202), `ensure_schema`.
**Świadome ograniczenie zakresu (komentarz + plan):** inventory **nie** dostaje takiego testu — nie
emituje własnych eventów (konsument `on_tx` grant_starter/wipe_character; brak `api/inventory/events`).
Emitter-side atomicity go nie dotyczy; to inny kształt (handler-write + checkpoint), poza #5.

**(d) Dispatch:** `[opus]` przez **core-implementer** — dokładka do `asyncevents::testing` (core) +
korektność na seamie durable plane.

---

## Krok 6 — Doc/komentarze: de-canonize grant + koszt tombstone + spójna paginacja `[sonnet]`

**(a)/(c):**
- **#2 grant:** `modules/inventory/src/service.rs` (metoda `grant`, 84–127). Komentarz-ostrzeżenie: to
  **nie** referencyjny wzorzec mutacji — dev-only (`INVENTORY_DEV_GRANT`), trzy osobne autocommity
  (`item_exists` → `grant_pool` → `list`, każde po własnej connection z puli — potwierdzone w
  `store.rs:107-110,162-165`), brak idempotency key: po zacommitowanym `grant_pool` końcowy `list`
  może paść → klient dostaje błąd mimo mutacji; ręczne ponowienie → podwójny grant
  (`ON CONFLICT ... quantity + EXCLUDED`, `store.rs:93-94`). Bez zmiany zachowania.
- **#4 tombstone:** `modules/inventory/src/projection.rs` (78–91, zwł. „UUIDs never recur" w 85; oraz
  152–155). Jawny **koszt**: `wiped_characters` rośnie monotonicznie, 1 wiersz/usuniętą postać, bez
  GC/watermark; OK w fazie „wipe-is-migration", dla długowieczności wymaga retencji.
- **#3 paginacja:** `modules/characters/src/lib.rs` (`LIST_HARD_LIMIT`, 36–41) — ujednolicić frazowanie
  do „KNOWN GAP" jak `modules/inventory/src/store.rs:6-10`. Bez zmiany kodu; brak `cursor/has_more`
  pozostaje świadomym safety-beltem.

**(d) Dispatch:** `[sonnet]` — czyste edycje komentarzy, zero logiki.

---

## Weryfikacja (respektując „one rollout at a time" — kolejno, pre-flight: brak aktywnego `cargo`/`rustc`, `devctl status` czysty)
1. `cargo test -p archcheck` — 4 testy gałęzi (module→module DEV = FAIL, module→rpc DEV = FAIL,
   module→core DEV = PASS, module→events DEV = PASS).
2. `cargo run -q -p archcheck` — musi przejść (trzy krawędzie usunięte + reguła zaostrzona).
3. `cargo test -p match` — 4 przełożone `wired()` + naprawiona noga `validate_requires`, bez dev-dep.
4. `cargo test -p apikeys` — pozostałe admin_tests zielone bez `adminrpc`.
5. `cargo test -p characters` — pozytywny + **nowy negatywny** test atomowości (live PG).
6. `cargo test -p inventory` — przebudowany test grant/config na fake.
7. Domknięcie autorytetu: `cargo run -p verifyctl -- --fast` (blokujący fortress-stage) — jeden
   rollout.

## Lanes + review (do zatwierdzenia z planem)
- Krok 1 `[opus]` · 2 `[opus]` · 3 `[opus]` · 4 `[opus/core-implementer]` · 5 `[opus/core-implementer]`
  · 6 `[sonnet]`.
- Po rolloutcie: audyt trailerów (`git log -N --format="%h %B" | grep Co-Authored`): `[opus]`→Opus 4.8,
  `[sonnet]`→Sonnet 4.6.
- Adversarial review: **jeden** pass `core-reviewer` na diffy (routowany po plikach; ≥ tier
  implementera) + **`proof-auditor`** na Krok 4 (zmiana gate'u/checkera) i Krok 5 (test JEST powierzchnią
  ryzyka — czy failing transport realnie trafia w gałąź rollbacku, nie obok).
